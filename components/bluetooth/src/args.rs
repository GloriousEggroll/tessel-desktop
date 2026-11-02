//! `launch_arguments(argv)`: an argparse-compatible parser for
//! `[-h] [--lang LANG] [--popup] [--popup-edge {top,bottom,left,right}]
//! [--popup-offset POPUP_OFFSET]` with Python's messages, unique-prefix
//! abbreviations, `--opt=value`, usage wrapping and exit codes.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Args {
    pub lang: Option<String>,
    pub popup: bool,
    pub popup_edge: Option<String>,
    pub popup_offset: Option<i32>,
}

/// `(exit code, text, to_stdout)`.
pub type Exit = (i32, String, bool);

pub const EDGES: [&str; 4] = ["top", "bottom", "left", "right"];
const OPTIONS: [&str; 5] = ["--help", "--lang", "--popup", "--popup-edge", "--popup-offset"];

/// argparse's `HelpFormatter` width: `$COLUMNS` (else the terminal, else 80) − 2.
fn text_width() -> usize {
    let columns = std::env::var("COLUMNS").ok().and_then(|c| c.trim().parse::<usize>().ok()).filter(|c| *c > 0).or_else(|| {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0;
        (ok && size.ws_col > 0).then_some(size.ws_col as usize)
    }).unwrap_or(80);
    columns.saturating_sub(2).max(11)
}

/// The `usage:` block, wrapped like argparse.
pub fn usage(prog: &str) -> String {
    usage_with_width(prog, text_width())
}

pub fn usage_with_width(prog: &str, text_width: usize) -> String {
    let parts = ["[-h]", "[--lang LANG]", "[--popup]", "[--popup-edge {top,bottom,left,right}]", "[--popup-offset POPUP_OFFSET]"];
    let prefix = "usage: ";
    let full = format!("{prefix}{prog} {}", parts.join(" "));
    if full.len() <= text_width {
        return full + "\n";
    }
    let get_lines = |parts: &[&str], indent: &str, prefix: Option<&str>| -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut line: Vec<&str> = Vec::new();
        let indent_len = indent.chars().count();
        let mut line_len = match prefix { Some(p) => p.chars().count() as isize - 1, None => indent_len as isize - 1 };
        for part in parts {
            let len = part.chars().count() as isize;
            if line_len + 1 + len > text_width as isize && !line.is_empty() {
                lines.push(format!("{indent}{}", line.join(" ")));
                line.clear();
                line_len = indent_len as isize - 1;
            }
            line.push(part);
            line_len += len + 1;
        }
        if !line.is_empty() {
            lines.push(format!("{indent}{}", line.join(" ")));
        }
        if prefix.is_some() {
            lines[0] = lines[0][indent.len()..].to_owned();
        }
        lines
    };
    let prog_len = prog.chars().count();
    let lines = if (prefix.len() + prog_len) as f64 <= 0.75 * text_width as f64 {
        let indent = " ".repeat(prefix.len() + prog_len + 1);
        let mut all = vec![prog];
        all.extend(parts);
        get_lines(&all, &indent, Some(prefix))
    } else {
        let indent = " ".repeat(prefix.len());
        let mut lines = get_lines(&[prog], &indent, Some(prefix));
        lines.extend(get_lines(&parts, &indent, None));
        lines
    };
    format!("{prefix}{}\n", lines.join("\n"))
}

pub fn help(prog: &str) -> String {
    format!("{}\nBluetooth device manager\n\noptions:\n  -h, --help            show this help message and exit\n  --lang LANG\n  --popup\n  --popup-edge {{top,bottom,left,right}}\n  --popup-offset POPUP_OFFSET\n",
            usage(prog))
}

fn error(prog: &str, message: &str) -> Exit {
    (2, format!("{}{prog}: error: {message}\n", usage(prog)), false)
}

