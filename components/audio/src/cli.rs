//! The two argparse command lines (audio_manager.py `arguments`,
//! loopback_service.py `main`), with argparse's prefix matching, messages and
//! exit status 2 on errors.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Default,
    Popup,
    Settings,
    VolumeUp,
    VolumeDown,
    ToggleMute,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    pub mode: Mode,
    pub popup_edge: String,
    pub popup_offset: i64,
}

/// A finished parse that must exit: (status, stdout text, stderr text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exit {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub const APP_USAGE: &str = "usage: audio_manager.py [-h] [--popup | --settings | --volume-up |
                        --volume-down | --toggle-mute]
                        [--popup-edge {top,bottom,left,right}]
                        [--popup-offset POPUP_OFFSET]
";

pub const APP_HELP: &str = "
Native audio settings and a Waybar system-volume popup.

options:
  -h, --help            show this help message and exit
  --popup               open the Waybar volume menu
  --settings            open audio settings (default)
  --volume-up           increase output volume by 5% and show the volume
                        indicator
  --volume-down         decrease output volume by 5% and show the volume
                        indicator
  --toggle-mute         toggle output mute and show the volume indicator
  --popup-edge {top,bottom,left,right}
  --popup-offset POPUP_OFFSET
";

pub const SERVICE_USAGE: &str = "usage: loopback_service.py [-h] [--config CONFIG]\n";

pub const SERVICE_HELP: &str = "
Restore managed PipeWire loopbacks and follow their selected output devices.

options:
  -h, --help       show this help message and exit
  --config CONFIG  Alternative loopback configuration file
";

fn error(usage: &str, prog: &str, message: &str) -> Exit {
    Exit { code: 2, stdout: String::new(), stderr: format!("{usage}{prog}: error: {message}\n") }
}

fn repr(text: &str) -> String {
    if text.contains('\'') && !text.contains('"') { format!("\"{text}\"") } else { format!("'{}'", text.replace('\'', "\\'")) }
}

/// argparse's negative-number pattern `^-\d+$|^-\d*\.\d+$`.
fn negative_number(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('-') else { return false };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(rest) || rest.split_once('.').is_some_and(|(a, b)| (a.is_empty() || digits(a)) && digits(b))
}

struct Spec {
    name: &'static str,
    takes_value: bool,
}

enum Token {
    Option(&'static str, Option<String>),
    Extra(String),
}

/// Resolve one argv into options (exact, then unique prefix) like argparse.
fn tokenize(argv: &[String], specs: &[Spec], usage: &str, prog: &str) -> Result<Vec<(Token, Option<String>)>, Exit> {
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut positional_only = false;
    while index < argv.len() {
        let arg = &argv[index];
        index += 1;
        if positional_only || !arg.starts_with('-') || arg == "-" || negative_number(arg) {
            tokens.push((Token::Extra(arg.clone()), None));
            continue;
        }
        if arg == "--" {
            positional_only = true;
            continue;
        }
        let (flag, explicit) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag.to_owned(), Some(value.to_owned())),
            _ => (arg.clone(), None),
        };
        let spec = match specs.iter().find(|s| s.name == flag || (s.name == "--help" && flag == "-h")) {
            Some(spec) => Some(spec),
            None if flag.starts_with("--") => {
                let matches: Vec<&Spec> = specs.iter().filter(|s| s.name.starts_with(flag.as_str())).collect();
                match matches.len() {
                    0 => None,
                    1 => Some(matches[0]),
                    _ => {
                        let names = matches.iter().map(|s| s.name).collect::<Vec<_>>().join(", ");
                        return Err(error(usage, prog, &format!("ambiguous option: {arg} could match {names}")));
                    }
                }
            }
            None => None,
        };
        let Some(spec) = spec else {
            tokens.push((Token::Extra(arg.clone()), None));
            continue;
        };
        let value = if spec.takes_value {
            match explicit {
                Some(value) => Some(value),
                None => match argv.get(index) {
                    Some(next) if !next.starts_with('-') || negative_number(next) || next == "-" => {
                        index += 1;
                        Some(next.clone())
                    }
                    _ => return Err(error(usage, prog, &format!("argument {}: expected one argument", spec.name))),
                },
            }
        } else {
            if let Some(value) = explicit {
                return Err(error(usage, prog, &format!("argument {}: ignored explicit argument {}", spec.name, repr(&value))));
            }
            None
        };
        tokens.push((Token::Option(spec.name, value), None));
    }
    Ok(tokens)
}

