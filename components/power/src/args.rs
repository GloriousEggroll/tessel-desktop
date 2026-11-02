//! The `power_manager.py` argparse interface: same flags, abbreviations,
//! mutual exclusion, messages and exit status 2 on errors.

use tessel_core::tr;

pub const ACTIONS: [&str; 7] = ["dim", "lock", "display-off", "display-on", "suspend", "hibernate", "suspend-then-hibernate"];
pub const EDGES: [&str; 4] = ["top", "bottom", "left", "right"];
const DESCRIPTION: &str = "Tessel power profiles and persistent session idle management.";

#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    pub popup: bool,
    pub settings: bool,
    pub background: bool,
    pub check: bool,
    pub idle_action: Option<String>,
    pub idle_resume: Option<String>,
    pub power_button: bool,
    pub policy_token: String,
    pub popup_edge: String,
    pub popup_offset: i64,
}

impl Default for Args {
    fn default() -> Self {
        Self { popup: false, settings: false, background: false, check: false, idle_action: None, idle_resume: None,
               power_button: false, policy_token: String::new(), popup_edge: "bottom".into(), popup_offset: 76 }
    }
}

impl Args {
    /// Processes that must never become the primary (IS_LAUNCHER).
    pub fn launcher(&self) -> bool {
        self.check || self.idle_action.is_some() || self.idle_resume.is_some() || self.power_button
    }
}

/// `Err((exit_status, text, to_stdout))`.
pub type Outcome = Result<Args, (i32, String, bool)>;

const OPTIONS: [&str; 11] = ["--help", "--popup", "--settings", "--background", "--check", "--idle-action", "--idle-resume",
                             "--power-button", "--policy-token", "--popup-edge", "--popup-offset"];
const MODES: [&str; 7] = ["--popup", "--settings", "--background", "--check", "--idle-action", "--idle-resume", "--power-button"];

pub fn usage(prog: &str) -> String {
    let choices = ACTIONS.join(",");
    let pad = " ".repeat("usage: ".len() + prog.len() + 1);
    format!("usage: {prog} [-h] [--popup | --settings | --background | --check |\n\
             {pad}--idle-action {{{choices}}} |\n\
             {pad}--idle-resume {{{choices}}} |\n\
             {pad}--power-button] [--policy-token POLICY_TOKEN]\n\
             {pad}[--popup-edge {{top,bottom,left,right}}]\n\
             {pad}[--popup-offset POPUP_OFFSET]\n")
}

pub fn help(prog: &str) -> String {
    let choices = ACTIONS.join(",");
    format!("{}\n{DESCRIPTION}\n\noptions:\n  -h, --help            show this help message and exit\n  --popup\n  --settings\n  \
             --background\n  --check\n  --idle-action {{{choices}}}\n  --idle-resume {{{choices}}}\n  --power-button\n  \
             --policy-token POLICY_TOKEN\n  --popup-edge {{top,bottom,left,right}}\n  --popup-offset POPUP_OFFSET\n", usage(prog))
}

fn error(prog: &str, message: &str) -> (i32, String, bool) {
    (2, format!("{}{prog}: error: {message}\n", usage(prog)), false)
}

fn quoted(choices: &[&str]) -> String {
    choices.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(", ")
}

