//! `argparse` twin of `session_manager.arguments()`: same flags, prefix
//! abbreviations, `int()` parsing, choices and error/usage/help text.

pub const DESCRIPTION: &str = "On-demand session controls for the dock; no resident service.";
pub const EDGES: [&str; 4] = ["top", "bottom", "left", "right"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arguments {
    pub popup: bool,
    /// Centered on the pointer monitor with exclusive keyboard (shortcuts, power button).
    pub menu: bool,
    pub popup_edge: String,
    pub popup_offset: i64,
}

impl Default for Arguments {
    fn default() -> Self { Self { popup: false, menu: false, popup_edge: "bottom".into(), popup_offset: 76 } }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Run(Arguments),
    /// `-h/--help`: print [`help`] to stdout, exit 0.
    Help,
    /// argparse `error()`: print usage + `prog: error: msg` to stderr, exit 2.
    Error(String),
}

const OPTIONS: [&str; 5] = ["--help", "--popup", "--menu", "--popup-edge", "--popup-offset"];

/// Python `int(text)` for decimal strings: surrounding whitespace, a sign,
/// single underscores between digits.
pub fn python_int(text: &str) -> Option<i64> {
    let trimmed = text.trim_matches(|c: char| c.is_whitespace());
    let (negative, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let mut value: i64 = 0;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        let digit = c.to_digit(10)? as i64;
        value = value.checked_mul(10)?.checked_add(digit)?;
    }
    Some(if negative { -value } else { value })
}

// argparse's `_negative_number_matcher`.
fn looks_negative(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('-') else { return false };
    let mut parts = rest.splitn(2, '.');
    let int = parts.next().unwrap_or("");
    let frac = parts.next();
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    match frac {
        None => digits(int),
        Some(frac) => (int.is_empty() || digits(int)) && digits(frac),
    }
}

// Whether argparse treats `arg` as an option string rather than a value.
fn is_option_like(arg: &str) -> bool {
    if !arg.starts_with('-') || arg == "-" || arg == "--" {
        return arg == "--";
    }
    !(looks_negative(arg) && !arg.contains(' '))
}

enum Resolved {
    Exact(&'static str, Option<String>),
    Ambiguous(String),
    Unknown,
}

fn resolve(arg: &str) -> Resolved {
    if arg == "-h" {
        return Resolved::Exact("--help", None);
    }
    let (name, explicit) = match arg.split_once('=') {
        Some((name, value)) => (name, Some(value.to_owned())),
        None => (arg, None),
    };
    if let Some(exact) = OPTIONS.iter().find(|option| **option == name) {
        return Resolved::Exact(exact, explicit);
    }
    if !name.starts_with("--") || name.len() < 3 {
        return Resolved::Unknown;
    }
    let matches: Vec<&'static str> = OPTIONS.iter().copied().filter(|option| option.starts_with(name)).collect();
    match matches.len() {
        0 => Resolved::Unknown,
        1 => Resolved::Exact(matches[0], explicit),
        _ => Resolved::Ambiguous(format!("ambiguous option: {name} could match {}", matches.join(", "))),
    }
}

pub fn parse<S: AsRef<str>>(argv: &[S]) -> Outcome {
    let argv: Vec<&str> = argv.iter().map(AsRef::as_ref).collect();
    let mut args = Arguments::default();
    let mut unrecognized: Vec<String> = Vec::new();
    let mut index = 0;
    let mut positional_only = false;
    while index < argv.len() {
        let arg = argv[index];
        index += 1;
        if positional_only || !is_option_like(arg) {
            unrecognized.push(arg.into());
            continue;
        }
        if arg == "--" {
            positional_only = true;
            unrecognized.push(arg.into());
            continue;
        }
        let (option, explicit) = match resolve(arg) {
            Resolved::Exact(option, explicit) => (option, explicit),
            Resolved::Ambiguous(message) => return Outcome::Error(message),
            Resolved::Unknown => {
                unrecognized.push(arg.into());
                continue;
            }
        };
        match option {
            "--help" => {
                if let Some(value) = explicit {
                    return Outcome::Error(format!("argument -h/--help: ignored explicit argument '{value}'"));
                }
                return Outcome::Help;
            }
            "--popup" | "--menu" => {
                if let Some(value) = explicit {
                    return Outcome::Error(format!("argument {option}: ignored explicit argument '{value}'"));
                }
                if option == "--popup" { args.popup = true } else { args.menu = true }
                continue;
            }
            _ => {}
        }
        let value = match explicit {
            Some(value) => value,
            None => match argv.get(index) {
                Some(next) if !is_option_like(next) => {
                    index += 1;
                    (*next).to_owned()
                }
                _ => return Outcome::Error(format!("argument {option}: expected one argument")),
            },
        };
        if option == "--popup-edge" {
            if !EDGES.contains(&value.as_str()) {
                return Outcome::Error(format!(
                    "argument --popup-edge: invalid choice: '{}' (choose from 'top', 'bottom', 'left', 'right')",
                    python_repr_body(&value)));
            }
            args.popup_edge = value;
        } else {
            match python_int(&value) {
                Some(number) => args.popup_offset = number,
                None => return Outcome::Error(format!("argument --popup-offset: invalid int value: '{}'",
                                                      python_repr_body(&value))),
            }
        }
    }
    if !unrecognized.is_empty() {
        return Outcome::Error(format!("unrecognized arguments: {}", unrecognized.join(" ")));
    }
    if args.popup_offset < 0 {
        return Outcome::Error("--popup-offset must be nonnegative".into());
    }
    Outcome::Run(args)
}

fn python_repr_body(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

/// The program name argparse shows: basename of argv[0].
pub fn prog(argv0: &str) -> String {
    std::path::Path::new(argv0).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| argv0.into())
}

/// argparse's usage block (80-column terminal, as when output is piped).
pub fn usage(prog: &str) -> String {
    let text_width = 78usize;
    let parts: Vec<String> = ["[-h]", "[--popup]", "[--menu]", "[--popup-edge {top,bottom,left,right}]",
                              "[--popup-offset POPUP_OFFSET]"].iter().map(|s| s.to_string()).collect();
    let prefix = "usage: ";
    let full = format!("{prog} {}", parts.join(" "));
    if prefix.len() + full.len() <= text_width {
        return format!("{prefix}{full}\n");
    }
    let indent = " ".repeat(prefix.len() + prog.len() + 1);
    let mut all = vec![prog.to_owned()];
    all.extend(parts);
    let mut lines: Vec<String> = Vec::new();
    let mut line: Vec<String> = Vec::new();
    let mut line_len = prefix.len() - 1;
    for part in all {
        if line_len + 1 + part.len() > text_width && !line.is_empty() {
            lines.push(format!("{indent}{}", line.join(" ")));
            line.clear();
            line_len = indent.len() - 1;
        }
        line_len += part.len() + 1;
        line.push(part);
    }
    if !line.is_empty() {
        lines.push(format!("{indent}{}", line.join(" ")));
    }
    lines[0] = lines[0][indent.len()..].to_owned();
    format!("{prefix}{}\n", lines.join("\n"))
}

pub fn help(prog: &str) -> String {
    format!("{}\n{DESCRIPTION}\n\noptions:\n  -h, --help            show this help message and exit\n  --popup\n  --menu                open centered on the pointer monitor (keyboard\n                        shortcuts, power button)\n  --popup-edge {{top,bottom,left,right}}\n  --popup-offset POPUP_OFFSET\n",
            usage(prog))
}

/// The stderr text of `parser.error(message)`.
pub fn error_text(prog: &str, message: &str) -> String {
    format!("{}{prog}: error: {message}\n", usage(prog))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(argv: &[&str]) -> Arguments {
        match parse(argv) {
            Outcome::Run(args) => args,
            other => panic!("{argv:?} -> {other:?}"),
        }
    }

    fn error(argv: &[&str]) -> String {
        match parse(argv) {
            Outcome::Error(message) => message,
            other => panic!("{argv:?} -> {other:?}"),
        }
    }

    #[test]
    fn flags_defaults_and_abbreviations() {
        assert_eq!(run(&[]), Arguments::default());
        assert!(run(&["--menu"]).menu);
        assert!(run(&["--m"]).menu);
        let args = run(&["--popup", "--popup-edge", "top", "--popup-offset", "45"]);
        assert_eq!((args.popup, args.menu, args.popup_edge.as_str(), args.popup_offset), (true, false, "top", 45));
        assert_eq!(run(&["--popup-o=4"]).popup_offset, 4);
        assert_eq!(run(&["--popup-offset", " 4_5 "]).popup_offset, 45);
        assert!(run(&["--popup", "--popup", "--menu"]).popup);
        assert_eq!(parse(&["-h"]), Outcome::Help);
        assert_eq!(parse(&["--he"]), Outcome::Help);
    }

    #[test]
    fn errors_match_argparse() {
        assert_eq!(error(&["--popup-edge", "x"]),
                   "argument --popup-edge: invalid choice: 'x' (choose from 'top', 'bottom', 'left', 'right')");
        assert_eq!(error(&["--pop"]), "ambiguous option: --pop could match --popup, --popup-edge, --popup-offset");
        assert_eq!(error(&["--popup-o", "3x"]), "argument --popup-offset: invalid int value: '3x'");
        assert_eq!(error(&["--popup-offset", "-3"]), "--popup-offset must be nonnegative");
        assert_eq!(error(&["extra"]), "unrecognized arguments: extra");
        assert_eq!(error(&["--popup-offset"]), "argument --popup-offset: expected one argument");
        assert_eq!(error(&["--menu=1"]), "argument --menu: ignored explicit argument '1'");
    }

    #[test]
    fn usage_and_help_match_python() {
        let usage_text = "usage: session_manager.py [-h] [--popup] [--menu]\n                          [--popup-edge {top,bottom,left,right}]\n                          [--popup-offset POPUP_OFFSET]\n";
        assert_eq!(usage("session_manager.py"), usage_text);
        assert_eq!(error_text("session_manager.py", "unrecognized arguments: extra"),
                   format!("{usage_text}session_manager.py: error: unrecognized arguments: extra\n"));
        assert!(help("session_manager.py").starts_with(&format!("{usage_text}\n{DESCRIPTION}\n\noptions:\n")));
    }

    #[test]
    fn shim_keeps_the_menu_flag_desktop_suite_action_greps_for() {
        // desktop-suite-action: grep -E "[\"']--menu[\"']" session_manager.py
        let shim = include_str!("../shim/session_manager.py");
        assert!(shim.contains("'--menu'") || shim.contains("\"--menu\""));
        assert!(shim.contains("os.execv") && shim.contains("\"session-manager\""));
    }
}
