//! Command line of notification_manager.py, including argparse's error text,
//! `=` values and unambiguous long-option prefixes.

use crate::client::request;
use glib::prelude::*;
use serde_json::{json, Value};

pub const PROG: &str = "notification_manager.py";
pub const USAGE: &str = "usage: notification_manager.py [-h] [--daemon | --check | --settings | --popup]\n\
                         \x20                              [--popup-edge {top,bottom,left,right}]\n\
                         \x20                              [--popup-offset POPUP_OFFSET]";
const HELP: &str = "\n\nNotification daemon, history popup and settings for the labwc Tessel.\n\n\
options:\n  -h, --help            show this help message and exit\n  --daemon\n  --check\n  --settings\n  --popup\n  \
--popup-edge {top,bottom,left,right}\n  --popup-offset POPUP_OFFSET\n";
const MODES: [&str; 4] = ["--daemon", "--check", "--settings", "--popup"];
const OPTIONS: [&str; 7] = ["--help", "--daemon", "--check", "--settings", "--popup", "--popup-edge", "--popup-offset"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Daemon,
    Check,
    Settings,
    Popup,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    pub mode: Mode,
    pub popup_edge: String,
    pub popup_offset: i64,
}

/// `Err((message, exit code))`: argparse errors exit 2, `--help` exits 0.
pub fn parse(argv: &[String]) -> Result<Args, (String, i32)> {
    let error = |message: String| Err((format!("{USAGE}\n{PROG}: error: {message}"), 2));
    let mut mode: Option<&str> = None;
    let mut edge = "bottom".to_owned();
    let mut offset = 76i64;
    let mut index = 0;
    while index < argv.len() {
        let raw = &argv[index];
        index += 1;
        if raw == "-h" {
            return Err((format!("{USAGE}{HELP}"), 0));
        }
        if !raw.starts_with("--") || raw == "--" {
            if raw == "--" {
                if index < argv.len() {
                    return error(format!("unrecognized arguments: {}", argv[index..].join(" ")));
                }
                continue;
            }
            return error(format!("unrecognized arguments: {raw}"));
        }
        let (name, inline) = match raw.split_once('=') {
            Some((name, value)) => (name.to_owned(), Some(value.to_owned())),
            None => (raw.clone(), None),
        };
        let matches: Vec<&str> = if OPTIONS.contains(&name.as_str()) {
            vec![OPTIONS.iter().find(|o| **o == name).copied().unwrap()]
        } else {
            OPTIONS.iter().copied().filter(|o| o.starts_with(name.as_str())).collect()
        };
        let option = match matches.as_slice() {
            [one] => *one,
            [] => return error(format!("unrecognized arguments: {raw}")),
            many => return error(format!("ambiguous option: {name} could match {}", many.join(", "))),
        };
        match option {
            "--help" => return Err((format!("{USAGE}{HELP}"), 0)),
            "--popup-edge" | "--popup-offset" => {
                let value = match inline {
                    Some(value) => value,
                    None if index < argv.len() && !(argv[index].starts_with('-') && argv[index].len() > 1
                                                     && argv[index].parse::<i64>().is_err()) => {
                        index += 1;
                        argv[index - 1].clone()
                    }
                    None => return error(format!("argument {option}: expected one argument")),
                };
                if option == "--popup-edge" {
                    if !["top", "bottom", "left", "right"].contains(&value.as_str()) {
                        return error(format!("argument --popup-edge: invalid choice: '{value}' (choose from top, bottom, left, right)"));
                    }
                    edge = value;
                } else {
                    match value.trim().replace('_', "").parse::<i64>() {
                        Ok(number) => offset = number,
                        Err(_) => return error(format!("argument --popup-offset: invalid int value: '{value}'")),
                    }
                }
            }
            flag => {
                if inline.is_some() {
                    return error(format!("argument {flag}: ignored explicit argument '{}'", inline.unwrap()));
                }
                if let Some(previous) = mode.filter(|m| *m != flag) {
                    return error(format!("argument {flag}: not allowed with argument {previous}"));
                }
                mode = Some(MODES.iter().copied().find(|m| *m == flag).unwrap());
            }
        }
    }
    let mode = match mode {
        Some("--daemon") => Mode::Daemon,
        Some("--check") => Mode::Check,
        Some("--settings") => Mode::Settings,
        _ => Mode::Popup,
    };
    Ok(Args { mode, popup_edge: edge, popup_offset: offset })
}

