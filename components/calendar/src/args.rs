//! The `calendar_manager.py` argparse interface: same flags, abbreviations,
//! mutual exclusion, messages and exit status 2 on errors.

pub const EDGES: [&str; 4] = ["top", "bottom", "left", "right"];
const DESCRIPTION: &str = "A Waybar calendar popup and system date settings.";
const OPTIONS: [&str; 5] = ["--help", "--popup", "--settings", "--popup-edge", "--popup-offset"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    pub popup: bool,
    pub settings: bool,
    pub popup_edge: String,
    pub popup_offset: i64,
}

impl Default for Args {
    fn default() -> Self {
        Self { popup: false, settings: false, popup_edge: "bottom".into(), popup_offset: 76 }
    }
}

/// `Err((exit_status, text, to_stdout))`.
pub type Outcome = Result<Args, (i32, String, bool)>;

/// argparse's usage line, wrapped like `HelpFormatter` at 80 columns.
pub fn usage(prog: &str) -> String {
    let parts = ["[-h]", "[--popup | --settings]", "[--popup-edge {top,bottom,left,right}]", "[--popup-offset POPUP_OFFSET]"];
    let prefix = "usage: ";
    let text_width = 78usize;
    let one_line = format!("{prog} {}", parts.join(" "));
    if prefix.len() + one_line.chars().count() <= text_width {
        return format!("{prefix}{one_line}\n");
    }
    let indent = " ".repeat(prefix.len() + prog.chars().count() + 1);
    let mut lines: Vec<String> = Vec::new();
    let mut line: Vec<&str> = vec![prog];
    let mut length = prefix.len() - 1 + prog.chars().count() + 1;
    for part in parts {
        if length + 1 + part.len() > text_width && !line.is_empty() {
            lines.push(line.join(" "));
            line.clear();
            length = indent.len() - 1;
        }
        line.push(part);
        length += part.len() + 1;
    }
    if !line.is_empty() {
        lines.push(line.join(" "));
    }
    let mut text = String::from(prefix);
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            text.push_str(&indent);
        }
        text.push_str(line);
        text.push('\n');
    }
    text
}

pub fn help(prog: &str) -> String {
    format!("{}\n{DESCRIPTION}\n\noptions:\n  -h, --help            show this help message and exit\n  \
             --popup               show the calendar menu (default)\n  --settings            open system date settings\n  \
             --popup-edge {{top,bottom,left,right}}\n  --popup-offset POPUP_OFFSET\n", usage(prog))
}

fn error(prog: &str, message: &str) -> (i32, String, bool) {
    (2, format!("{}{prog}: error: {message}\n", usage(prog)), false)
}

/// argparse `_negative_number_matcher`: `^-\d+$|^-\d*\.\d+$`.
fn negative_number(value: &str) -> bool {
    let Some(rest) = value.strip_prefix('-') else { return false };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(rest) || rest.split_once('.').is_some_and(|(a, b)| (a.is_empty() || digits(a)) && digits(b))
}

/// Whether argparse would treat `value` as an option string rather than a value.
fn looks_like_option(value: &str) -> bool {
    if !value.starts_with('-') || value == "-" {
        return false;
    }
    if negative_number(value) {
        return false;
    }
    !value.contains(' ') || OPTIONS.iter().any(|o| value.split('=').next() == Some(*o))
}

/// Python `int()` for argument values.
fn python_int(text: &str) -> Option<i64> {
    crate::py::int(text).and_then(|v| i64::try_from(v).ok())
}