/// argparse `_negative_number_matcher`: `^-\d+$|^-\d*\.\d+$`.
fn looks_negative(arg: &str) -> bool {
    let Some(rest) = arg.strip_prefix('-') else { return false };
    if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    match rest.split_once('.') {
        Some((a, b)) => a.chars().all(|c| c.is_ascii_digit()) && !b.is_empty() && b.chars().all(|c| c.is_ascii_digit()),
        None => false,
    }
}

/// Whether argparse treats `arg` as an option string (not a value).
fn is_optional(arg: &str) -> bool {
    if !arg.starts_with('-') || arg == "-" {
        return false;
    }
    if OPTIONS.contains(&arg) || arg == "-h" {
        return true;
    }
    if arg.len() == 1 || looks_negative(arg) || arg.contains(' ') {
        return false;
    }
    true
}

enum Matched {
    One(&'static str, Option<String>),
    Ambiguous(String),
    Unknown,
}

fn match_option(arg: &str) -> Matched {
    if arg == "-h" {
        return Matched::One("--help", None);
    }
    let (name, explicit) = match arg.split_once('=') {
        Some((n, v)) if arg.starts_with("--") => (n, Some(v.to_owned())),
        _ => (arg, None),
    };
    if let Some(exact) = OPTIONS.iter().find(|o| **o == name) {
        return Matched::One(exact, explicit);
    }
    if !name.starts_with("--") {
        return Matched::Unknown;
    }
    let candidates: Vec<&'static str> = OPTIONS.iter().copied().filter(|o| o.starts_with(name)).collect();
    match candidates.len() {
        0 => Matched::Unknown,
        1 => Matched::One(candidates[0], explicit),
        _ => Matched::Ambiguous(format!("ambiguous option: {name} could match {}", candidates.join(", "))),
    }
}

/// Python `int()` of an argument.
fn parse_int(text: &str) -> Option<i64> {
    tessel_core::pyjson::py_int(Some(&serde_json::Value::String(text.into())))
}

pub fn parse(prog: &str, argv: &[String]) -> Result<Args, Exit> {
    let mut args = Args::default();
    let mut unknown: Vec<String> = Vec::new();
    let mut index = 0;
    let mut only_positionals = false;
    while index < argv.len() {
        let arg = &argv[index];
        index += 1;
        if only_positionals || !is_optional(arg) {
            unknown.push(arg.clone());
            continue;
        }
        if arg == "--" {
            only_positionals = true;
            continue;
        }
        let (option, explicit) = match match_option(arg) {
            Matched::One(option, explicit) => (option, explicit),
            Matched::Ambiguous(message) => return Err(error(prog, &message)),
            Matched::Unknown => {
                unknown.push(arg.clone());
                continue;
            }
        };
        if option == "--help" || option == "--popup" {
            if let Some(value) = explicit {
                return Err(error(prog, &format!("argument {}: ignored explicit argument '{value}'",
                                                if option == "--help" { "-h/--help" } else { option })));
            }
            if option == "--help" {
                return Err((0, help(prog), true));
            }
            args.popup = true;
            continue;
        }
        let value = match explicit {
            Some(value) => value,
            None => match argv.get(index) {
                Some(next) if !is_optional(next) && next != "--" => {
                    index += 1;
                    next.clone()
                }
                _ => return Err(error(prog, &format!("argument {option}: expected one argument"))),
            },
        };
        match option {
            "--lang" => args.lang = Some(value),
            "--popup-edge" => {
                if !EDGES.contains(&value.as_str()) {
                    return Err(error(prog, &format!(
                        "argument --popup-edge: invalid choice: '{value}' (choose from 'top', 'bottom', 'left', 'right')")));
                }
                args.popup_edge = Some(value);
            }
            _ => match parse_int(&value) {
                Some(number) => args.popup_offset = Some(number.clamp(i32::MIN as i64, i32::MAX as i64) as i32),
                None => return Err(error(prog, &format!("argument --popup-offset: invalid int value: '{value}'"))),
            },
        }
    }
    if !unknown.is_empty() {
        return Err(error(prog, &format!("unrecognized arguments: {}", unknown.join(" "))));
    }
    if args.popup_offset.is_some_and(|o| o < 0) {
        return Err(error(prog, "--popup-offset must be a nonnegative number"));
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(words: &[&str]) -> Result<Args, Exit> {
        std::env::set_var("COLUMNS", "80");
        parse("tessel-bluetooth.py", &words.iter().map(|w| w.to_string()).collect::<Vec<_>>())
    }

    const USAGE: &str = "usage: tessel-bluetooth.py [-h] [--lang LANG] [--popup]\n                    [--popup-edge {top,bottom,left,right}]\n                    [--popup-offset POPUP_OFFSET]\n";

    fn err(words: &[&str]) -> String {
        let (code, text, stdout) = run(words).unwrap_err();
        assert_eq!((code, stdout), (2, false));
        assert!(text.starts_with(USAGE), "{text}");
        text[USAGE.len()..].to_owned()
    }

    #[test]
    fn accepts_the_launcher_flags() {
        assert_eq!(run(&["--popup", "--popup-edge", "left", "--popup-offset", "98", "--lang", "fr"]).unwrap(),
                   Args { lang: Some("fr".into()), popup: true, popup_edge: Some("left".into()), popup_offset: Some(98) });
        assert_eq!(run(&[]).unwrap(), Args::default());
        assert_eq!(run(&["--popup-o=5", "--popup-e", "top", "--l=de"]).unwrap().popup_offset, Some(5));
        assert_eq!(run(&["--popup-offset", " +1_0 "]).unwrap().popup_offset, Some(10));
        assert_eq!(run(&["--lang", "-"]).unwrap().lang.as_deref(), Some("-"));
    }

    #[test]
    fn matches_python_errors_and_help() {
        assert_eq!(err(&["--popup-edge", "x"]),
                   "tessel-bluetooth.py: error: argument --popup-edge: invalid choice: 'x' (choose from 'top', 'bottom', 'left', 'right')\n");
        assert_eq!(err(&["--popup-offset", "-1"]), "tessel-bluetooth.py: error: --popup-offset must be a nonnegative number\n");
        assert_eq!(err(&["--popup-offset=-5"]), "tessel-bluetooth.py: error: --popup-offset must be a nonnegative number\n");
        assert_eq!(err(&["--popup-offset", "x"]), "tessel-bluetooth.py: error: argument --popup-offset: invalid int value: 'x'\n");
        assert_eq!(err(&["--bogus"]), "tessel-bluetooth.py: error: unrecognized arguments: --bogus\n");
        assert_eq!(err(&["-x"]), "tessel-bluetooth.py: error: unrecognized arguments: -x\n");
        assert_eq!(err(&["positional"]), "tessel-bluetooth.py: error: unrecognized arguments: positional\n");
        assert_eq!(err(&["--popup-o", "5", "--pop"]),
                   "tessel-bluetooth.py: error: ambiguous option: --pop could match --popup, --popup-edge, --popup-offset\n");
        assert_eq!(err(&["--lang"]), "tessel-bluetooth.py: error: argument --lang: expected one argument\n");
        assert_eq!(err(&["--popup-edge"]), "tessel-bluetooth.py: error: argument --popup-edge: expected one argument\n");
        assert_eq!(err(&["--popup=1"]), "tessel-bluetooth.py: error: argument --popup: ignored explicit argument '1'\n");
        let (code, text, stdout) = run(&["-h"]).unwrap_err();
        assert_eq!((code, stdout), (0, true));
        assert_eq!(text, format!("{USAGE}\nBluetooth device manager\n\noptions:\n  -h, --help            show this help message and exit\n  --lang LANG\n  --popup\n  --popup-edge {{top,bottom,left,right}}\n  --popup-offset POPUP_OFFSET\n"));
    }

    #[test]
    fn usage_fits_on_one_line_when_wide() {
        assert_eq!(usage_with_width("tessel-bluetooth.py", 200),
                   "usage: tessel-bluetooth.py [-h] [--lang LANG] [--popup] [--popup-edge {top,bottom,left,right}] [--popup-offset POPUP_OFFSET]\n");
    }
}
