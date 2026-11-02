//! The xdpw dmenu entry point (capture_picker.py `main`, minus the GUI).
//!
//! Stdout is reserved for precisely one accepted protocol record, or nothing.
//! Everything else is diagnostic and goes to stderr.

use crate::sources::{parse_sources, write_selection, CaptureSource, MAX_INPUT_BYTES};
use std::io::{Read, Write};

/// The GDK_DEBUG value with `no-portals` added (unless `all` is present, which
/// inverts the other flags: `all,no-portals` ENABLES portals).
pub fn portal_debug_flags(existing: &str) -> String {
    let mut flags: Vec<&str> = existing
        .split([':', ';', ',', ' ', '\t'])
        .filter(|flag| !flag.is_empty() && !flag.eq_ignore_ascii_case("no-portals"))
        .collect();
    if !flags.iter().any(|flag| flag.eq_ignore_ascii_case("all")) {
        flags.push("no-portals");
    }
    flags.join(",")
}

/// xdpw waits synchronously for this chooser. Asking its portal frontend for
/// GTK settings here can deadlock behind that same selection request. Keep
/// this override local to the helper; the suite supplies its palette.
/// Must run before GTK initialises (single-threaded, start of `main`).
pub fn configure_portal_environment() {
    let existing = std::env::var("GDK_DEBUG").unwrap_or_default();
    std::env::set_var("GDK_DEBUG", portal_debug_flags(&existing));
    std::env::set_var("GTK_USE_PORTAL", "0");
}

/// Python's `int(text)` for the decimal forms xdpw could send.
fn python_int(text: &str) -> Option<i64> {
    let text = tessel_core::pyjson::py_strip(text);
    let (negative, digits) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let mut value: i64 = 0;
    for ch in digits.chars() {
        if ch == '_' {
            continue;
        }
        let digit = ch.to_digit(10).or_else(|| unicode_digit(ch))? as i64;
        // Only the low two bits matter; keep the value bounded.
        value = (value * 10 + digit) % (1 << 40);
    }
    Some(if negative { -value } else { value })
}

/// Unicode decimal digits accepted by `int()` (Nd blocks of ten).
fn unicode_digit(ch: char) -> Option<u32> {
    const ZEROS: &[u32] = &[0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66, 0xDE6,
        0xE50, 0xED0, 0xF20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90, 0x1B50, 0x1BB0, 0x1C40,
        0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0, 0xFF10];
    let code = ch as u32;
    ZEROS.iter().find(|&&zero| (zero..zero + 10).contains(&code)).map(|zero| code - zero)
}

/// `int(XDPW_CHOOSER_REMOTE_DESKTOP or '0') & 3`, non-integers → 3.
/// xdpw sets it for RemoteDesktop sessions: accepting also grants input
/// control (1: keyboard, 2: pointer).
pub fn remote_control_devices_from(value: Option<&str>) -> u8 {
    match python_int(value.unwrap_or("0")) {
        Some(number) => (number & 3) as u8,
        None => 3,
    }
}

pub fn remote_control_devices() -> u8 {
    remote_control_devices_from(std::env::var("XDPW_CHOOSER_REMOTE_DESKTOP").ok().as_deref())
}

/// The external effects of `main`, injectable for tests.
pub struct Hooks<'a> {
    /// `capture_selection.requested_selection(session, sources)`.
    pub requested_selection: &'a mut dyn FnMut(&str, &[CaptureSource]) -> (bool, Option<CaptureSource>),
    /// The GUI: `Ok(None)` is cancel.
    pub choose: &'a mut dyn FnMut(&[CaptureSource], u8, &mut dyn Write) -> Result<Option<CaptureSource>, String>,
    /// `capture_selection.report_selection(session, outputs)`.
    pub report_selection: &'a mut dyn FnMut(&str, &[String]),
    /// `capture_output.selected_outputs(source)`.
    pub selected_outputs: &'a mut dyn FnMut(&CaptureSource) -> Vec<String>,
}

/// `main()`: returns the exit status. `env` is `os.environ.get`.
pub fn run(argv_len: usize, stdin: &mut dyn Read, stdout: &mut dyn Write, stderr: &mut dyn Write,
           env: &dyn Fn(&str) -> Option<String>, hooks: Hooks) -> i32 {
    match run_inner(argv_len, stdin, stdout, stderr, env, hooks) {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(stderr, "Tessel Shell capture picker: {error}");
            let _ = stderr.flush();
            1
        }
    }
}

fn run_inner(argv_len: usize, stdin: &mut dyn Read, stdout: &mut dyn Write, stderr: &mut dyn Write,
             env: &dyn Fn(&str) -> Option<String>, hooks: Hooks) -> Result<(), String> {
    if argv_len != 1 {
        return Err("This chooser takes its portal source list on standard input.".into());
    }
    let mut data = Vec::new();
    stdin.take(MAX_INPUT_BYTES as u64 + 1).read_to_end(&mut data).map_err(|e| tessel_core::pyerr::os_error_text(&e))?;
    let sources = parse_sources(&data).map_err(|e| e.to_string())?;
    let session = env("XDPW_CHOOSER_SESSION").unwrap_or_default();
    let remote_control = remote_control_devices_from(env("XDPW_CHOOSER_REMOTE_DESKTOP").as_deref());
    let mut handled = false;
    let mut selected = None;
    // Any requester can answer CaptureSelection on its own bus name, so input
    // control is never granted without the visible picker.
    if remote_control == 0 {
        (handled, selected) = (hooks.requested_selection)(&session, &sources);
    }
    if !handled {
        selected = (hooks.choose)(&sources, remote_control, stderr)?;
    }
    if let Some(source) = &selected {
        let outputs = (hooks.selected_outputs)(source);
        (hooks.report_selection)(&session, &outputs);
    }
    write_selection(selected.as_ref(), &sources, stdout).map_err(|e| e.to_string())
}

static PARENT: std::sync::OnceLock<libc::pid_t> = std::sync::OnceLock::new();

/// The portal's pid, captured at the first call (`main` calls it first thing,
/// so an early parent exit is still noticed by the 1 s poll).
pub fn parent_pid() -> libc::pid_t {
    *PARENT.get_or_init(|| unsafe { libc::getppid() })
}

/// Route fd 1 to stderr for the rest of the process and return a writer on
/// the original stdout: incidental prints from GTK, GLib or helpers can never
/// reach the portal's pipe (Python: `redirect_stdout(sys.stderr)`).
pub fn reserve_stdout() -> std::fs::File {
    use std::os::fd::FromRawFd;
    unsafe {
        let saved = libc::fcntl(1, libc::F_DUPFD_CLOEXEC, 3);
        if saved < 0 {
            // No stdout at all: the record goes nowhere, like a closed pipe.
            return std::fs::File::from_raw_fd(libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC));
        }
        libc::dup2(2, 1);
        std::fs::File::from_raw_fd(saved)
    }
}