fn extras_error(extras: &[String], usage: &str, prog: &str) -> Result<(), Exit> {
    if extras.is_empty() { Ok(()) } else { Err(error(usage, prog, &format!("unrecognized arguments: {}", extras.join(" ")))) }
}

/// `int(text)` with Python's rules.
pub fn python_int(text: &str) -> Option<i64> {
    tessel_core::pyjson::py_int(Some(&serde_json::Value::String(text.to_owned())))
}

/// `audio_manager.arguments(argv)` (argv without the program name).
pub fn app_arguments(argv: &[String], prog: &str) -> Result<Args, Exit> {
    const MODES: [(&str, Mode); 5] = [("--popup", Mode::Popup), ("--settings", Mode::Settings), ("--volume-up", Mode::VolumeUp),
                                       ("--volume-down", Mode::VolumeDown), ("--toggle-mute", Mode::ToggleMute)];
    let specs = [
        Spec { name: "--help", takes_value: false },
        Spec { name: "--popup", takes_value: false },
        Spec { name: "--settings", takes_value: false },
        Spec { name: "--volume-up", takes_value: false },
        Spec { name: "--volume-down", takes_value: false },
        Spec { name: "--toggle-mute", takes_value: false },
        Spec { name: "--popup-edge", takes_value: true },
        Spec { name: "--popup-offset", takes_value: true },
    ];
    let usage = APP_USAGE;
    let mut args = Args { mode: Mode::Default, popup_edge: "bottom".into(), popup_offset: 76 };
    let mut seen: Option<&str> = None;
    let mut extras = Vec::new();
    for (token, _) in tokenize(argv, &specs, usage, prog)? {
        match token {
            Token::Extra(text) => extras.push(text),
            Token::Option("--help", _) => return Err(Exit { code: 0, stdout: format!("{usage}{APP_HELP}"), stderr: String::new() }),
            Token::Option("--popup-edge", Some(value)) => {
                if !["top", "bottom", "left", "right"].contains(&value.as_str()) {
                    return Err(error(usage, prog, &format!("argument --popup-edge: invalid choice: {} (choose from 'top', 'bottom', 'left', 'right')", repr(&value))));
                }
                args.popup_edge = value;
            }
            Token::Option("--popup-offset", Some(value)) => match python_int(&value) {
                Some(offset) => args.popup_offset = offset,
                None => return Err(error(usage, prog, &format!("argument --popup-offset: invalid int value: {}", repr(&value)))),
            },
            Token::Option(name, _) => {
                if let Some(previous) = seen.filter(|p| *p != name) {
                    return Err(error(usage, prog, &format!("argument {name}: not allowed with argument {previous}")));
                }
                seen = Some(name);
                args.mode = MODES.iter().find(|(n, _)| *n == name).map(|(_, m)| *m).unwrap();
            }
        }
    }
    extras_error(&extras, usage, prog)?;
    if args.popup_offset < 0 {
        return Err(error(usage, prog, "--popup-offset must be nonnegative"));
    }
    Ok(args)
}

