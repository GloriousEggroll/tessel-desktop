//! text_editor.py's argparse front end: `[-h] [--check] [files ...]`.

pub const DESCRIPTION: &str = "Edit text files in separate windows.";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Check,
    Run(Vec<String>),
    /// argparse error (exit 2): the message after `<prog>: error: `.
    Error(String),
}

pub fn usage(prog: &str) -> String {
    format!("usage: {prog} [-h] [--check] [files ...]\n")
}

pub fn help(prog: &str) -> String {
    format!("{}\n{DESCRIPTION}\n\npositional arguments:\n  files       Files or GIO URIs (sftp://, smb://...) to open\n\n\
options:\n  -h, --help  show this help message and exit\n  --check     Check runtime dependencies without opening a window\n", usage(prog))
}

fn negative_number(arg: &str) -> bool {
    let rest = &arg[1..];
    !rest.is_empty() && (rest.chars().all(|c| c.is_ascii_digit())
        || rest.split_once('.').is_some_and(|(a, b)| a.chars().all(|c| c.is_ascii_digit()) && !b.is_empty() && b.chars().all(|c| c.is_ascii_digit())))
}

/// argparse rules: unique prefixes of `--check`/`--help`, `-h`, `--` ends
/// options, `-` and negative numbers are positional, unknown options are errors.
pub fn parse(args: &[String]) -> Command {
    let (mut check, mut help_flag, mut files, mut unknown) = (false, false, Vec::new(), Vec::new());
    let mut only_files = false;
    for arg in args {
        if only_files || arg == "-" || !arg.starts_with('-') || negative_number(arg) {
            files.push(arg.clone());
            continue;
        }
        if arg == "--" {
            only_files = true;
            continue;
        }
        if arg == "-h" {
            help_flag = true;
            continue;
        }
        if let Some(name) = arg.strip_prefix("--") {
            let (name, value) = match name.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (name, None),
            };
            let matches: Vec<&str> = ["help", "check"].into_iter().filter(|o| o.starts_with(name)).collect();
            if matches.len() > 1 {
                return Command::Error(format!("ambiguous option: --{name} could match --help, --check"));
            }
            match (matches.first(), value) {
                (Some(option), Some(_)) => return Command::Error(format!("argument --{option}: ignored explicit argument '{}'", value.unwrap())),
                (Some(&"help"), None) => help_flag = true,
                (Some(_), None) => check = true,
                (None, _) => unknown.push(arg.clone()),
            }
            continue;
        }
        unknown.push(arg.clone());
    }
    if help_flag {
        return Command::Help;
    }
    if !unknown.is_empty() {
        return Command::Error(format!("unrecognized arguments: {}", unknown.join(" ")));
    }
    if check { Command::Check } else { Command::Run(files) }
}

/// `print(json.dumps(dependencies()))` when everything links (always, for this binary).
pub fn check_json() -> String {
    format!("{{\"ready\": true, \"gtk\": {}, \"gtksourceview\": 5, \"tabs\": false}}", gtk::major_version())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Command {
        parse(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn argparse_rules() {
        assert_eq!(p(&[]), Command::Run(vec![]));
        assert_eq!(p(&["a.txt", "sftp://h/x"]), Command::Run(vec!["a.txt".into(), "sftp://h/x".into()]));
        assert_eq!(p(&["--check"]), Command::Check);
        assert_eq!(p(&["--ch"]), Command::Check);
        assert_eq!(p(&["x", "--he"]), Command::Help);
        assert_eq!(p(&["--", "--check", "-"]), Command::Run(vec!["--check".into(), "-".into()]));
        assert_eq!(p(&["-1", "-"]), Command::Run(vec!["-1".into(), "-".into()]));
        assert_eq!(p(&["--bogus", "-x"]), Command::Error("unrecognized arguments: --bogus -x".into()));
        assert!(help("text-editor").starts_with("usage: text-editor [-h] [--check] [files ...]\n\nEdit text files"));
        assert_eq!(check_json(), "{\"ready\": true, \"gtk\": 4, \"gtksourceview\": 5, \"tabs\": false}");
    }
}
