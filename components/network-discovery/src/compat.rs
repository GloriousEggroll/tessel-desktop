//! Helpers missing from tessel-core (candidates to upstream; the same
//! argparse twin lives in tessel-tiling): Python `repr`/`int()` and an
//! argparse parser for `--mode | … --popup-edge E --popup-offset N`.

use std::path::Path;

pub fn repr_string(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::from(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 32 || c as u32 == 127 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python `int(text)` for decimal strings: surrounding whitespace, a sign,
/// single underscores between digits.
pub fn python_int(text: &str) -> Option<i64> {
    let trimmed = text.trim();
    let (negative, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let mut value: i64 = 0;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        let digit = c.to_digit(10)? as i64;
        value = value.checked_mul(10)?.checked_add(digit)?;
    }
    Some(if negative { -value } else { value })
}

// ---------------------------------------------------------------- argparse

/// One `argparse` parser with a mutually exclusive group of `store_true`
/// modes plus `--popup-edge {top,bottom,left,right}` and `--popup-offset INT`.
pub struct ArgSpec {
    pub description: &'static str,
    pub modes: &'static [&'static str],
}

pub const EDGES: [&str; 4] = ["top", "bottom", "left", "right"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    /// The chosen mode name without `--`.
    pub mode: Option<&'static str>,
    pub popup_edge: String,
    pub popup_offset: i64,
}

impl Default for Parsed {
    fn default() -> Self { Self { mode: None, popup_edge: "bottom".into(), popup_offset: 76 } }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Run(Parsed),
    /// `-h/--help`: print [`ArgSpec::help`] to stdout, exit 0.
    Help,
    /// `parser.error()`: usage + `prog: error: msg` on stderr, exit 2.
    Error(String),
}

// argparse's `_negative_number_matcher`.
fn looks_negative(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('-') else { return false };
    let mut parts = rest.splitn(2, '.');
    let int = parts.next().unwrap_or("");
    let frac = parts.next();
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    match frac {
        None => digits(int),
        Some(frac) => (int.is_empty() || digits(int)) && digits(frac),
    }
}

fn is_option_like(arg: &str) -> bool {
    if !arg.starts_with('-') || arg == "-" || arg == "--" {
        return arg == "--";
    }
    !(looks_negative(arg) && !arg.contains(' '))
}

enum Resolved {
    Exact(String, Option<String>),
    Ambiguous(String),
    Unknown,
}

impl ArgSpec {
    fn options(&self) -> Vec<String> {
        let mut options = vec!["--help".to_owned()];
        options.extend(self.modes.iter().map(|m| format!("--{m}")));
        options.push("--popup-edge".into());
        options.push("--popup-offset".into());
        options
    }

    fn resolve(&self, arg: &str) -> Resolved {
        if arg == "-h" {
            return Resolved::Exact("--help".into(), None);
        }
        let (name, explicit) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (arg, None),
        };
        let options = self.options();
        if options.iter().any(|o| o == name) {
            return Resolved::Exact(name.into(), explicit);
        }
        if !name.starts_with("--") || name.len() < 3 {
            return Resolved::Unknown;
        }
        let matches: Vec<&String> = options.iter().filter(|o| o.starts_with(name)).collect();
        match matches.len() {
            0 => Resolved::Unknown,
            1 => Resolved::Exact(matches[0].clone(), explicit),
            _ => Resolved::Ambiguous(format!("ambiguous option: {name} could match {}",
                                             matches.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "))),
        }
    }

    /// Parse like `parser.parse_args(argv)`. `nonnegative`: the discovery
    /// manager's extra `--popup-offset must be nonnegative` check.
    pub fn parse<S: AsRef<str>>(&self, argv: &[S], nonnegative: bool) -> Outcome {
        let argv: Vec<&str> = argv.iter().map(AsRef::as_ref).collect();
        let mut args = Parsed::default();
        let mut unrecognized: Vec<String> = Vec::new();
        let mut seen_mode: Option<String> = None;
        let mut index = 0;
        let mut positional_only = false;
        while index < argv.len() {
            let arg = argv[index];
            index += 1;
            if positional_only || !is_option_like(arg) {
                unrecognized.push(arg.into());
                continue;
            }
            if arg == "--" {
                positional_only = true;
                unrecognized.push(arg.into());
                continue;
            }
            let (option, explicit) = match self.resolve(arg) {
                Resolved::Exact(option, explicit) => (option, explicit),
                Resolved::Ambiguous(message) => return Outcome::Error(message),
                Resolved::Unknown => {
                    unrecognized.push(arg.into());
                    continue;
                }
            };
            if let Some(mode) = self.modes.iter().find(|m| option[2..] == ***m) {
                if let Some(value) = explicit {
                    return Outcome::Error(format!("argument {option}: ignored explicit argument '{value}'"));
                }
                if let Some(previous) = seen_mode.as_ref().filter(|previous| **previous != option) {
                    return Outcome::Error(format!("argument {option}: not allowed with argument {previous}"));
                }
                seen_mode = Some(option.clone());
                args.mode = Some(mode);
                continue;
            }
            if option == "--help" {
                if let Some(value) = explicit {
                    return Outcome::Error(format!("argument -h/--help: ignored explicit argument '{value}'"));
                }
                return Outcome::Help;
            }
            let value = match explicit {
                Some(value) => value,
                None => match argv.get(index) {
                    Some(next) if !is_option_like(next) => {
                        index += 1;
                        (*next).to_owned()
                    }
                    _ => return Outcome::Error(format!("argument {option}: expected one argument")),
                },
            };
            if option == "--popup-edge" {
                if !EDGES.contains(&value.as_str()) {
                    return Outcome::Error(format!(
                        "argument --popup-edge: invalid choice: {} (choose from 'top', 'bottom', 'left', 'right')",
                        repr_string(&value)));
                }
                args.popup_edge = value;
            } else {
                match python_int(&value) {
                    Some(number) => args.popup_offset = number,
                    None => return Outcome::Error(format!("argument --popup-offset: invalid int value: {}",
                                                          repr_string(&value))),
                }
            }
        }
        if !unrecognized.is_empty() {
            return Outcome::Error(format!("unrecognized arguments: {}", unrecognized.join(" ")));
        }
        if nonnegative && args.popup_offset < 0 {
            return Outcome::Error("--popup-offset must be nonnegative".into());
        }
        Outcome::Run(args)
    }

    /// argparse's usage block (80-column terminal, as when output is piped).
    pub fn usage(&self, prog: &str) -> String {
        let text_width = 78usize;
        let mut parts: Vec<String> = vec!["[-h]".into()];
        for (index, mode) in self.modes.iter().enumerate() {
            if index > 0 {
                parts.push("|".into());
            }
            let mut part = format!("--{mode}");
            if index == 0 {
                part.insert(0, '[');
            }
            if index == self.modes.len() - 1 {
                part.push(']');
            }
            parts.push(part);
        }
        parts.push("[--popup-edge {top,bottom,left,right}]".into());
        parts.push("[--popup-offset POPUP_OFFSET]".into());
        let prefix = "usage: ";
        let full = format!("{prog} {}", parts.join(" "));
        if prefix.len() + full.len() <= text_width {
            return format!("{prefix}{full}\n");
        }
        // argparse wraps the optional groups as units, then the group's own parts.
        let indent = " ".repeat(prefix.len() + prog.len() + 1);
        let mut all = vec![prog.to_owned()];
        all.extend(parts);
        let mut lines: Vec<String> = Vec::new();
        let mut line: Vec<String> = Vec::new();
        let mut line_len = prefix.len() - 1;
        for part in all {
            if line_len + 1 + part.len() > text_width && !line.is_empty() {
                lines.push(format!("{indent}{}", line.join(" ")));
                line.clear();
                line_len = indent.len() - 1;
            }
            line_len += part.len() + 1;
            line.push(part);
        }
        if !line.is_empty() {
            lines.push(format!("{indent}{}", line.join(" ")));
        }
        lines[0] = lines[0][indent.len()..].to_owned();
        format!("{prefix}{}\n", lines.join("\n"))
    }

    pub fn help(&self, prog: &str) -> String {
        let mut text = format!("{}\n{}\n\noptions:\n  -h, --help            show this help message and exit\n",
                               self.usage(prog), self.description);
        for mode in self.modes {
            text.push_str(&format!("  --{mode}\n"));
        }
        text.push_str("  --popup-edge {top,bottom,left,right}\n  --popup-offset POPUP_OFFSET\n");
        text
    }

    pub fn error_text(&self, prog: &str, message: &str) -> String { format!("{}{prog}: error: {message}\n", self.usage(prog)) }
}

/// The program name argparse shows: basename of argv[0].
pub fn prog(argv0: &str) -> String {
    Path::new(argv0).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| argv0.into())
}

