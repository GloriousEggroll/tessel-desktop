//! `lock-screen/voice_state.py`: read Kasumi's existing voice lifecycle from
//! its log, without touching its microphone.
//!
//! The log is untrusted text: lines are decoded lossily (`errors="replace"`),
//! NULs are dropped, reads are bounded to 256 KiB per poll and only fixed
//! substrings are matched — nothing from the log is executed or displayed.
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Instant;

const CHUNK: u64 = 262_144;
const EXPIRE_SECONDS: f64 = 180.0;

const IDLE_MARKERS: [&str; 10] = [
    "Voice stop: listener released", "Voice stop: keyword detected",
    "Recording too quiet", "No speech within", "CLI cleanup calling",
    "Voice recording cancelled", "Voice transcription failed", "Voice notification: finished",
    "Voice capture: idle", "Voice wake: ready",
];

/// Seconds on a monotonic clock (`time.monotonic()`).
pub fn monotonic() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    // Offset so that "now - 0 > 180" never holds right after start-up.
    START.get_or_init(Instant::now).elapsed().as_secs_f64() + 1_000_000.0
}

/// `VoiceState`: "idle" | "listening" | "thinking" | "speaking".
#[derive(Clone, Debug)]
pub struct VoiceState {
    pub state: &'static str,
    pub changed_at: f64,
    pub last_activity: f64,
}

impl Default for VoiceState {
    fn default() -> Self { Self::new() }
}

impl VoiceState {
    pub fn new() -> Self {
        let now = monotonic();
        Self { state: "idle", changed_at: now, last_activity: now }
    }

    /// `feed(line, now=None)`.
    pub fn feed(&mut self, line: &str, now: Option<f64>) -> &'static str {
        let now = now.unwrap_or_else(monotonic);
        let active = self.state != "idle";
        let state = if IDLE_MARKERS.iter().any(|text| line.contains(text)) {
            Some("idle")
        } else if line.contains("Voice notification: speaking") {
            Some("speaking")
        } else if line.contains("Voice recording started") {
            Some("listening")
        } else if line.contains("Voice follow-up: opening one reply window") || line.contains("Voice capture: awaiting command") {
            Some("listening")
        } else if line.contains("Voice recording stopped") && active {
            Some("thinking")
        } else if active {
            if line.contains("TTS audio saved:") {
                Some("speaking")
            } else if line.contains("Voice stop: listening during wake reply") {
                // The stop detector can finish warming after speech has begun.
                Some(if self.state == "speaking" { "speaking" } else { "thinking" })
            } else if line.contains("Generating speech with KittenTTS") {
                // During streaming, another sentence may already be playing.
                Some(self.state)
            } else if line.contains("conversation turn:") && line.contains("platform=cli") {
                Some("thinking")
            } else if line.contains("API call #") || (line.contains("tool ") && line.contains(" completed (")) {
                Some(self.state)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(state) = state {
            self.last_activity = now;
            if self.state != state {
                self.state = state;
                self.changed_at = now;
            }
        }
        self.state
    }

    /// Recover from a killed voice process or a missed/truncated completion.
    pub fn expire(&mut self, now: Option<f64>) -> &'static str {
        let now = now.unwrap_or_else(monotonic);
        if self.state != "idle" && now - self.last_activity > EXPIRE_SECONDS {
            self.state = "idle";
            self.changed_at = now;
        }
        self.state
    }
}

/// Seconds since the epoch of a naive local `"%Y-%m-%d %H:%M:%S"` stamp.
pub fn parse_local_stamp(text: &str) -> Option<f64> {
    let prefix: String = text.chars().take(19).collect();
    let b = prefix.as_bytes();
    if b.len() != 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b' ' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let field = |range: std::ops::Range<usize>| -> Option<i32> {
        let s = &prefix[range];
        s.bytes().all(|c| c.is_ascii_digit()).then(|| s.parse().ok()).flatten()
    };
    let (year, month, day) = (field(0..4)?, field(5..7)?, field(8..10)?);
    let (hour, minute, second) = (field(11..13)?, field(14..16)?, field(17..19)?);
    let days = [31, if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if year < 1 || !(1..=12).contains(&month) || day < 1 || day > days[month as usize - 1]
        || hour > 23 || minute > 59 || second > 61 {
        return None;
    }
    // SAFETY: tm is fully initialised; mktime only reads/normalises it.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        tm.tm_year = year - 1900;
        tm.tm_mon = month - 1;
        tm.tm_mday = day;
        tm.tm_hour = hour;
        tm.tm_min = minute;
        tm.tm_sec = second;
        tm.tm_isdst = -1;
        let stamp = libc::mktime(&mut tm);
        (stamp != -1).then_some(stamp as f64)
    }
}

