//! file_manager.py: argparse front end, `--check`, and the add_help=False parser
//! that `FilesApplication.do_command_line` re-runs in the primary instance.

use gtk::gio;
use gtk::prelude::*;

pub const PROG: &str = "tessel-files";
pub const USAGE: &str = "usage: tessel-files [-h] [--check] [--new-window] [--select] [--split]
                    [--home]
                    [locations ...]
";
/// Usage of the primary's `ArgumentParser(add_help=False)` (no prog given:
/// argparse uses `sys.argv[0]`'s basename).
pub const COMMAND_USAGE: &str = "usage: tessel-files [--new-window] [--select] [--split] [--home]
                    [locations ...]
";
pub const HELP: &str = "usage: tessel-files [-h] [--check] [--new-window] [--select] [--split]
                    [--home]
                    [locations ...]

Browse files.

positional arguments:
  locations

options:
  -h, --help    show this help message and exit
  --check
  --new-window
  --select      Select the supplied files in their parent folders
  --split       Open two folders side by side
  --home        Open your home folder
";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub check: bool,
    pub new_window: bool,
    pub select: bool,
    pub split: bool,
    pub home: bool,
    pub locations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exit {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

fn negative_number(text: &str) -> bool {
    // argparse's _negative_number_matcher: '^-\d+$|^-\d*\.\d+$'
    let body = &text[1..];
    match body.split_once('.') {
        None => !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit()),
        Some((i, f)) => i.bytes().all(|b| b.is_ascii_digit()) && !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()),
    }
}

/// Python argparse semantics for this parser: unique long-option prefixes,
/// `--` ends options, `-h`/`--help` only when `with_help`, `--check` only on
/// the launcher parser; unknown options are collected as "unrecognized".
pub fn parse(argv: &[String], launcher: bool) -> Result<Options, Exit> {
    let usage = if launcher { USAGE } else { COMMAND_USAGE };
    let error = |message: String| Exit { code: 2, stdout: String::new(), stderr: format!("{usage}{PROG}: error: {message}\n") };
    let mut long: Vec<&str> = Vec::new();
    if launcher {
        long.extend(["--help", "--check"]);
    }
    long.extend(["--new-window", "--select", "--split", "--home"]);
    let mut options = Options::default();
    let mut extras: Vec<String> = Vec::new();
    let mut positional_only = false;
    for argument in argv {
        if positional_only || !argument.starts_with('-') || argument == "-" {
            options.locations.push(argument.clone());
            continue;
        }
        if argument == "--" {
            positional_only = true;
            continue;
        }
        if argument.starts_with("--") {
            let (name, explicit) = match argument.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (argument.clone(), None),
            };
            let exact = long.iter().copied().find(|o| *o == name);
            let matches: Vec<&str> = long.iter().copied().filter(|o| o.starts_with(name.as_str())).collect();
            let found = exact.or(if matches.len() == 1 { Some(matches[0]) } else { None });
            match found {
                Some(option) => {
                    if let Some(value) = explicit {
                        let shown = if option == "--help" { "-h/--help" } else { option };
                        return Err(error(format!("argument {shown}: ignored explicit argument '{value}'")));
                    }
                    match option {
                        "--help" => return Err(Exit { code: 0, stdout: HELP.into(), stderr: String::new() }),
                        "--check" => options.check = true,
                        "--new-window" => options.new_window = true,
                        "--select" => options.select = true,
                        "--split" => options.split = true,
                        _ => options.home = true,
                    }
                }
                None if matches.len() > 1 => {
                    return Err(error(format!("ambiguous option: {name} could match {}", matches.join(", "))));
                }
                None if argument.contains(' ') => options.locations.push(argument.clone()),
                None => extras.push(argument.clone()),
            }
            continue;
        }
        if negative_number(argument) || argument.contains(' ') {
            options.locations.push(argument.clone());
            continue;
        }
        if launcher && argument.starts_with("-h") {
            if argument.len() > 2 {
                let value = argument[2..].strip_prefix('=').unwrap_or(&argument[2..]).to_string();
                return Err(error(format!("argument -h/--help: ignored explicit argument '{value}'")));
            }
            return Err(Exit { code: 0, stdout: HELP.into(), stderr: String::new() });
        }
        extras.push(argument.clone());
    }
    if !extras.is_empty() {
        return Err(error(format!("unrecognized arguments: {}", extras.join(" "))));
    }
    Ok(options)
}

/// `json.dumps(dependencies())` for `--check`.
pub fn check_json() -> String {
    let mut schemes: Vec<String> = gio::Vfs::default().supported_uri_schemes().iter().map(|s| s.to_string()).collect();
    schemes.sort();
    let list = schemes.iter().map(|s| tessel_core::pyfmt::json_dumps(&serde_json::Value::String(s.clone()), &tessel_core::pyfmt::JsonStyle::DEFAULT))
        .collect::<Vec<_>>().join(", ");
    format!("{{\"ready\": true, \"gtk\": {}, \"kde_components\": false, \"embedded_terminal\": false, \"uri_schemes\": [{list}]}}",
            gtk::major_version())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flags_prefixes_and_errors() {
        let o = parse(&args(&["--new", "--sel", "--home", "a", "--", "--split"]), true).unwrap();
        assert!(o.new_window && o.select && o.home && !o.split);
        assert_eq!(o.locations, ["a", "--split"]);
        let e = parse(&args(&["--s"]), true).unwrap_err();
        assert_eq!(e.code, 2);
        assert!(e.stderr.ends_with("tessel-files: error: ambiguous option: --s could match --select, --split\n"));
        let e = parse(&args(&["--bogus", "-x"]), true).unwrap_err();
        assert!(e.stderr.ends_with("error: unrecognized arguments: --bogus -x\n"), "{}", e.stderr);
        assert_eq!(parse(&args(&["-h"]), true).unwrap_err().stdout, HELP);
        assert_eq!(parse(&args(&["--he"]), true).unwrap_err().code, 0);
        assert!(parse(&args(&["--check"]), true).unwrap().check);
        // The primary's parser has no help/check.
        assert_eq!(parse(&args(&["--check"]), false).unwrap_err().code, 2);
        assert_eq!(parse(&args(&["-5", "-"]), false).unwrap().locations, ["-5", "-"]);
        assert!(parse(&args(&["--home=1"]), true).unwrap_err().stderr.contains("argument --home: ignored explicit argument '1'"));
    }
}
