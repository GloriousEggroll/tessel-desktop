//! system_monitor.py's argparse parser (prefix abbreviations, `--opt=value`).

pub const PAGES: &[&str] = &["processes", "performance", "storage", "network", "gpu", "system"];
pub const DESCRIPTION: &str = "Standalone GTK4 process and performance monitor.";

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Args {
    pub check: bool,
    pub page: Option<String>,
    pub quit: bool,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Run(Args),
    Help,
    Error(String),
}

/// argparse `prog`: the basename of argv[0].
pub fn prog(argv0: &str) -> String {
    std::path::Path::new(argv0).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "tessel-system-monitor".into())
}

pub fn usage(prog: &str) -> String {
    let indent = " ".repeat("usage: ".len() + prog.chars().count() + 1);
    format!("usage: {prog} [-h] [--check]\n{indent}[--page {{{}}}]\n", PAGES.join(","))
}

pub fn help(prog: &str) -> String {
    format!("{}\n{DESCRIPTION}\n\noptions:\n  -h, --help            show this help message and exit\n  --check\n  --page {{{}}}\n",
            usage(prog), PAGES.join(","))
}

pub fn error_text(prog: &str, message: &str) -> String {
    format!("{}{prog}: error: {message}\n", usage(prog))
}

const OPTIONS: &[&str] = &["--help", "--check", "--page", "--quit"];

fn resolve(name: &str) -> Result<Option<&'static str>, String> {
    if let Some(exact) = OPTIONS.iter().find(|o| **o == name) {
        return Ok(Some(exact));
    }
    let matches: Vec<&&str> = OPTIONS.iter().filter(|o| o.starts_with(name)).collect();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches[0])),
        _ => Err(format!("ambiguous option: {name} could match {}",
                         matches.iter().map(|m| m.to_string()).collect::<Vec<_>>().join(", "))),
    }
}

pub fn parse(args: &[String]) -> Outcome {
    let mut parsed = Args::default();
    let mut unknown = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        index += 1;
        if arg == "-h" {
            return Outcome::Help;
        }
        if !arg.starts_with("--") || arg == "--" {
            unknown.push(arg.clone());
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        let option = match resolve(name) {
            Ok(Some(option)) => option,
            Ok(None) => { unknown.push(arg.clone()); continue; }
            Err(message) => return Outcome::Error(message),
        };
        match option {
            "--help" => return Outcome::Help,
            "--page" => {
                let value = match inline {
                    Some(value) => value,
                    None if index < args.len() && !args[index].starts_with('-') => { index += 1; args[index - 1].clone() }
                    None => return Outcome::Error("argument --page: expected one argument".into()),
                };
                if !PAGES.contains(&value.as_str()) {
                    let choices: Vec<String> = PAGES.iter().map(|p| format!("'{p}'")).collect();
                    return Outcome::Error(format!("argument --page: invalid choice: '{value}' (choose from {})", choices.join(", ")));
                }
                parsed.page = Some(value);
            }
            flag => {
                if inline.is_some() {
                    return Outcome::Error(format!("argument {flag}: ignored explicit argument '{}'", inline.unwrap()));
                }
                if flag == "--check" { parsed.check = true } else { parsed.quit = true }
            }
        }
    }
    if !unknown.is_empty() {
        return Outcome::Error(format!("unrecognized arguments: {}", unknown.join(" ")));
    }
    Outcome::Run(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> { list.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn argparse_texts_match_python() {
        assert_eq!(help("system_monitor.py"), "usage: system_monitor.py [-h] [--check]\n                         [--page {processes,performance,storage,network,gpu,system}]\n\nStandalone GTK4 process and performance monitor.\n\noptions:\n  -h, --help            show this help message and exit\n  --check\n  --page {processes,performance,storage,network,gpu,system}\n");
        assert_eq!(parse(&args(&["--page", "bogus"])), Outcome::Error("argument --page: invalid choice: 'bogus' (choose from 'processes', 'performance', 'storage', 'network', 'gpu', 'system')".into()));
        assert_eq!(parse(&args(&["--nope"])), Outcome::Error("unrecognized arguments: --nope".into()));
        assert_eq!(parse(&args(&["--page"])), Outcome::Error("argument --page: expected one argument".into()));
        assert_eq!(parse(&args(&["--pa=gpu", "--ch"])), Outcome::Run(Args { check: true, page: Some("gpu".into()), quit: false }));
        assert_eq!(parse(&args(&["--quit"])), Outcome::Run(Args { quit: true, ..Default::default() }));
        assert_eq!(parse(&args(&["--help"])), Outcome::Help);
    }
}