/// loopback_service.py `--config PATH`.
pub fn service_arguments(argv: &[String], prog: &str) -> Result<Option<std::path::PathBuf>, Exit> {
    let specs = [Spec { name: "--help", takes_value: false }, Spec { name: "--config", takes_value: true }];
    let mut config = None;
    let mut extras = Vec::new();
    for (token, _) in tokenize(argv, &specs, SERVICE_USAGE, prog)? {
        match token {
            Token::Extra(text) => extras.push(text),
            Token::Option("--help", _) => return Err(Exit { code: 0, stdout: format!("{SERVICE_USAGE}{SERVICE_HELP}"), stderr: String::new() }),
            Token::Option(_, value) => config = value.map(std::path::PathBuf::from),
        }
    }
    extras_error(&extras, SERVICE_USAGE, prog)?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(args: &[&str]) -> Result<Args, Exit> {
        app_arguments(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>(), "audio_manager.py")
    }

    fn last_line(exit: Exit) -> String {
        exit.stderr.lines().last().unwrap().to_owned()
    }

    #[test]
    fn defaults_modes_and_values() {
        assert_eq!(app(&[]).unwrap(), Args { mode: Mode::Default, popup_edge: "bottom".into(), popup_offset: 76 });
        let args = app(&["--popup", "--popup-edge", "left", "--popup-offset=12"]).unwrap();
        assert_eq!((args.mode, args.popup_edge.as_str(), args.popup_offset), (Mode::Popup, "left", 12));
        assert_eq!(app(&["--volume-up"]).unwrap().mode, Mode::VolumeUp);
        assert_eq!(app(&["--volume-d"]).unwrap().mode, Mode::VolumeDown);
        assert_eq!(app(&["--toggle-mute", "--toggle-mute"]).unwrap().mode, Mode::ToggleMute);
        assert_eq!(app(&["--popup-e=top", "--popup"]).unwrap().popup_edge, "top");
        assert_eq!(app(&["--popup-offset", " +7 "]).unwrap().popup_offset, 7);
    }

    #[test]
    fn errors_match_python_argparse() {
        assert_eq!(last_line(app(&["--volume-up", "--popup"]).unwrap_err()), "audio_manager.py: error: argument --popup: not allowed with argument --volume-up");
        assert_eq!(last_line(app(&["--popup", "--volume-up"]).unwrap_err()), "audio_manager.py: error: argument --volume-up: not allowed with argument --popup");
        assert_eq!(last_line(app(&["--popup-edge", "x"]).unwrap_err()), "audio_manager.py: error: argument --popup-edge: invalid choice: 'x' (choose from 'top', 'bottom', 'left', 'right')");
        assert_eq!(last_line(app(&["--popup-offset", "-5"]).unwrap_err()), "audio_manager.py: error: --popup-offset must be nonnegative");
        assert_eq!(last_line(app(&["--popup-offset", "abc"]).unwrap_err()), "audio_manager.py: error: argument --popup-offset: invalid int value: 'abc'");
        assert_eq!(last_line(app(&["--pop"]).unwrap_err()), "audio_manager.py: error: ambiguous option: --pop could match --popup, --popup-edge, --popup-offset");
        assert_eq!(last_line(app(&["extra"]).unwrap_err()), "audio_manager.py: error: unrecognized arguments: extra");
        assert_eq!(last_line(app(&["--popup-offset"]).unwrap_err()), "audio_manager.py: error: argument --popup-offset: expected one argument");
        let exit = app(&["--popup", "--help"]).unwrap_err();
        assert_eq!(exit.code, 0);
        assert!(exit.stdout.starts_with("usage: audio_manager.py [-h] [--popup | --settings"));
        assert_eq!(app(&["--bogus"]).unwrap_err().code, 2);
    }

    #[test]
    fn service_arguments_match_python() {
        let parse = |a: &[&str]| service_arguments(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>(), "loopback_service.py");
        assert_eq!(parse(&[]).unwrap(), None);
        assert_eq!(parse(&["--config", "/x/l.json"]).unwrap(), Some("/x/l.json".into()));
        assert_eq!(parse(&["--con=/y"]).unwrap(), Some("/y".into()));
        assert_eq!(parse(&["--bogus"]).unwrap_err().stderr, "usage: loopback_service.py [-h] [--config CONFIG]\nloopback_service.py: error: unrecognized arguments: --bogus\n");
        assert_eq!(parse(&["--config"]).unwrap_err().stderr, "usage: loopback_service.py [-h] [--config CONFIG]\nloopback_service.py: error: argument --config: expected one argument\n");
        assert_eq!(parse(&["-h"]).unwrap_err().stdout, "usage: loopback_service.py [-h] [--config CONFIG]\n\nRestore managed PipeWire loopbacks and follow their selected output devices.\n\noptions:\n  -h, --help       show this help message and exit\n  --config CONFIG  Alternative loopback configuration file\n");
    }
}
