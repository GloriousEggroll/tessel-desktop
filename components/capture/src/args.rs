//! argparse-compatible command line (capture.py `arguments`).

pub const DESCRIPTION: &str = "Portal-backed Screenshot and Screen Recorder.";
pub const AUDIO_PRESETS: [&str; 3] = ["none", "desktop", "desktop-mic"];
pub const CAPTURE_TARGETS: [&str; 4] = ["workspace", "screen", "window", "area"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub record: bool,
    pub screenshot: bool,
    pub stop: bool,
    pub start_recording: bool,
    pub toggle_recording: bool,
    pub audio_preset: Option<String>,
    pub capture: Option<String>,
    pub lock_shot: bool,
    pub check: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Parsed(Options),
    /// `--help`: text for stdout, exit 0.
    Help(String),
    /// argparse error: text for stderr, exit 2.
    Error(String),
}

const FLAGS: [&str; 10] = ["--help", "--record", "--screenshot", "--stop", "--start-recording", "--toggle-recording",
    "--audio-preset", "--capture", "--lock-shot", "--check"];

fn width() -> usize {
    std::env::var("COLUMNS").ok().and_then(|v| v.trim().parse::<usize>().ok()).filter(|w| *w > 0).unwrap_or(80).saturating_sub(2)
}

/// argparse's wrapped usage line(s).
pub fn usage(prog: &str) -> String {
    let parts = ["[-h]", "[--record]", "[--screenshot]", "[--stop]", "[--start-recording]", "[--toggle-recording]",
        "[--audio-preset {none,desktop,desktop-mic}]", "[--capture {workspace,screen,window,area}]", "[--lock-shot]", "[--check]"];
    let text_width = width();
    let full = format!("usage: {prog} {}", parts.join(" "));
    if full.len() <= text_width {
        return full + "\n";
    }
    let indent = " ".repeat("usage: ".len() + prog.len() + 1);
    let mut lines: Vec<String> = Vec::new();
    let mut line: Vec<&str> = Vec::new();
    let mut line_len = indent.len() - 1;
    for part in parts {
        if line_len + 1 + part.len() > text_width && !line.is_empty() {
            lines.push(line.join(" "));
            line.clear();
            line_len = indent.len() - 1;
        }
        line.push(part);
        line_len += part.len() + 1;
    }
    if !line.is_empty() {
        lines.push(line.join(" "));
    }
    let mut out = format!("usage: {prog} {}", lines[0]);
    for rest in &lines[1..] {
        out.push('\n');
        out.push_str(&indent);
        out.push_str(rest);
    }
    out + "\n"
}

pub fn help(prog: &str) -> String {
    format!("{}\n{DESCRIPTION}\n\noptions:\n{}", usage(prog), concat!(
        "  -h, --help            show this help message and exit\n",
        "  --record              open screen recording\n",
        "  --screenshot          open screenshots\n",
        "  --stop                stop and save the current recording\n",
        "  --start-recording     start recording with saved settings\n",
        "  --toggle-recording    start or stop recording\n",
        "  --audio-preset {none,desktop,desktop-mic}\n",
        "  --capture {workspace,screen,window,area}\n",
        "                        take a screenshot directly\n",
        "  --lock-shot           take a previously authorized tessel-lock screenshot\n",
        "  --check               check local capture dependencies\n"))
}

fn error(prog: &str, message: &str) -> Outcome {
    Outcome::Error(format!("{}{prog}: error: {message}\n", usage(prog)))
}

fn choices(list: &[&str]) -> String {
    list.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(", ")
}

