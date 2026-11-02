//! `battery_manager.arguments()`: Python argparse semantics for the launcher
//! flags (mutually exclusive modes, prefix abbreviations, `--opt=value`, the
//! exact usage/error text and exit codes: help 0, errors 2).

use tessel_core::tr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// No mode flag: primary activation shows the popup.
    Default,
    Popup,
    Settings,
    Background,
    Check,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    pub mode: Mode,
    pub popup_edge: String,
    pub popup_offset: i64,
}

impl Args {
    pub fn check(&self) -> bool {
        self.mode == Mode::Check
    }
}

/// `SystemExit`: text for stdout (help) or stderr (errors) and the exit code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exit {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub const DESCRIPTION: &str = "Independent battery status and charging controls for Waybar.";
const EDGES: [&str; 4] = ["top", "bottom", "left", "right"];
const OPTIONS: [&str; 7] = ["--help", "--popup", "--settings", "--background", "--check", "--popup-edge", "--popup-offset"];

pub fn usage(prog: &str) -> String {
    let indent = " ".repeat(prog.len() + 8);
    format!("usage: {prog} [-h] [--popup | --settings | --background | --check]\n{indent}[--popup-edge {{top,bottom,left,right}}]\n{indent}[--popup-offset POPUP_OFFSET]\n")
}

pub fn help(prog: &str) -> String {
    format!("{}\n{DESCRIPTION}\n\noptions:\n  -h, --help            show this help message and exit\n  --popup\n  --settings\n  --background\n  --check\n  --popup-edge {{top,bottom,left,right}}\n  --popup-offset POPUP_OFFSET\n",
            usage(prog))
}

fn error(prog: &str, message: &str) -> Exit {
    Exit { code: 2, stdout: String::new(), stderr: format!("{}{prog}: error: {message}\n", usage(prog)) }
}

/// Python `int(text)` (whitespace, sign, `_` separators).
fn python_int(text: &str) -> Option<i64> {
    tessel_power_core::charge_limits::parse_int(text)
}

/// argparse's test for "looks like an option, not a value" (no option looks like a negative number).
fn is_option_like(token: &str) -> bool {
    if !token.starts_with('-') || token == "-" {
        return false;
    }
    let negative_number = {
        let rest = &token[1..];
        (!rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
            || rest.split_once('.').is_some_and(|(a, b)| a.chars().all(|c| c.is_ascii_digit()) && !b.is_empty() && b.chars().all(|c| c.is_ascii_digit()))
    };
    !negative_number
}

pub fn parse(prog: &str, argv: &[String]) -> Result<Args, Exit> {
    let mut args = Args { mode: Mode::Default, popup_edge: "bottom".into(), popup_offset: 76 };
    let mut mode_flag: Option<&'static str> = None;
    let mut unrecognized: Vec<String> = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        let token = &argv[index];
        index += 1;
        if token == "--" {
            unrecognized.extend(argv[index - 1..].iter().cloned());
            break;
        }
        if !is_option_like(token) {
            unrecognized.push(token.clone());
            continue;
        }
        let (name, explicit) = match token.split_once('=') {
            Some((name, value)) if token.starts_with("--") => (name.to_owned(), Some(value.to_owned())),
            _ => (token.clone(), None),
        };
        let option = if name == "-h" {
            "--help"
        } else if !name.starts_with("--") {
            unrecognized.push(token.clone());
            continue;
        } else if let Some(exact) = OPTIONS.iter().find(|o| **o == name) {
            exact
        } else {
            let matches: Vec<&str> = OPTIONS.iter().copied().filter(|o| o.starts_with(name.as_str())).collect();
            match matches.len() {
                0 => {
                    unrecognized.push(token.clone());
                    continue;
                }
                1 => matches[0],
                _ => return Err(error(prog, &format!("ambiguous option: {token} could match {}", matches.join(", ")))),
            }
        };
        match option {
            "--help" | "--popup" | "--settings" | "--background" | "--check" => {
                if let Some(value) = explicit {
                    return Err(error(prog, &format!("argument {option}: ignored explicit argument '{value}'")));
                }
                if option == "--help" {
                    return Err(Exit { code: 0, stdout: help(prog), stderr: String::new() });
                }
                if let Some(previous) = mode_flag.filter(|previous| *previous != option) {
                    return Err(error(prog, &format!("argument {option}: not allowed with argument {previous}")));
                }
                mode_flag = Some(option);
                args.mode = match option {
                    "--popup" => Mode::Popup,
                    "--settings" => Mode::Settings,
                    "--background" => Mode::Background,
                    _ => Mode::Check,
                };
            }
            _ => {
                let value = match explicit {
                    Some(value) => value,
                    None if index < argv.len() && !is_option_like(&argv[index]) => {
                        index += 1;
                        argv[index - 1].clone()
                    }
                    None => return Err(error(prog, &format!("argument {option}: expected one argument"))),
                };
                if option == "--popup-edge" {
                    if !EDGES.contains(&value.as_str()) {
                        return Err(error(prog, &format!("argument --popup-edge: invalid choice: '{value}' (choose from 'top', 'bottom', 'left', 'right')")));
                    }
                    args.popup_edge = value;
                } else {
                    args.popup_offset = python_int(&value)
                        .ok_or_else(|| error(prog, &format!("argument --popup-offset: invalid int value: '{value}'")))?;
                }
            }
        }
    }
    if !unrecognized.is_empty() {
        return Err(error(prog, &format!("unrecognized arguments: {}", unrecognized.join(" "))));
    }
    if args.popup_offset < 0 {
        return Err(error(prog, &tr!("Popup offset must be nonnegative")));
    }
    Ok(args)
}

