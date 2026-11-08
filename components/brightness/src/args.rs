//! The `brightness_manager.py` argparse interface: same flags, abbreviations,
//! mutual exclusion, messages and exit status 2 on errors.

use tessel_core::tr;

pub const EDGES: [&str; 4] = ["top", "bottom", "left", "right"];
const DESCRIPTION: &str = "Shared monitor controls for Tessel and tessel-displays.";

#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    pub popup: bool,
    pub settings: bool,
    pub background: bool,
    pub check: bool,
    pub brightness_up: bool,
    pub brightness_down: bool,
    pub idle_dim: Option<String>,
    pub idle_restore: Option<String>,
    pub popup_edge: String,
    pub popup_offset: i64,
}

impl Default for Args {
    fn default() -> Self {
        Self { popup: false, settings: false, background: false, check: false, brightness_up: false, brightness_down: false,
               idle_dim: None, idle_restore: None, popup_edge: "bottom".into(), popup_offset: 76 }
    }
}

fn given(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|v| !v.is_empty())
}

impl Args {
    /// Processes that must never become the primary (IS_LAUNCHER).
    pub fn launcher(&self) -> bool {
        self.check || given(&self.idle_dim) || given(&self.idle_restore) || self.brightness_up || self.brightness_down
    }

    pub fn idle_dim_token(&self) -> Option<&str> {
        self.idle_dim.as_deref().filter(|v| !v.is_empty())
    }

    pub fn idle_restore_token(&self) -> Option<&str> {
        self.idle_restore.as_deref().filter(|v| !v.is_empty())
    }
}

/// `Err((exit_status, text, to_stdout))`.
pub type Outcome = Result<Args, (i32, String, bool)>;

const OPTIONS: [&str; 11] = ["--help", "--popup", "--settings", "--background", "--check", "--brightness-up", "--brightness-down",
                             "--idle-dim", "--idle-restore", "--popup-edge", "--popup-offset"];
const MODES: [&str; 8] = ["--popup", "--settings", "--background", "--check", "--brightness-up", "--brightness-down", "--idle-dim",
                          "--idle-restore"];

pub fn usage(prog: &str) -> String {
    let pad = " ".repeat("usage: ".len() + prog.len() + 1);
    format!("usage: {prog} [-h] [--popup | --settings | --background |\n\
             {pad}--check | --brightness-up | --brightness-down |\n\
             {pad}--idle-dim TOKEN | --idle-restore TOKEN]\n\
             {pad}[--popup-edge {{top,bottom,left,right}}]\n\
             {pad}[--popup-offset POPUP_OFFSET]\n")
}

pub fn help(prog: &str) -> String {
    format!("{}\n{DESCRIPTION}\n\noptions:\n  -h, --help            show this help message and exit\n  --popup\n  --settings\n  \
             --background\n  --check\n  --brightness-up\n  --brightness-down\n  --idle-dim TOKEN\n  --idle-restore TOKEN\n  \
             --popup-edge {{top,bottom,left,right}}\n  --popup-offset POPUP_OFFSET\n", usage(prog))
}

fn error(prog: &str, message: &str) -> (i32, String, bool) {
    (2, format!("{}{prog}: error: {message}\n", usage(prog)), false)
}

fn looks_like_option(value: &str) -> bool {
    if !value.starts_with('-') || value == "-" || value.contains(' ') {
        return false;
    }
    value[1..].parse::<f64>().is_err()
}

/// `int()` of Python: optional sign, surrounding whitespace, underscores between digits.
fn python_int(text: &str) -> Option<i64> {
    let text = text.trim();
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, text.strip_prefix('+').unwrap_or(text)),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__")
        || !digits.chars().all(|c| c.is_ascii_digit() || c == '_') {
        return None;
    }
    digits.replace('_', "").parse::<i64>().ok().map(|v| v * sign)
}

