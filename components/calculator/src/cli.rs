//! calculator.py `arguments()` (argparse) and `check()`.

use crate::backend::MAX_EXPRESSION;

/// argparse's `prog`: the Python launcher runs `calculator.py`.
pub const PROG: &str = "calculator.py";
pub const USAGE: &str = "usage: calculator.py [-h] [--scientific] [--check] [expression]\n";
pub const HELP: &str = "usage: calculator.py [-h] [--scientific] [--check] [expression]

A standalone themed calculator with standard and scientific modes.

positional arguments:
  expression    optional expression to calculate

options:
  -h, --help    show this help message and exit
  --scientific  open scientific mode
  --check       report dependencies without opening a window
";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub expression: Option<String>,
    pub scientific: bool,
    pub check: bool,
    pub quit: bool,
}

/// argparse's SystemExit: `code` plus what it prints (stdout for help, stderr otherwise).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exit {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

fn error(message: &str) -> Exit {
    Exit { code: 2, stdout: String::new(), stderr: format!("{USAGE}{PROG}: error: {message}\n") }
}

const LONG: [&str; 4] = ["--help", "--scientific", "--check", "--quit"];

/// argparse's `_negative_number_matcher`.
fn negative_number(text: &str) -> bool {
    let body = &text[1..];
    let (integer, fraction) = match body.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (body, None),
    };
    match fraction {
        None => !integer.is_empty() && integer.bytes().all(|b| b.is_ascii_digit()),
        Some(f) => integer.bytes().all(|b| b.is_ascii_digit()) && !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()),
    }
}

/// Resolve an option string the way argparse does (exact, then unique `--` prefix).
fn option(text: &str) -> Result<Option<(&'static str, Option<String>)>, Exit> {
    let (name, explicit) = match text.split_once('=') {
        Some((name, value)) if name.starts_with("--") => (name, Some(value.to_string())),
        _ => (text, None),
    };
    if name == "-h" {
        return Ok(Some(("--help", explicit)));
    }
    if let Some(exact) = LONG.iter().find(|o| **o == name) {
        return Ok(Some((exact, explicit)));
    }
    if name.starts_with("--") && name.len() > 2 {
        let matches: Vec<&&str> = LONG.iter().filter(|o| o.starts_with(name)).collect();
        // --quit is SUPPRESSed from help but still a real option.
        match matches.len() {
            1 => return Ok(Some((matches[0], explicit))),
            0 => {}
            _ => {
                let names: Vec<&str> = matches.iter().map(|m| **m).collect();
                return Err(error(&format!("ambiguous option: {name} could match {}", names.join(", "))));
            }
        }
    }
    Ok(None)
}

pub fn arguments(argv: &[String]) -> Result<Options, Exit> {
    let mut options = Options::default();
    let mut positionals: Vec<String> = Vec::new();
    let mut extras: Vec<String> = Vec::new();
    let mut only_positional = false;
    for argument in argv {
        if only_positional || !argument.starts_with('-') || argument == "-" {
            positionals.push(argument.clone());
            continue;
        }
        if argument == "--" {
            only_positional = true;
            continue;
        }
        match option(argument)? {
            Some((name, explicit)) => {
                if let Some(value) = explicit {
                    let shown = if name == "--help" { "-h/--help" } else { name };
                    return Err(error(&format!("argument {shown}: ignored explicit argument '{value}'")));
                }
                match name {
                    "--help" => return Err(Exit { code: 0, stdout: HELP.into(), stderr: String::new() }),
                    "--scientific" => options.scientific = true,
                    "--check" => options.check = true,
                    _ => options.quit = true,
                }
            }
            None if negative_number(argument) || argument.contains(' ') => positionals.push(argument.clone()),
            None => extras.push(argument.clone()),
        }
    }
    let mut rest = positionals.into_iter();
    if let Some(expression) = rest.next() {
        if expression.chars().count() > MAX_EXPRESSION {
            return Err(error("argument expression: the expression is too long"));
        }
        options.expression = Some(expression);
    }
    let mut unrecognized: Vec<String> = extras;
    unrecognized.extend(rest);
    if !unrecognized.is_empty() {
        return Err(error(&format!("unrecognized arguments: {}", unrecognized.join(" "))));
    }
    Ok(options)
}

/// `--check`: `{"ready", "application_id", "gtk4", "layer_shell", "calculator", "error"}`.
pub fn check() -> String {
    let (mut gtk4, mut layer_shell, mut calculator, mut message) = (false, false, false, String::new());
    let handle = unsafe { libc::dlopen(c"libgtk4-layer-shell.so".as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
    if handle.is_null() {
        let text = unsafe { libc::dlerror() };
        message = if text.is_null() { "libgtk4-layer-shell.so: cannot open shared object file".into() }
                  else { unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy().into_owned() };
    } else {
        layer_shell = true;
        gtk4 = gtk::major_version() == 4;
        calculator = crate::backend::evaluate("(2+3)^2", true, &crate::decimal::Decimal::zero())
            .is_ok_and(|v| v == crate::decimal::Decimal::from_i64(25));
    }
    let ready = gtk4 && layer_shell && calculator;
    format!("{{\"ready\": {ready}, \"application_id\": \"{}\", \"gtk4\": {gtk4}, \"layer_shell\": {layer_shell}, \
             \"calculator\": {calculator}, \"error\": {}}}", crate::APPLICATION_ID, crate::compat::json_string(&message))
}
