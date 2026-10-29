//! The subset of `argparse` the installers use: `store_true` flags, an optional mutually
//! exclusive group, `-h/--help`, unique-prefix abbreviations and argparse's error texts.

use std::collections::BTreeSet;

pub struct Parser {
    pub usage: String,
    pub help: String,
    pub flags: &'static [&'static str],
    pub exclusive: bool,
}

pub enum Parsed {
    Flags(BTreeSet<&'static str>),
    Exit(i32),
}

impl Parser {
    fn error(&self, prog: &str, message: &str) -> Parsed {
        eprint!("{}\n{prog}: error: {message}\n", self.usage);
        Parsed::Exit(2)
    }

    pub fn parse(&self, prog: &str, args: &[String]) -> Parsed {
        let mut seen: Vec<&'static str> = Vec::new();
        let mut extras: Vec<String> = Vec::new();
        let mut rest = false;
        for arg in args {
            if rest {
                extras.push(arg.clone());
                continue;
            }
            if arg == "--" {
                rest = true;
                extras.push(arg.clone());
                continue;
            }
            if let Some(body) = arg.strip_prefix("--") {
                let (name, explicit) = match body.split_once('=') {
                    Some((name, value)) => (name, Some(value)),
                    None => (body, None),
                };
                let options: Vec<&'static str> = self.flags.iter().copied().chain(["--help"]).collect();
                let exact = options.iter().find(|option| option[2..] == *name).copied();
                let candidates: Vec<&'static str> = match exact {
                    Some(option) => vec![option],
                    None if !name.is_empty() => options.iter().filter(|o| o[2..].starts_with(name)).copied().collect(),
                    None => Vec::new(),
                };
                match candidates.as_slice() {
                    [option] => {
                        let label = if *option == "--help" { "-h/--help" } else { option };
                        if let Some(value) = explicit {
                            return self.error(prog, &format!("argument {label}: ignored explicit argument '{value}'"));
                        }
                        if *option == "--help" {
                            print!("{}", self.help);
                            return Parsed::Exit(0);
                        }
                        if self.exclusive {
                            if let Some(other) = seen.iter().find(|other| *other != option) {
                                return self.error(prog, &format!("argument {option}: not allowed with argument {other}"));
                            }
                        }
                        seen.push(option);
                    }
                    [] => extras.push(arg.clone()),
                    many => {
                        let names = many.iter().map(|option| match explicit {
                            Some(value) => format!("{option}={value}"),
                            None => option.to_string(),
                        });
                        return self.error(prog, &format!("ambiguous option: {arg} could match {}", names.collect::<Vec<_>>().join(", ")));
                    }
                }
                continue;
            }
            if arg.starts_with("-h") {
                print!("{}", self.help);
                return Parsed::Exit(0);
            }
            extras.push(arg.clone());
        }
        if !extras.is_empty() {
            return self.error(prog, &format!("unrecognized arguments: {}", extras.join(" ")));
        }
        Parsed::Flags(seen.into_iter().collect())
    }
}

/// `sys.argv[0]` basename, which argparse prints as the program name.
pub fn prog(default: &str) -> String {
    std::env::args_os()
        .next()
        .and_then(|arg| std::path::Path::new(&arg).file_name().map(|name| name.to_string_lossy().into_owned()))
        .unwrap_or_else(|| default.to_owned())
}