pub fn parse(prog: &str, argv: &[String]) -> Outcome {
    let mut args = Args::default();
    let mut seen_modes: Vec<&'static str> = Vec::new();
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
        let flag = !matches!(option, "--idle-dim" | "--idle-restore" | "--popup-edge" | "--popup-offset");
        let value = if flag {
            if let Some(explicit) = explicit {
                return Err(error(prog, &format!("argument {option}: ignored explicit argument '{explicit}'")));
            }
            None
        } else if let Some(explicit) = explicit {
            Some(explicit)
        } else {
            // Policy tokens are URL-safe base64 and may start with "-": always
            // take the next word (Python argparse would reject such a token).
            match argv.get(index) {
                Some(next) if matches!(option, "--idle-dim" | "--idle-restore") || !looks_like_option(next) => {
                    index += 1;
                    Some(next.clone())
                }
                _ => return Err(error(prog, &format!("argument {option}: expected one argument"))),
            }
        };
        if MODES.contains(&option) {
            if let Some(other) = seen_modes.iter().find(|o| **o != option) {
                return Err(error(prog, &format!("argument {option}: not allowed with argument {other}")));
            }
            if !seen_modes.contains(&option) {
                seen_modes.push(option);
            }
        }
        match option {
            "--help" => return Err((0, help(prog), true)),
            "--popup" => args.popup = true,
            "--settings" => args.settings = true,
            "--background" => args.background = true,
            "--check" => args.check = true,
            "--brightness-up" => args.brightness_up = true,
            "--brightness-down" => args.brightness_down = true,
            "--idle-dim" => args.idle_dim = value,
            "--idle-restore" => args.idle_restore = value,
            "--popup-edge" => {
                let value = value.unwrap();
                if !EDGES.contains(&value.as_str()) {
                    return Err(error(prog, &format!("argument --popup-edge: invalid choice: '{value}' (choose from {})",
                        EDGES.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(", "))));
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
        return Err(error(prog, &tr!("Popup offset must be nonnegative.")));
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(words: &[&str]) -> Outcome {
        parse("brightness_manager.py", &words.iter().map(|w| w.to_string()).collect::<Vec<_>>())
    }

    fn message(words: &[&str]) -> String {
        let (code, text, stdout) = p(words).unwrap_err();
        assert_eq!((code, stdout), (2, false));
        text.lines().last().unwrap().to_owned()
    }

    #[test]
    fn argparse_compatible() {
        assert_eq!(p(&[]).unwrap(), Args::default());
        assert!(p(&["--brightness-u"]).unwrap().brightness_up);
        assert!(p(&["--brightness-up"]).unwrap().launcher());
        assert!(p(&["--check"]).unwrap().launcher());
        assert!(!p(&["--background"]).unwrap().launcher());
        let a = p(&["--idle-dim", "-dash_token"]).unwrap();
        assert_eq!(a.idle_dim_token(), Some("-dash_token"));
        assert!(a.launcher());
        assert!(!p(&["--idle-restore", ""]).unwrap().launcher(), "Python: an empty token is falsy");
        let a = p(&["--popup", "--popup-edge", "top", "--popup-offset=12"]).unwrap();
        assert_eq!((a.popup_edge.as_str(), a.popup_offset), ("top", 12));
        assert_eq!(message(&["--b"]),
                   "brightness_manager.py: error: ambiguous option: --b could match --background, --brightness-up, --brightness-down");
        assert_eq!(message(&["--popup", "--check"]), "brightness_manager.py: error: argument --check: not allowed with argument --popup");
        assert_eq!(message(&["--popup-offset", "-5"]), "brightness_manager.py: error: Popup offset must be nonnegative.");
        assert_eq!(message(&["--popup-edge", "x"]),
                   "brightness_manager.py: error: argument --popup-edge: invalid choice: 'x' (choose from 'top', 'bottom', 'left', 'right')");
        assert_eq!(message(&["--idle-dim"]), "brightness_manager.py: error: argument --idle-dim: expected one argument");
        assert_eq!(message(&["x"]), "brightness_manager.py: error: unrecognized arguments: x");
        assert_eq!(p(&["-h"]).unwrap_err().0, 0);
    }

    #[test]
    fn help_matches_python_layout() {
        let expected = "usage: brightness_manager.py [-h] [--popup | --settings | --background |\n                             --check | --brightness-up | --brightness-down |\n                             --idle-dim TOKEN | --idle-restore TOKEN]\n                             [--popup-edge {top,bottom,left,right}]\n                             [--popup-offset POPUP_OFFSET]\n\nShared monitor controls for Tessel and tessel-displays.\n\noptions:\n  -h, --help            show this help message and exit\n";
        assert!(help("brightness_manager.py").starts_with(expected));
    }
}