pub fn parse(prog: &str, argv: &[String]) -> Outcome {
    let mut args = Args::default();
    let mut mode: Option<&'static str> = None;
    let mut unrecognized: Vec<String> = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        let word = &argv[index];
        index += 1;
        if word == "--" {
            unrecognized.extend(argv[index..].iter().cloned());
            break;
        }
        if word == "-h" {
            return Err((0, help(prog), true));
        }
        if !word.starts_with("--") || word.len() == 2 {
            unrecognized.push(word.clone());
            continue;
        }
        let (name, explicit) = match word.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (word.as_str(), None),
        };
        let option: &'static str = if let Some(exact) = OPTIONS.iter().find(|o| **o == name) {
            exact
        } else {
            let matches: Vec<&'static str> = OPTIONS.iter().copied().filter(|o| o.starts_with(name)).collect();
            match matches.len() {
                0 => {
                    unrecognized.push(word.clone());
                    continue;
                }
                1 => matches[0],
                _ => return Err(error(prog, &format!("ambiguous option: {name} could match {}", matches.join(", ")))),
            }
        };
        let flag = matches!(option, "--help" | "--popup" | "--settings");
        let value = if flag {
            if let Some(explicit) = explicit {
                return Err(error(prog, &format!("argument {option}: ignored explicit argument '{explicit}'")));
            }
            None
        } else if let Some(explicit) = explicit {
            Some(explicit)
        } else {
            match argv.get(index) {
                Some(next) if !looks_like_option(next) => {
                    index += 1;
                    Some(next.clone())
                }
                _ => return Err(error(prog, &format!("argument {option}: expected one argument"))),
            }
        };
        if matches!(option, "--popup" | "--settings") {
            if let Some(other) = mode.filter(|o| *o != option) {
                return Err(error(prog, &format!("argument {option}: not allowed with argument {other}")));
            }
            mode = Some(option);
        }
        match option {
            "--help" => return Err((0, help(prog), true)),
            "--popup" => args.popup = true,
            "--settings" => args.settings = true,
            "--popup-edge" => {
                let value = value.unwrap();
                if !EDGES.contains(&value.as_str()) {
                    let choices = EDGES.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(", ");
                    return Err(error(prog, &format!("argument --popup-edge: invalid choice: '{value}' (choose from {choices})")));
                }
                args.popup_edge = value;
            }
            "--popup-offset" => {
                let value = value.unwrap();
                args.popup_offset = python_int(&value)
                    .ok_or_else(|| error(prog, &format!("argument --popup-offset: invalid int value: '{value}'")))?;
            }
            _ => unreachable!(),
        }
    }
    if !unrecognized.is_empty() {
        return Err(error(prog, &format!("unrecognized arguments: {}", unrecognized.join(" "))));
    }
    if args.popup_offset < 0 {
        return Err(error(prog, "--popup-offset must be nonnegative"));
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(words: &[&str]) -> Outcome {
        parse("calendar_manager.py", &words.iter().map(|w| w.to_string()).collect::<Vec<_>>())
    }

    fn message(words: &[&str]) -> String {
        let (code, text, stdout) = p(words).unwrap_err();
        assert_eq!((code, stdout), (2, false));
        assert!(text.starts_with(&usage("calendar_manager.py")));
        text.lines().last().unwrap().to_owned()
    }

    #[test]
    fn argparse_compatible() {
        assert_eq!(p(&[]).unwrap(), Args::default());
        assert!(p(&["--set"]).unwrap().settings);
        assert!(p(&["--popup", "--popup"]).unwrap().popup);
        let a = p(&["--popup", "--popup-edge", "top", "--popup-offset=12"]).unwrap();
        assert_eq!((a.popup_edge.as_str(), a.popup_offset), ("top", 12));
        assert_eq!(p(&["--popup-o", " 7 "]).unwrap().popup_offset, 7);
        assert_eq!(message(&["--popup", "--settings"]), "calendar_manager.py: error: argument --settings: not allowed with argument --popup");
        assert_eq!(message(&["--settings", "--popup"]), "calendar_manager.py: error: argument --popup: not allowed with argument --settings");
        assert_eq!(message(&["--popup-offset", "-5"]), "calendar_manager.py: error: --popup-offset must be nonnegative");
        assert_eq!(message(&["--popup-offset=-3"]), "calendar_manager.py: error: --popup-offset must be nonnegative");
        assert_eq!(message(&["--popup-edge", "x"]),
                   "calendar_manager.py: error: argument --popup-edge: invalid choice: 'x' (choose from 'top', 'bottom', 'left', 'right')");
        assert_eq!(message(&["--pop"]), "calendar_manager.py: error: ambiguous option: --pop could match --popup, --popup-edge, --popup-offset");
        assert_eq!(message(&["x"]), "calendar_manager.py: error: unrecognized arguments: x");
        assert_eq!(message(&["--popup-offset", "1.5"]), "calendar_manager.py: error: argument --popup-offset: invalid int value: '1.5'");
        assert_eq!(message(&["--popup=1"]), "calendar_manager.py: error: argument --popup: ignored explicit argument '1'");
        assert_eq!(message(&["--popup-offset"]), "calendar_manager.py: error: argument --popup-offset: expected one argument");
        assert_eq!(message(&["--popup-edge", "--popup"]), "calendar_manager.py: error: argument --popup-edge: expected one argument");
        let help = p(&["--help"]).unwrap_err();
        assert_eq!((help.0, help.2), (0, true));
        assert_eq!(p(&["-h"]).unwrap_err().0, 0);
    }

    #[test]
    fn usage_and_help_match_python_layout() {
        assert_eq!(usage("calendar_manager.py"), "usage: calendar_manager.py [-h] [--popup | --settings]\n                           \
            [--popup-edge {top,bottom,left,right}]\n                           [--popup-offset POPUP_OFFSET]\n");
        assert!(help("calendar_manager.py").ends_with("\nA Waybar calendar popup and system date settings.\n\noptions:\n  -h, --help            \
            show this help message and exit\n  --popup               show the calendar menu (default)\n  --settings            open system date settings\n  \
            --popup-edge {top,bottom,left,right}\n  --popup-offset POPUP_OFFSET\n"));
    }
}