/// Parse `argv[1:]` like `argparse.ArgumentParser.parse_args`.
pub fn parse(prog: &str, args: &[String]) -> Outcome {
    let mut options = Options::default();
    let mut unrecognized: Vec<String> = Vec::new();
    let mut index = 0;
    let mut only_positional = false;
    while index < args.len() {
        let arg = &args[index];
        index += 1;
        if only_positional || !arg.starts_with('-') || arg == "-" {
            unrecognized.push(arg.clone());
            continue;
        }
        if arg == "--" {
            only_positional = true;
            continue;
        }
        if arg == "-h" {
            return Outcome::Help(help(prog));
        }
        if !arg.starts_with("--") {
            unrecognized.push(arg.clone());
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name.to_string(), Some(value.to_string())),
            None => (arg.clone(), None),
        };
        let flag = if FLAGS.contains(&name.as_str()) {
            name.clone()
        } else {
            let matches: Vec<&str> = FLAGS.iter().copied().filter(|f| f.starts_with(name.as_str())).collect();
            match matches.len() {
                1 => matches[0].to_string(),
                0 => {
                    unrecognized.push(arg.clone());
                    continue;
                }
                _ => {
                    let arg_text = match &inline { Some(v) => format!("{name}={v}"), None => name.clone() };
                    return error(prog, &format!("ambiguous option: {arg_text} could match {}", matches.join(", ")));
                }
            }
        };
        let takes_value = flag == "--audio-preset" || flag == "--capture";
        if !takes_value {
            if inline.is_some() {
                return error(prog, &format!("argument {}: ignored explicit argument '{}'", flag_label(&flag), inline.unwrap()));
            }
            match flag.as_str() {
                "--help" => return Outcome::Help(help(prog)),
                "--record" => options.record = true,
                "--screenshot" => options.screenshot = true,
                "--stop" => options.stop = true,
                "--start-recording" => options.start_recording = true,
                "--toggle-recording" => options.toggle_recording = true,
                "--lock-shot" => options.lock_shot = true,
                "--check" => options.check = true,
                _ => unreachable!(),
            }
            continue;
        }
        let value = match inline {
            Some(value) => value,
            None => {
                if index < args.len() && !(args[index].starts_with('-') && args[index].len() > 1) {
                    index += 1;
                    args[index - 1].clone()
                } else {
                    return error(prog, &format!("argument {flag}: expected one argument"));
                }
            }
        };
        let list: &[&str] = if flag == "--capture" { &CAPTURE_TARGETS } else { &AUDIO_PRESETS };
        if !list.contains(&value.as_str()) {
            return error(prog, &format!("argument {flag}: invalid choice: {} (choose from {})", crate::compat::py_repr(&value), choices(list)));
        }
        if flag == "--capture" {
            options.capture = Some(value);
        } else {
            options.audio_preset = Some(value);
        }
    }
    if !unrecognized.is_empty() {
        return error(prog, &format!("unrecognized arguments: {}", unrecognized.join(" ")));
    }
    Outcome::Parsed(options)
}

fn flag_label(flag: &str) -> String {
    if flag == "--help" { "-h/--help".into() } else { flag.into() }
}

/// `prog` = basename of argv[0] (argparse default).
pub fn prog_name(argv0: Option<&str>) -> String {
    argv0.and_then(|a| std::path::Path::new(a).file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "tessel-capture".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_like_argparse() {
        let Outcome::Parsed(options) = parse("tessel-capture", &args(&["--rec", "--capture=area", "--audio-preset", "desktop-mic"])) else { panic!() };
        assert!(options.record);
        assert_eq!(options.capture.as_deref(), Some("area"));
        assert_eq!(options.audio_preset.as_deref(), Some("desktop-mic"));
        let usage = usage("tessel-capture");
        assert_eq!(usage, "usage: tessel-capture [-h] [--record] [--screenshot] [--stop] [--start-recording]\n                  [--toggle-recording]\n                  [--audio-preset {none,desktop,desktop-mic}]\n                  [--capture {workspace,screen,window,area}] [--lock-shot]\n                  [--check]\n");
        assert_eq!(parse("tessel-capture", &args(&["--capture", "x"])), Outcome::Error(format!(
            "{usage}tessel-capture: error: argument --capture: invalid choice: 'x' (choose from 'workspace', 'screen', 'window', 'area')\n")));
        assert_eq!(parse("tessel-capture", &args(&["--st"])), Outcome::Error(format!(
            "{usage}tessel-capture: error: ambiguous option: --st could match --stop, --start-recording\n")));
        assert_eq!(parse("tessel-capture", &args(&["foo"])), Outcome::Error(format!("{usage}tessel-capture: error: unrecognized arguments: foo\n")));
        assert!(matches!(parse("tessel-capture", &args(&["--help"])), Outcome::Help(text) if text.contains("  --lock-shot           take a previously authorized tessel-lock screenshot\n")));
        assert!(matches!(parse("tessel-capture", &args(&["--capture"])), Outcome::Error(text) if text.ends_with("argument --capture: expected one argument\n")));
    }
}