/// `basename(argv[0])` like Python's default `prog`.
pub fn prog(argv0: Option<&str>) -> String {
    argv0.and_then(|a| std::path::Path::new(a).file_name()).map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tessel-battery".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(argv: &[&str]) -> Result<Args, Exit> {
        parse("battery_manager.py", &argv.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    fn stderr(argv: &[&str]) -> String {
        let exit = run(argv).unwrap_err();
        assert_eq!(exit.code, 2, "{argv:?}");
        exit.stderr.lines().last().unwrap().to_owned()
    }

    #[test]
    fn launcher_routes_modes_and_positions() {
        let args = run(&["--popup", "--popup-edge", "top", "--popup-offset", "30"]).unwrap();
        assert_eq!((args.mode, args.popup_edge.as_str(), args.popup_offset), (Mode::Popup, "top", 30));
        assert_eq!(run(&["--settings"]).unwrap().mode, Mode::Settings);
        assert_eq!(run(&["--background"]).unwrap().mode, Mode::Background);
        assert_eq!(run(&[]).unwrap(), Args { mode: Mode::Default, popup_edge: "bottom".into(), popup_offset: 76 });
        assert!(run(&["--check", "--check"]).unwrap().check());
        assert_eq!(run(&["--set"]).unwrap().mode, Mode::Settings);
        assert_eq!(run(&["--popup-o", "7"]).unwrap().popup_offset, 7);
        assert_eq!(run(&["--popup-offset=5", "--check"]).unwrap().popup_offset, 5);
        assert_eq!(run(&["--popup-edge=left"]).unwrap().popup_edge, "left");
        for (text, value) in [(" 12 ", 12), ("+5", 5), ("1_0", 10), ("0", 0)] {
            assert_eq!(run(&["--popup-offset", text]).unwrap().popup_offset, value);
        }
    }

    #[test]
    fn launcher_rejects_ambiguous_modes_and_invalid_position() {
        // Exact argparse (Python 3.14) messages.
        assert_eq!(stderr(&["--popup", "--settings"]), "battery_manager.py: error: argument --settings: not allowed with argument --popup");
        assert_eq!(stderr(&["--popup-offset", "-1"]), "battery_manager.py: error: Popup offset must be nonnegative");
        assert_eq!(stderr(&["--popup-edge", "diagonal"]),
                   "battery_manager.py: error: argument --popup-edge: invalid choice: 'diagonal' (choose from 'top', 'bottom', 'left', 'right')");
        assert_eq!(stderr(&["--popup-offset", "x"]), "battery_manager.py: error: argument --popup-offset: invalid int value: 'x'");
        assert_eq!(stderr(&["--popup-offset", "1e3"]), "battery_manager.py: error: argument --popup-offset: invalid int value: '1e3'");
        assert_eq!(stderr(&["x"]), "battery_manager.py: error: unrecognized arguments: x");
        assert_eq!(stderr(&["--bogus"]), "battery_manager.py: error: unrecognized arguments: --bogus");
        assert_eq!(stderr(&["--pop"]), "battery_manager.py: error: ambiguous option: --pop could match --popup, --popup-edge, --popup-offset");
        assert_eq!(stderr(&["--popup-edge"]), "battery_manager.py: error: argument --popup-edge: expected one argument");
        assert_eq!(stderr(&["--popup-edge", "--popup"]), "battery_manager.py: error: argument --popup-edge: expected one argument");
        assert_eq!(stderr(&["--settings=1"]), "battery_manager.py: error: argument --settings: ignored explicit argument '1'");
        assert_eq!(stderr(&["--", "--popup"]), "battery_manager.py: error: unrecognized arguments: -- --popup");
        assert!(run(&["--popup", "--settings"]).unwrap_err().stderr.starts_with(&usage("battery_manager.py")));
    }

    #[test]
    fn help_matches_argparse() {
        let exit = run(&["-h"]).unwrap_err();
        assert_eq!(exit.code, 0);
        assert_eq!(exit.stdout, "usage: battery_manager.py [-h] [--popup | --settings | --background | --check]
                          [--popup-edge {top,bottom,left,right}]
                          [--popup-offset POPUP_OFFSET]

Independent battery status and charging controls for Waybar.

options:
  -h, --help            show this help message and exit
  --popup
  --settings
  --background
  --check
  --popup-edge {top,bottom,left,right}
  --popup-offset POPUP_OFFSET
");
        assert_eq!(run(&["--he"]).unwrap_err().code, 0);
    }
}
