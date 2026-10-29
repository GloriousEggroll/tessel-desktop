//! `argparse` parity for `language_manager.py [-h] [--status] [--quit]`.

pub const DESCRIPTION: &str = "Set the shared desktop and system language with a guided GTK4 wizard.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub status: bool,
    pub quit: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Run(Args),
    Help,
    Error(String),
}

/// `prog`: argparse uses `os.path.basename(sys.argv[0])`.
pub fn prog(argv0: &str) -> String {
    std::path::Path::new(argv0).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| argv0.to_owned())
}

pub fn usage(prog: &str) -> String {
    format!("usage: {prog} [-h] [--status]\n")
}

pub fn help(prog: &str) -> String {
    format!("{}\n{DESCRIPTION}\n\noptions:\n  -h, --help  show this help message and exit\n  --status    print current language settings without changing them\n",
            usage(prog))
}

pub fn error_text(prog: &str, message: &str) -> String {
    format!("{}{prog}: error: {message}\n", usage(prog))
}

const LONG: [(&str, &str); 3] = [("--help", "-h/--help"), ("--status", "--status"), ("--quit", "--quit")];

pub fn parse<S: AsRef<str>>(argv: &[S]) -> Outcome {
    let mut args = Args::default();
    let mut extras: Vec<String> = Vec::new();
    let mut positional_only = false;
    for raw in argv {
        let arg = raw.as_ref();
        if positional_only || arg == "-" || !arg.starts_with('-') {
            extras.push(arg.to_owned());
            continue;
        }
        if arg == "--" {
            positional_only = true;
            continue;
        }
        let (option, explicit) = if arg.starts_with("--") {
            let (name, value) = match arg.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (arg, None),
            };
            let exact = LONG.iter().find(|(long, _)| *long == name);
            let matches: Vec<&(&str, &str)> = LONG.iter().filter(|(long, _)| long.starts_with(name)).collect();
            match (exact, matches.as_slice()) {
                (Some(found), _) | (None, &[found]) => (found.0, value.map(str::to_owned)),
                _ => {
                    extras.push(arg.to_owned());
                    continue;
                }
            }
        } else if let Some(rest) = arg.strip_prefix("-h") {
            ("--help", (!rest.is_empty()).then(|| rest.to_owned()))
        } else {
            extras.push(arg.to_owned());
            continue;
        };
        let display = LONG.iter().find(|(long, _)| *long == option).unwrap().1;
        if let Some(value) = explicit {
            return Outcome::Error(format!("argument {display}: ignored explicit argument '{value}'"));
        }
        match option {
            "--help" => return Outcome::Help,
            "--status" => args.status = true,
            _ => args.quit = true,
        }
    }
    if !extras.is_empty() {
        return Outcome::Error(format!("unrecognized arguments: {}", extras.join(" ")));
    }
    Outcome::Run(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_and_abbreviations() {
        assert_eq!(parse::<&str>(&[]), Outcome::Run(Args::default()));
        assert_eq!(parse(&["--s"]), Outcome::Run(Args { status: true, quit: false }));
        assert_eq!(parse(&["--q"]), Outcome::Run(Args { status: false, quit: true }));
        assert_eq!(parse(&["--status", "--quit"]), Outcome::Run(Args { status: true, quit: true }));
        assert_eq!(parse(&["--h"]), Outcome::Help);
        assert_eq!(parse(&["--bogus", "-h"]), Outcome::Help);
    }

    #[test]
    fn errors_match_argparse() {
        assert_eq!(parse(&["x"]), Outcome::Error("unrecognized arguments: x".into()));
        assert_eq!(parse(&["--bogus", "-x"]), Outcome::Error("unrecognized arguments: --bogus -x".into()));
        assert_eq!(parse(&["--status=1"]), Outcome::Error("argument --status: ignored explicit argument '1'".into()));
        assert_eq!(parse(&["--", "--status"]), Outcome::Error("unrecognized arguments: --status".into()));
    }

    #[test]
    fn help_text_matches_python() {
        assert_eq!(help("language_manager.py"), "usage: language_manager.py [-h] [--status]\n\n\
            Set the shared desktop and system language with a guided GTK4 wizard.\n\n\
            options:\n  -h, --help  show this help message and exit\n  \
            --status    print current language settings without changing them\n");
        assert_eq!(error_text("language_manager.py", "unrecognized arguments: x"),
                   "usage: language_manager.py [-h] [--status]\nlanguage_manager.py: error: unrecognized arguments: x\n");
    }
}