/// Python's `str(GLib.Error)`: `<domain>: <message> (<code>)`.
pub fn glib_error_text(error: &glib::Error) -> String {
    use glib::translate::ToGlibPtr;
    let code = unsafe { (*ToGlibPtr::<*const glib::ffi::GError>::to_glib_none(error).0).code };
    format!("{}: {} ({code})", error.domain().as_str(), error.message())
}

pub fn main(argv: Vec<String>) -> i32 {
    let args = match parse(&argv[1.min(argv.len())..]) {
        Ok(args) => args,
        Err((text, code)) => {
            if code == 0 { print!("{text}") } else { eprintln!("{text}") }
            return code;
        }
    };
    if args.mode == Mode::Daemon {
        tessel_core::i18n::initialize();
        return crate::daemon::Daemon::new().run();
    }
    let result: Result<(), String> = (|| {
        match args.mode {
            Mode::Check => {
                let reply = request("GetState", None).map_err(|e| glib_error_text(&e))?;
                let text = reply.child_value(0).get::<String>().unwrap_or_default();
                let state: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                let get = |key: &str, default: Value| state.get(key).cloned().unwrap_or(default);
                let output = json!({"ready": true, "unread": get("unread", json!(0)),
                                    "settings": get("settings", json!({})), "presentation": get("presentation", json!({}))});
                println!("{}", tessel_core::pyfmt::json_dumps(&output, &tessel_core::pyfmt::JsonStyle::DEFAULT));
            }
            Mode::Settings => {
                request("ShowSettings", None).map_err(|e| glib_error_text(&e))?;
            }
            _ => {
                if !(0..=4096).contains(&args.popup_offset) {
                    return Err("\u{0}Popup offset must be between 0 and 4096".into());
                }
                request("ShowCenterAt", Some((args.popup_edge.as_str(), args.popup_offset as u32).to_variant()))
                    .map_err(|e| glib_error_text(&e))?;
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(message) if message.starts_with('\0') => {
            eprintln!("{USAGE}\n{PROG}: error: {}", &message[1..]);
            2
        }
        Err(message) => {
            eprintln!("Notifications: {message}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Args, (String, i32)> {
        parse(&list.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn argparse_compatible_parsing() {
        assert_eq!(args(&[]).unwrap(), Args { mode: Mode::Popup, popup_edge: "bottom".into(), popup_offset: 76 });
        assert_eq!(args(&["--daemon"]).unwrap().mode, Mode::Daemon);
        assert_eq!(args(&["--check"]).unwrap().mode, Mode::Check);
        assert_eq!(args(&["--sett"]).unwrap().mode, Mode::Settings);
        let popup = args(&["--popup", "--popup-edge", "top", "--popup-offset=24"]).unwrap();
        assert_eq!((popup.popup_edge.as_str(), popup.popup_offset), ("top", 24));
        assert_eq!(args(&["--popup-offset", "-5"]).unwrap().popup_offset, -5);
        let (message, code) = args(&["--daemon", "--check"]).unwrap_err();
        assert_eq!(code, 2);
        assert!(message.ends_with("error: argument --check: not allowed with argument --daemon"), "{message}");
        assert!(args(&["--popup-edge", "middle"]).unwrap_err().0.contains("invalid choice: 'middle'"));
        assert!(args(&["--popup-offset", "x"]).unwrap_err().0.contains("invalid int value: 'x'"));
        assert!(args(&["--pop"]).unwrap_err().0.contains("ambiguous option"));
        assert!(args(&["--bogus"]).unwrap_err().0.contains("unrecognized arguments: --bogus"));
        assert_eq!(args(&["--help"]).unwrap_err().1, 0);
    }
}
