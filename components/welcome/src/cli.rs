//! `welcome.arguments()`: argparse (`allow_abbrev`) with byte-identical texts.
pub const PROG: &str = "welcome.py";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Arguments {
    pub first_run: bool,
    pub page: Option<String>,
}

pub type Exit = (i32, String, String);

pub const USAGE: &str = "usage: welcome.py [-h] [--first-run] [--page PAGE]\n";

pub fn help() -> String {
    format!("{USAGE}\nWelcome: first-run setup for Tessel; reopen it from the application\nmenu.\n\noptions:\n  -h, --help   show this help message and exit\n  --first-run  opened by the first-login setup\n  --page PAGE  start at this step\n")
}

fn error(message: &str) -> Exit { (2, String::new(), format!("{USAGE}{PROG}: error: {message}\n")) }

const LONG: [&str; 3] = ["--help", "--first-run", "--page"];

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
            "--page" => {
                let value = match explicit {
                    Some(value) => value,
                    None => match argv.get(index) {
                        Some(next) if !(next.starts_with('-') && next.len() > 1) => {
                            index += 1;
                            next.clone()
                        }
                        _ => return Err(error("argument --page: expected one argument")),
                    },
                };
                args.page = Some(value);
            }
            flag => {
                if let Some(value) = explicit {
                    let shown = if flag == "--help" { "-h/--help" } else { flag };
                    return Err(error(&format!("argument {shown}: ignored explicit argument '{value}'")));
                }
                if flag == "--help" {
                    return Err((0, help(), String::new()));
                }
                args.first_run = true;
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
    fn prefixes_are_expanded_like_argparse() {
        assert_eq!(parse(&v(&["--f", "--pa", "style"])).unwrap(), Arguments { first_run: true, page: Some("style".into()) });
        assert_eq!(parse(&v(&["--page=done"])).unwrap().page.as_deref(), Some("done"));
        assert_eq!(parse(&v(&["--page", "nonsense"])).unwrap().page.as_deref(), Some("nonsense"));
    }

    /// Byte comparison with argparse output captured from welcome.py inside
    /// the private headless session (tests/fixtures/welcome_capture.out).
    #[test]
    fn texts_match_captured_python_output() {
        let captured = include_str!("../tests/fixtures/welcome_capture.out");
        for block in captured.split("=== ").skip(1) {
            let (header, body) = block.split_once('\n').unwrap();
            let args: Vec<String> = header.split(' ').map(str::to_owned).collect();
            let (text, rc) = body.rsplit_once("rc=").unwrap();
            let (code, out, err) = parse(&args).unwrap_err();
            assert_eq!(rc.trim(), code.to_string(), "{header}");
            assert_eq!(text, format!("{out}{err}"), "{header}");
        }
    }
}
