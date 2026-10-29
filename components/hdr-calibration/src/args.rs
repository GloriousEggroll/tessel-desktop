//! hdr_calibration.py's argparse parser.

pub const DESCRIPTION: &str = "Desktop HDR calibration: visual setup and recorded meter readings.";

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Args {
    pub output: Option<String>,
    pub sdr_editing: bool,
    pub smoke_test: bool,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Run(Args),
    Help,
    Error(String),
}

pub fn prog(argv0: &str) -> String {
    std::path::Path::new(argv0).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "hdr-calibration".into())
}

pub fn usage(prog: &str) -> String {
    let indent = " ".repeat("usage: ".len() + prog.chars().count() + 1);
    format!("usage: {prog} [-h] [--output OUTPUT] [--sdr-editing]\n{indent}[--smoke-test]\n")
}

pub fn help(prog: &str) -> String {
    format!("{}\n{DESCRIPTION}\n\noptions:\n  -h, --help       show this help message and exit\n  --output OUTPUT\n  --sdr-editing\n  --smoke-test\n", usage(prog))
}

pub fn error_text(prog: &str, message: &str) -> String {
    format!("{}{prog}: error: {message}\n", usage(prog))
}

const OPTIONS: &[&str] = &["--help", "--output", "--sdr-editing", "--smoke-test"];

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
        let candidates: Vec<&&str> = OPTIONS.iter().filter(|o| **o == name || o.starts_with(name)).collect();
        let option = match OPTIONS.iter().find(|o| **o == name).or(if candidates.len() == 1 { Some(candidates[0]) } else { None }) {
            Some(option) => *option,
            None if candidates.len() > 1 => return Outcome::Error(format!("ambiguous option: {name} could match {}",
                candidates.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", "))),
            None => { unknown.push(arg.clone()); continue; }
        };
        match option {
            "--help" => return Outcome::Help,
            "--output" => {
                parsed.output = Some(match inline {
                    Some(value) => value,
                    None if index < args.len() && !args[index].starts_with('-') => { index += 1; args[index - 1].clone() }
                    None => return Outcome::Error("argument --output: expected one argument".into()),
                });
            }
            flag => {
                if let Some(value) = inline {
                    return Outcome::Error(format!("argument {flag}: ignored explicit argument '{value}'"));
                }
                if flag == "--sdr-editing" { parsed.sdr_editing = true } else { parsed.smoke_test = true }
            }
        }
    }
    if !unknown.is_empty() {
        return Outcome::Error(format!("unrecognized arguments: {}", unknown.join(" ")));
    }
    Outcome::Run(parsed)
}