fn looks_like_option(value: &str) -> bool {
    if !value.starts_with('-') || value == "-" || value.contains(' ') {
        return false;
    }
    // argparse treats negative numbers as values when no option looks like one.
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
        let flag = matches!(option, "--help" | "--popup" | "--settings" | "--background" | "--check" | "--power-button");
        let value = if flag {
            if let Some(explicit) = explicit {
                return Err(error(prog, &format!("argument {option}: ignored explicit argument '{explicit}'")));
            }
            None
        } else if let Some(explicit) = explicit {
            Some(explicit)
        } else {
            // Tokens are URL-safe base64 and may start with "-": always take the next word.
            match argv.get(index) {
                Some(next) if option == "--policy-token" || !looks_like_option(next) => {
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
        let choose = |value: &str, choices: &[&str]| -> Result<String, (i32, String, bool)> {
            if choices.contains(&value) {
                Ok(value.to_owned())
            } else {
                Err(error(prog, &format!("argument {option}: invalid choice: '{value}' (choose from {})", quoted(choices))))
            }
        };
        match option {
            "--help" => return Err((0, help(prog), true)),
            "--popup" => args.popup = true,
            "--settings" => args.settings = true,
            "--background" => args.background = true,
            "--check" => args.check = true,
            "--power-button" => args.power_button = true,
            "--idle-action" => args.idle_action = Some(choose(&value.unwrap(), &ACTIONS)?),
            "--idle-resume" => args.idle_resume = Some(choose(&value.unwrap(), &ACTIONS)?),
            "--policy-token" => args.policy_token = value.unwrap(),
            "--popup-edge" => args.popup_edge = choose(&value.unwrap(), &EDGES)?,
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
    if (args.idle_action.is_some() || args.idle_resume.is_some()) && args.policy_token.is_empty() {
        return Err(error(prog, &tr!("Idle events require --policy-token")));
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(words: &[&str]) -> Outcome {
        parse("power_manager.py", &words.iter().map(|w| w.to_string()).collect::<Vec<_>>())
    }

    fn message(words: &[&str]) -> String {
        let (code, text, stdout) = p(words).unwrap_err();
        assert_eq!((code, stdout), (2, false));
        text.lines().last().unwrap().to_owned()
    }

    #[test]
    fn argparse_compatible() {
        assert_eq!(p(&[]).unwrap(), Args::default());
        assert!(p(&["--power-button"]).unwrap().power_button);
        assert!(p(&["--chec"]).unwrap().check);
        assert!(p(&["--popup", "--popup"]).unwrap().popup);
        let a = p(&["--popup", "--popup-edge", "top", "--popup-offset=12"]).unwrap();
        assert_eq!((a.popup_edge.as_str(), a.popup_offset), ("top", 12));
        let a = p(&["--idle-action", "lock", "--policy-token", "-dashed_token"]).unwrap();
        assert_eq!((a.idle_action.as_deref(), a.policy_token.as_str()), (Some("lock"), "-dashed_token"));
        assert!(a.launcher());
        assert_eq!(message(&["--po"]),
                   "power_manager.py: error: ambiguous option: --po could match --popup, --power-button, --policy-token, --popup-edge, --popup-offset");
        assert_eq!(message(&["--popup", "--settings"]), "power_manager.py: error: argument --settings: not allowed with argument --popup");
        assert_eq!(message(&["--popup-edge", "x"]),
                   "power_manager.py: error: argument --popup-edge: invalid choice: 'x' (choose from 'top', 'bottom', 'left', 'right')");
        assert_eq!(message(&["--popup-offset", "x"]), "power_manager.py: error: argument --popup-offset: invalid int value: 'x'");
        assert_eq!(message(&["--popup-offset", "-5"]), "power_manager.py: error: --popup-offset must be nonnegative");
        assert_eq!(message(&["x"]), "power_manager.py: error: unrecognized arguments: x");
        assert_eq!(message(&["--popup=1"]), "power_manager.py: error: argument --popup: ignored explicit argument '1'");
        assert_eq!(message(&["--policy-token"]), "power_manager.py: error: argument --policy-token: expected one argument");
        assert!(p(&["--help"]).unwrap_err().1.contains("show this help message and exit"));
        assert_eq!(p(&["-h"]).unwrap_err().0, 0);
    }

    #[test]
    fn idle_events_require_policy_token() {
        for switch in ["--idle-action", "--idle-resume"] {
            assert_eq!(message(&[switch, "suspend"]), "power_manager.py: error: Idle events require --policy-token");
        }
        assert!(p(&["--idle-action", "reboot", "--policy-token", "t"]).is_err());
    }

    #[test]
    fn usage_matches_python_layout() {
        let expected = "usage: power_manager.py [-h] [--popup | --settings | --background | --check |\n                        --idle-action {dim,lock,display-off,display-on,suspend,hibernate,suspend-then-hibernate} |\n";
        assert!(usage("power_manager.py").starts_with(expected));
    }
}
