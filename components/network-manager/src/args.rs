//! The `network_manager.py` argparse interface: same flags, abbreviations,
//! mutual exclusion, messages and exit status 2 on errors.

pub const EDGES: [&str; 4] = ["top", "bottom", "left", "right"];
const DESCRIPTION: &str = "NetworkManager controls and authentication for the Waybar desktop.";

#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    pub popup: bool,
    pub settings: bool,
    pub background: bool,
    pub check: bool,
    pub popup_edge: String,
    pub popup_offset: i64,
}

impl Default for Args {
    fn default() -> Self {
        Self { popup: false, settings: false, background: false, check: false, popup_edge: "bottom".into(), popup_offset: 76 }
    }
}

/// `Err((exit_status, text, to_stdout))`.
pub type Outcome = Result<Args, (i32, String, bool)>;

const OPTIONS: [&str; 7] = ["--help", "--popup", "--settings", "--background", "--check", "--popup-edge", "--popup-offset"];
const MODES: [&str; 4] = ["--popup", "--settings", "--background", "--check"];

pub fn usage(prog: &str) -> String {
    let pad = " ".repeat("usage: ".len() + prog.len() + 1);
    format!("usage: {prog} [-h] [--popup | --settings | --background | --check]\n\
             {pad}[--popup-edge {{top,bottom,left,right}}]\n\
             {pad}[--popup-offset POPUP_OFFSET]\n")
}

pub fn help(prog: &str) -> String {
    format!("{}\n{DESCRIPTION}\n\noptions:\n  -h, --help            show this help message and exit\n  --popup\n  --settings\n  \
             --background          start the authentication agent without opening a\n                        window\n  \
             --check               report whether the running backend and authentication\n                        agent are ready\n  \
             --popup-edge {{top,bottom,left,right}}\n  --popup-offset POPUP_OFFSET\n", usage(prog))
}

fn error(prog: &str, message: &str) -> (i32, String, bool) {
    (2, format!("{}{prog}: error: {message}\n", usage(prog)), false)
}

fn looks_like_option(value: &str) -> bool {
    if !value.starts_with('-') || value == "-" || value.contains(' ') {
        return false;
    }
    value[1..].parse::<f64>().is_err()
}

/// Python `int()`: optional sign, surrounding whitespace, underscores between digits.
fn python_int(text: &str) -> Option<i64> {
    let text = text.trim();
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, text.strip_prefix('+').unwrap_or(text)),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__")
        || !digits.chars().all(|c| c.is_ascii_digit() || c == '_') {
        return None;
    }
    digits.replace('_', "").parse::<i64>().ok().map(|v| v * sign)
}

pub fn parse(prog: &str, argv: &[String]) -> Outcome {
    let mut args = Args::default();
    let mut seen: Option<&'static str> = None;
    let mut unrecognized: Vec<String> = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        let word = &argv[index];
        index += 1;
        if word == "--" {
            unrecognized.extend(argv[index..].iter().cloned());
            break;
        }
        if word == "-h" {
            return Err((0, help(prog), true));
        }
        if !word.starts_with("--") || word.len() == 2 {
            unrecognized.push(word.clone());
            continue;
        }
        let (name, explicit) = match word.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (word.as_str(), None),
        };
        let option: &'static str = if let Some(exact) = OPTIONS.iter().find(|o| **o == name) {
            exact
        } else {
            let matches: Vec<&'static str> = OPTIONS.iter().copied().filter(|o| o.starts_with(name)).collect();
            match matches.len() {
                0 => {
                    unrecognized.push(word.clone());
                    continue;
                }
                1 => matches[0],
                _ => return Err(error(prog, &format!("ambiguous option: {name} could match {}", matches.join(", ")))),
            }
        };
        let flag = matches!(option, "--help" | "--popup" | "--settings" | "--background" | "--check");
        let value = if flag {
            if let Some(explicit) = explicit {
                return Err(error(prog, &format!("argument {option}: ignored explicit argument '{explicit}'")));
            }
            None
        } else if let Some(explicit) = explicit {
            Some(explicit)
        } else {
            match argv.get(index) {
                Some(next) if !looks_like_option(next) => {
                    index += 1;
                    Some(next.clone())
                }
                _ => return Err(error(prog, &format!("argument {option}: expected one argument"))),
            }
        };
        if MODES.contains(&option) {
            if let Some(other) = seen.filter(|o| *o != option) {
                return Err(error(prog, &format!("argument {option}: not allowed with argument {other}")));
            }
            seen = Some(option);
        }
        match option {
            "--help" => return Err((0, help(prog), true)),
            "--popup" => args.popup = true,
            "--settings" => args.settings = true,
            "--background" => args.background = true,
            "--check" => args.check = true,
            "--popup-edge" => {
                let value = value.unwrap();
                if !EDGES.contains(&value.as_str()) {
                    return Err(error(prog, &format!("argument --popup-edge: invalid choice: '{value}' (choose from 'top', 'bottom', 'left', 'right')")));
                }
                args.popup_edge = value;
            }
            "--popup-offset" => {
                let value = value.unwrap();
                args.popup_offset = python_int(&value)
                    .ok_or_else(|| error(prog, &format!("argument --popup-offset: invalid int value: '{value}'")))?;
            }
            _ => {}
        }
    }
    if !unrecognized.is_empty() {
        return Err(error(prog, &format!("unrecognized arguments: {}", unrecognized.join(" "))));
    }
    if args.popup_offset < 0 {
        return Err(error(prog, "--popup-offset must be nonnegative"));
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(words: &[&str]) -> Outcome {
        parse("network_manager.py", &words.iter().map(|w| w.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn modes_edges_offsets_and_errors_match_argparse() {
        let args = run(&["--popu", "--popup-e", "top", "--popup-offset=12"]).unwrap_err();
        assert!(args.1.contains("ambiguous option: --popu could match --popup, --popup-edge, --popup-offset"));
        let args = run(&["--popup", "--popup-e", "top", "--popup-o=12"]).unwrap();
        assert!(args.popup);
        assert_eq!((args.popup_edge.as_str(), args.popup_offset), ("top", 12));
        assert_eq!(run(&[]).unwrap(), Args::default());
        let (code, text, stdout) = run(&["--popup", "--settings"]).unwrap_err();
        assert_eq!((code, stdout), (2, false));
        assert_eq!(text, "usage: network_manager.py [-h] [--popup | --settings | --background | --check]\n                          \
                          [--popup-edge {top,bottom,left,right}]\n                          [--popup-offset POPUP_OFFSET]\n\
                          network_manager.py: error: argument --settings: not allowed with argument --popup\n");
        assert!(run(&["--popup-offset", "-1"]).unwrap_err().1.ends_with("error: --popup-offset must be nonnegative\n"));
        assert!(run(&["--popup-edge", "middle"]).unwrap_err().1.contains("invalid choice: 'middle'"));
        assert!(run(&["--popup-offset", "x"]).unwrap_err().1.contains("invalid int value: 'x'"));
        assert!(run(&["--bogus"]).unwrap_err().1.ends_with("unrecognized arguments: --bogus\n"));
        let (code, text, stdout) = run(&["--help"]).unwrap_err();
        assert_eq!((code, stdout), (0, true));
        assert!(text.contains("--background          start the authentication agent without opening a\n                        window\n"));
        assert!(run(&["--check"]).unwrap().check);
    }
}
