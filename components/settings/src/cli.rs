//! `global_settings.arguments()`: argparse with `allow_abbrev` (unique long
//! prefixes), byte-identical usage, help and error texts.
use crate::window::{alias, section_keys};

pub const PROG: &str = "global_settings.py";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Arguments {
    pub section: Option<String>,
    pub shortcuts: bool,
    pub quit: bool,
}

/// `Err((exit code, stdout text, stderr text))` = argparse's SystemExit.
pub type Exit = (i32, String, String);

fn choices() -> String { section_keys().join(",") }

pub fn usage() -> String {
    format!("usage: {PROG} [-h]\n                          [--section {{{}}}]\n                          [--shortcuts]\n", choices())
}

pub fn help() -> String {
    format!("{}\nShared GTK4 settings for Tessel; no background service required.\n\noptions:\n  -h, --help            show this help message and exit\n  --section {{{}}}\n  --shortcuts           show or hide the keyboard shortcut overview (Super+/)\n",
            usage(), choices())
}

fn error(message: &str) -> Exit {
    (2, String::new(), format!("{}{PROG}: error: {message}\n", usage()))
}

const LONG: [&str; 4] = ["--help", "--section", "--shortcuts", "--quit"];

fn resolve(option: &str) -> Result<Option<&'static str>, Exit> {
    if let Some(exact) = LONG.iter().find(|o| **o == option) {
        return Ok(Some(exact));
    }
    if !option.starts_with("--") || option.len() < 3 {
        return Ok(None);
    }
    let matches: Vec<&str> = LONG.iter().copied().filter(|o| o.starts_with(option)).collect();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches[0])),
        _ => Err(error(&format!("ambiguous option: {option} could match {}", matches.join(", ")))),
    }
}

pub fn parse(argv: &[String]) -> Result<Arguments, Exit> {
    let mut args = Arguments::default();
    let mut extras: Vec<String> = Vec::new();
    let mut index = 0;
    let mut positional_only = false;
    while index < argv.len() {
        let arg = &argv[index];
        index += 1;
        if positional_only || !arg.starts_with('-') || arg == "-" {
            extras.push(arg.clone());
            continue;
        }
        if arg == "--" {
            positional_only = true;
            continue;
        }
        if arg == "-h" {
            return Err((0, help(), String::new()));
        }
        let (name, explicit) = match arg.split_once('=') {
            Some((name, value)) if arg.starts_with("--") => (name.to_owned(), Some(value.to_owned())),
            _ => (arg.clone(), None),
        };
        let Some(option) = resolve(&name)? else {
            extras.push(arg.clone());
            continue;
        };
        match option {
            "--section" => {
                let value = match explicit {
                    Some(value) => value,
                    None => match argv.get(index) {
                        Some(next) if !(next.starts_with('-') && next.len() > 1) => {
                            index += 1;
                            next.clone()
                        }
                        _ => return Err(error("argument --section: expected one argument")),
                    },
                };
                let value = alias(&value).to_owned();
                if !section_keys().contains(&value.as_str()) {
                    let list = section_keys().iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(", ");
                    return Err(error(&format!("argument --section: invalid choice: '{value}' (choose from {list})")));
                }
                args.section = Some(value);
            }
            flag => {
                if let Some(value) = explicit {
                    let shown = if flag == "--help" { "-h/--help" } else { flag };
                    return Err(error(&format!("argument {shown}: ignored explicit argument '{value}'")));
                }
                match flag {
                    "--help" => return Err((0, help(), String::new())),
                    "--shortcuts" => args.shortcuts = true,
                    _ => args.quit = true,
                }
            }
        }
    }
    if !extras.is_empty() {
        return Err(error(&format!("unrecognized arguments: {}", extras.join(" "))));
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> { items.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn prefixes_aliases_and_errors_match_argparse() {
        assert_eq!(parse(&v(&["--sec", "brightness"])).unwrap().section.as_deref(), Some("displays"));
        assert_eq!(parse(&v(&["--section=dock"])).unwrap().section.as_deref(), Some("dock"));
        assert!(parse(&v(&["--q"])).unwrap().quit);
        assert!(parse(&v(&["--sh"])).unwrap().shortcuts);
        let (code, _, err) = parse(&v(&["--s"])).unwrap_err();
        assert_eq!(code, 2);
        assert!(err.ends_with("global_settings.py: error: ambiguous option: --s could match --section, --shortcuts\n"));
        let (_, _, err) = parse(&v(&["extra"])).unwrap_err();
        assert!(err.ends_with("error: unrecognized arguments: extra\n"));
        let (_, _, err) = parse(&v(&["--section"])).unwrap_err();
        assert!(err.ends_with("error: argument --section: expected one argument\n"));
        let (_, _, err) = parse(&v(&["--quit=1"])).unwrap_err();
        assert!(err.ends_with("error: argument --quit: ignored explicit argument '1'\n"));
        let (code, out, _) = parse(&v(&["--he"])).unwrap_err();
        assert_eq!(code, 0);
        assert!(out.starts_with("usage: global_settings.py [-h]\n"));
    }

    /// Byte comparison with argparse output captured from the Python app
    /// inside the private headless session (tests/fixtures/cli_capture.out).
    #[test]
    fn texts_match_captured_python_output() {
        let captured = include_str!("../tests/fixtures/cli_capture.out");
        let mut sections = captured.split("=== ").skip(1);
        for args in [&["--help"][..], &["--section", "bogus"], &["--s"], &["extra"], &["--section"], &["--quit=1"], &["-x"]] {
            let block = sections.next().unwrap();
            let (header, body) = block.split_once('\n').unwrap();
            assert_eq!(header, args.join(" "));
            let (text, rc) = body.rsplit_once("rc=").unwrap();
            let (code, out, err) = parse(&v(args)).unwrap_err();
            assert_eq!(rc.trim(), code.to_string(), "{header}");
            assert_eq!(text, format!("{out}{err}"), "{header}");
        }
    }
}