fn wall_clock() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// `VoiceLog(path)`: tails the voice log (rotation/truncation aware).
pub struct VoiceLog {
    pub path: PathBuf,
    pub voice: VoiceState,
    identity: Option<(u64, u64)>,
    position: u64,
    pending: Vec<u8>,
    initial: bool,
    /// Hardening (not in Python): a partial line grew past [`CHUNK`]; its
    /// bytes are dropped up to the next newline so `pending` stays bounded.
    discard: bool,
}

impl VoiceLog {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self { path: crate::expanduser(path.as_ref()), voice: VoiceState::new(), identity: None,
               position: 0, pending: Vec::new(), initial: true, discard: false }
    }

    fn read(&mut self) -> std::io::Result<Vec<u8>> {
        // Hardening (not in Python): O_NONBLOCK so a FIFO or device planted at
        // the log path cannot block the GTK thread in open(2); only regular files are read.
        let mut stream = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&self.path)?;
        let stat = stream.metadata()?;
        if !stat.file_type().is_file() {
            return Err(std::io::Error::other("not a regular file"));
        }
        let identity = (stat.dev(), stat.ino());
        if Some(identity) != self.identity || stat.size() < self.position {
            self.identity = Some(identity);
            self.position = if self.initial { stat.size().saturating_sub(CHUNK) } else { 0 };
            self.pending.clear();
            self.discard = false;
            if !self.initial {
                self.voice.feed("Voice stop: listener released", None);
            }
            if self.position > 0 {
                // Skip a possible partial first line.
                stream.seek(SeekFrom::Start(self.position))?;
                // Bounded (Python's readline() was not): at most one chunk.
                let mut reader = BufReader::new(Read::by_ref(&mut stream).take(CHUNK));
                let mut skipped = Vec::new();
                reader.read_until(b'\n', &mut skipped)?;
                self.position += skipped.len() as u64;
                self.discard = skipped.last() != Some(&b'\n');
            }
        }
        stream.seek(SeekFrom::Start(self.position))?;
        let mut chunk = Vec::new();
        Read::by_ref(&mut stream).take(CHUNK).read_to_end(&mut chunk)?;
        self.position += chunk.len() as u64;
        Ok(chunk)
    }

    /// `poll()` → the current state after expiry.
    pub fn poll(&mut self) -> &'static str {
        if let Ok(chunk) = self.read() {
            let mut data = std::mem::take(&mut self.pending);
            let mut chunk = &chunk[..];
            if self.discard {
                match chunk.iter().position(|b| *b == b'\n') {
                    Some(end) => {
                        chunk = &chunk[end + 1..];
                        self.discard = false;
                    }
                    None => chunk = &[],
                }
            }
            data.extend_from_slice(chunk);
            let mut lines: Vec<&[u8]> = data.split(|b| *b == b'\n').collect();
            let rest = lines.pop().unwrap_or_default().to_vec();
            for raw in lines {
                let line = String::from_utf8_lossy(raw).replace('\0', "");
                if self.initial {
                    let Some(stamp) = parse_local_stamp(&line) else { continue };
                    let age = wall_clock() - stamp;
                    if !(0.0..EXPIRE_SECONDS).contains(&age) {
                        continue;
                    }
                }
                self.voice.feed(&line, None);
            }
            if rest.len() as u64 > CHUNK {
                // An over-long partial line: drop it (and its remainder) instead of growing without bound.
                self.discard = true;
                self.pending = Vec::new();
            } else {
                self.pending = rest;
            }
            self.initial = false;
        }
        self.voice.expire(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn actual_lifecycle_stays_active_through_tools_and_clears_on_completion() {
        let mut voice = VoiceState::new();
        let sequence = [
            ("tools.wake_word: wake word: listening", "idle"),
            ("tools.tts_tool: TTS audio saved: unrelated gateway.wav", "idle"),
            ("tools.voice_mode: Voice recording started (rate=44100, channels=1)", "listening"),
            ("tools.voice_mode: Voice recording stopped", "thinking"),
            ("cli: Voice stop: listening during wake reply", "thinking"),
            ("tools.tts_tool: TTS audio saved: sentence.mp3", "speaking"),
            ("cli: Voice stop: listening during wake reply", "speaking"),
            ("agent.conversation_loop: Turn ended: reason=text_response", "speaking"),
            ("tools.tts_tool: Generating speech with KittenTTS", "speaking"),
            ("cli: Voice stop: listener released", "idle"),
            ("cli: Voice follow-up: opening one reply window", "listening"),
            ("tools.voice_mode: No speech within 10s, auto-stopping", "idle"),
            ("tools.voice_mode: Voice recording stopped", "idle"),
            ("tools.voice_mode: Voice recording started", "listening"),
            ("tools.voice_mode: Recording too quiet", "idle"),
            ("tools.voice_mode: Voice recording started", "listening"),
            ("cli: Voice stop: keyword detected; reply cancelled", "idle"),
        ];
        for (line, expected) in sequence {
            assert_eq!(voice.feed(line, Some(1.0)), expected, "{line}");
        }
        voice.feed("Voice recording started", Some(10.0));
        assert_eq!(voice.expire(Some(191.0)), "idle");
    }

    fn stamp(offset: i64) -> String {
        let now = (wall_clock() as i64 + offset) as libc::time_t;
        // SAFETY: localtime_r writes into the provided tm.
        let tm = unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&now, &mut tm);
            tm
        };
        format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
    }

    #[test]
    fn log_tail_handles_old_events_partial_writes_truncation_and_rotation() {
        let dir = std::env::temp_dir().join(format!("suite-lock-voice-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("voice.log");
        std::fs::write(&path, format!("{} INFO Voice recording started\n", stamp(-3600))).unwrap();
        let mut log = VoiceLog::new(&path);
        assert_eq!(log.poll(), "idle");
        let append = |text: &str| std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(text.as_bytes()).unwrap();
        append("Voice recording sta");
        assert_eq!(log.poll(), "idle");
        append("rted\n");
        assert_eq!(log.poll(), "listening");
        std::fs::write(&path, "Voice recording stopped\n").unwrap();
        assert_eq!(log.poll(), "idle");
        std::fs::rename(&path, path.with_extension("old")).unwrap();
        std::fs::write(&path, "Voice recording started\nTTS audio saved: one.mp3\n").unwrap();
        assert_eq!(log.poll(), "speaking");
        append("\0cli: Voice stop: listener released\n");
        assert_eq!(log.poll(), "idle");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn log_tail_bounds_overlong_lines_and_ignores_fifos() {
        let dir = std::env::temp_dir().join(format!("suite-lock-voice3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("voice.log");
        std::fs::write(&path, "").unwrap();
        let mut log = VoiceLog::new(&path);
        assert_eq!(log.poll(), "idle");
        let append = |text: &str| std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(text.as_bytes()).unwrap();
        // 1 MiB without a newline: pending never exceeds one chunk; the over-long line is dropped.
        for _ in 0..4 {
            append(&"y".repeat(CHUNK as usize));
            log.poll();
            assert!(log.pending.len() as u64 <= CHUNK);
        }
        append(" Voice recording started\nVoice recording started\n");
        assert_eq!(log.poll(), "listening");
        assert!(log.pending.is_empty() && !log.discard);
        // A FIFO at the log path neither blocks nor changes the state.
        let fifo = dir.join("fifo.log");
        let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert_eq!(VoiceLog::new(&fifo).poll(), "idle");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn initial_read_uses_only_recent_stamped_lines_and_skips_partial_tail() {
        let dir = std::env::temp_dir().join(format!("suite-lock-voice2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("voice.log");
        let mut text = "x".repeat(300_000);
        text.push_str(" Voice recording started\n");
        text.push_str(&format!("{} INFO Voice recording started\n", stamp(-5)));
        std::fs::write(&path, text).unwrap();
        let mut log = VoiceLog::new(&path);
        assert_eq!(log.poll(), "listening");
        assert!(parse_local_stamp("2026-02-30 10:00:00").is_none());
        assert!(parse_local_stamp("not a date at all here").is_none());
        let missing = VoiceLog::new(dir.join("missing.log")).poll();
        assert_eq!(missing, "idle");
        let _ = std::fs::remove_dir_all(dir);
    }
}
