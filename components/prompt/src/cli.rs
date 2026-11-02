//! The argparse grammar of wayshell_prompt.py `arguments()` (Python 3.14
//! argparse semantics: abbreviations, `=` values, the required mutually
//! exclusive group, left-to-right errors, exit status 2 / help exit 0).

use tessel_core::tr;

/// Parsed options (Python `argparse.Namespace`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub message: Option<String>,
    pub message_stdin: bool,
    pub error: bool,
    pub accept_label: String,
    pub decline_label: String,
    pub destructive: bool,
}

/// Result of parsing: run with options, or exit (help / usage error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Run(Options),
    Exit { code: i32, stdout: String, stderr: String },
}

const DESCRIPTION: &str = "On-demand GTK4 confirmations and configuration errors for labwc.

labwc's action protocol is 0=yes, 254=dismiss (run neither branch), and any
other status=no. This helper displays text only; it never executes an action.
";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Act {
    Help,
    Message,
    MessageStdin,
    Error,
    Accept,
    Decline,
    Destructive,
}

impl Act {
    fn takes_value(self) -> bool {
        matches!(self, Act::Message | Act::Accept | Act::Decline)
    }
    fn name(self) -> &'static str {
        match self {
            Act::Help => "-h/--help",
            Act::Message => "--message",
            Act::MessageStdin => "--message-stdin",
            Act::Error => "--error",
            Act::Accept => "--accept-label",
            Act::Decline => "--decline-label",
            Act::Destructive => "--destructive",
        }
    }
    fn group(self) -> bool {
        matches!(self, Act::Message | Act::MessageStdin | Act::Error)
    }
}

/// `parser._option_string_actions` in registration order.
const OPTION_STRINGS: [(&str, Act); 8] = [
    ("-h", Act::Help), ("--help", Act::Help), ("--message", Act::Message), ("--message-stdin", Act::MessageStdin),
    ("--error", Act::Error), ("--accept-label", Act::Accept), ("--decline-label", Act::Decline),
    ("--destructive", Act::Destructive),
];

fn lookup(option: &str) -> Option<Act> {
    OPTION_STRINGS.iter().find(|(name, _)| *name == option).map(|(_, act)| *act)
}

/// One classified argument: `None` = positional ('A'), else an option tuple.
#[derive(Clone, Debug)]
enum Class {
    Positional,
    DoubleDash,
    Option { act: Option<Act>, option: String, sep: bool, explicit: Option<String> },
}

/// Python `repr(str)` for the short ASCII strings argparse quotes.
fn py_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
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
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `_negative_number_matcher = re.compile(r'-\.?\d')` (match at start).
fn looks_negative(arg: &str) -> bool {
    static NEGATIVE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    NEGATIVE.get_or_init(|| regex::Regex::new(r"^-\.?\d").unwrap()).is_match(arg)
}

/// `_parse_optional` + `_get_option_tuples`; `Err` = ambiguous option.
fn classify(arg: &str) -> Result<Class, String> {
    if !arg.starts_with('-') {
        return Ok(Class::Positional);
    }
    if let Some(act) = lookup(arg) {
        return Ok(Class::Option { act: Some(act), option: arg.into(), sep: false, explicit: None });
    }
    if arg.chars().count() == 1 {
        return Ok(Class::Positional);
    }
    if let Some((option, explicit)) = arg.split_once('=') {
        if let Some(act) = lookup(option) {
            return Ok(Class::Option { act: Some(act), option: option.into(), sep: true, explicit: Some(explicit.into()) });
        }
    }
    let mut tuples: Vec<(Act, &str, bool, Option<String>)> = Vec::new();
    let (prefix, explicit) = match arg.split_once('=') {
        Some((prefix, explicit)) => (prefix, Some(explicit.to_owned())),
        None => (arg, None),
    };
    let double = arg[1..].starts_with('-');
    if double {
        for (name, act) in OPTION_STRINGS {
            if name.starts_with(prefix) {
                tuples.push((act, name, explicit.is_some(), explicit.clone()));
            }
        }
    } else {
        let short_end = arg.char_indices().nth(2).map_or(arg.len(), |(i, _)| i);
        let (short_prefix, short_explicit) = arg.split_at(short_end);
        for (name, act) in OPTION_STRINGS {
            if name == short_prefix {
                tuples.push((act, name, false, Some(short_explicit.to_owned())));
            } else if name.starts_with(prefix) {
                tuples.push((act, name, explicit.is_some(), explicit.clone()));
            }
        }
    }
    if tuples.len() > 1 {
        let names: Vec<&str> = tuples.iter().map(|t| t.1).collect();
        return Err(format!("ambiguous option: {arg} could match {}", names.join(", ")));
    }
    if let Some((act, name, sep, explicit)) = tuples.pop() {
        return Ok(Class::Option { act: Some(act), option: name.into(), sep, explicit });
    }
    if looks_negative(arg) || arg.contains(' ') {
        return Ok(Class::Positional);
    }
    Ok(Class::Option { act: None, option: arg.into(), sep: false, explicit: None })
}

/// `shutil.get_terminal_size().columns` (COLUMNS, then stdout, then 80).
fn terminal_columns() -> usize {
    if let Some(columns) = std::env::var("COLUMNS").ok().and_then(|v| v.trim().parse::<usize>().ok()).filter(|c| *c > 0) {
        return columns;
    }
    // SAFETY: plain TIOCGWINSZ ioctl into a zeroed struct.
    unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut size) == 0 && size.ws_col > 0 {
            return size.ws_col as usize;
        }
    }
    80
}

/// textwrap chunks: whitespace-separated words, hyphenated words split after
/// `-`; the flag says whether a space preceded the chunk.
fn chunks(text: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for (index, word) in text.split_whitespace().enumerate() {
        let chars: Vec<char> = word.chars().collect();
        let mut start = 0;
        for i in 1..chars.len().saturating_sub(1) {
            if chars[i] == '-' && chars[i - 1].is_alphanumeric() && chars[i + 1].is_alphanumeric() {
                out.push((chars[start..=i].iter().collect(), start == 0 && index > 0));
                start = i + 1;
            }
        }
        out.push((chars[start..].iter().collect(), start == 0 && index > 0));
    }
    out
}

/// Greedy `textwrap.wrap` after argparse's whitespace collapsing.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for (piece, space) in chunks(text) {
        let addition = piece.chars().count() + usize::from(space && !line.is_empty());
        if !line.is_empty() && line.chars().count() + addition > width {
            lines.push(std::mem::take(&mut line));
        }
        if space && !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&piece);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

const USAGE_PARTS: [&str; 5] = ["[-h]", "(--message MESSAGE | --message-stdin | --error)", "[--accept-label ACCEPT_LABEL]",
                                "[--decline-label DECLINE_LABEL]", "[--destructive]"];

fn usage_body(prog: &str, width: usize) -> String {
    let prefix = "usage: ";
    let flat = format!("{prog} {}", USAGE_PARTS.join(" "));
    let text_width = width;
    if prefix.len() + flat.chars().count() <= text_width {
        return flat;
    }
    let get_lines = |parts: &[&str], indent: &str, prefix: Option<&str>| -> Vec<String> {
        let mut lines = Vec::new();
        let mut line: Vec<&str> = Vec::new();
        let indent_length = indent.len();
        let mut line_len = match prefix { Some(p) => p.len() as isize - 1, None => indent_length as isize - 1 };
        for part in parts {
            let len = part.chars().count() as isize;
            if line_len + 1 + len > text_width as isize && !line.is_empty() {
                lines.push(format!("{indent}{}", line.join(" ")));
                line.clear();
                line_len = indent_length as isize - 1;
            }
            line.push(part);
            line_len += len + 1;
        }
        if !line.is_empty() {
            lines.push(format!("{indent}{}", line.join(" ")));
        }
        if prefix.is_some() {
            lines[0] = lines[0][indent.len()..].to_owned();
        }
        lines
    };
    let prog_len = prog.chars().count();
    let lines = if (prefix.len() + prog_len) as f64 <= 0.75 * text_width as f64 {
        let indent = " ".repeat(prefix.len() + prog_len + 1);
        let mut parts = vec![prog];
        parts.extend(USAGE_PARTS);
        get_lines(&parts, &indent, Some(prefix))
    } else {
        let indent = " ".repeat(prefix.len());
        let mut lines = vec![prog.to_owned()];
        lines.extend(get_lines(&USAGE_PARTS, &indent, None));
        lines
    };
    lines.join("\n")
}

/// `parser.format_usage()`.
pub fn usage(prog: &str) -> String {
    usage_with_width(prog, terminal_columns().saturating_sub(2))
}

fn usage_with_width(prog: &str, width: usize) -> String {
    format!("usage: {}\n", usage_body(prog, width))
}

/// `parser.format_help()`.
pub fn help(prog: &str) -> String {
    help_with_width(prog, terminal_columns().saturating_sub(2))
}

pub fn help_with_width(prog: &str, width: usize) -> String {
    let mut out = format!("usage: {}\n\n", usage_body(prog, width));
    out += &wrap(DESCRIPTION, width.max(11)).join("\n");
    out += "\n\noptions:\n";
    let max_help_position = 24.min((width as isize - 20).max(4) as usize);
    let entries: [(&str, &str); 7] = [
        ("-h, --help", "show this help message and exit"),
        ("--message MESSAGE", ""), ("--message-stdin", ""), ("--error", ""),
        ("--accept-label ACCEPT_LABEL", ""), ("--decline-label DECLINE_LABEL", ""),
        ("--destructive", "Style the accept button as destructive; it is then not the default"),
    ];
    let action_max_length = entries.iter().map(|(inv, _)| inv.len() + 2).max().unwrap();
    let help_position = (action_max_length + 2).min(max_help_position);
    let help_width = (width as isize - help_position as isize).max(11) as usize;
    let action_width = help_position as isize - 2 - 2;
    for (invocation, text) in entries {
        if text.is_empty() {
            out += &format!("  {invocation}\n");
            continue;
        }
        let lines = wrap(text, help_width);
        if invocation.len() as isize <= action_width {
            out += &format!("  {invocation:<width$}  {}\n", lines[0], width = action_width as usize);
        } else {
            out += &format!("  {invocation}\n{}{}\n", " ".repeat(help_position), lines[0]);
        }
        for line in &lines[1..] {
            out += &format!("{}{line}\n", " ".repeat(help_position));
        }
    }
    out
}

/// `parser.parse_args(args)` with `prog` as `%(prog)s`.
pub fn parse(prog: &str, args: &[String]) -> Parsed {
    parse_with_width(prog, args, terminal_columns().saturating_sub(2))
}

/// `parse` with an explicit formatter width (terminal columns − 2).
pub fn parse_with_width(prog: &str, args: &[String], width: usize) -> Parsed {
    let error = |message: String| Parsed::Exit {
        code: 2, stdout: String::new(), stderr: format!("{}{prog}: error: {message}\n", usage_with_width(prog, width)),
    };
    // Classification pass (ambiguity errors come first, as in argparse).
    let mut classes = Vec::with_capacity(args.len());
    let mut after_dashes = false;
    for arg in args {
        if after_dashes {
            classes.push(Class::Positional);
        } else if arg == "--" {
            classes.push(Class::DoubleDash);
            after_dashes = true;
        } else {
            match classify(arg) {
                Ok(class) => classes.push(class),
                Err(message) => return error(message),
            }
        }
    }
    let mut options = Options {
        message: None, message_stdin: false, error: false, accept_label: tr!("Yes"), decline_label: tr!("No"), destructive: false,
    };
    let mut extras: Vec<&str> = Vec::new();
    let mut seen_group: Vec<Act> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let Class::Option { act, option, sep, explicit } = classes[index].clone() else {
            extras.push(&args[index]);
            index += 1;
            continue;
        };
        let Some(act) = act else {
            extras.push(&args[index]);
            index += 1;
            continue;
        };
        let value: Option<String>;
        let mut help_now = false;
        if let Some(explicit) = explicit {
            if act.takes_value() {
                value = Some(explicit);
            } else if !option[1..].starts_with('-') && !explicit.is_empty() {
                // Single-dash flag cluster: only -h exists, so "-hX" asks for help.
                if sep || explicit.starts_with('-') {
                    return error(format!("argument {}: ignored explicit argument {}", act.name(), py_repr(&explicit)));
                }
                value = None;
                help_now = true;
            } else {
                return error(format!("argument {}: ignored explicit argument {}", act.name(), py_repr(&explicit)));
            }
            index += 1;
        } else if act.takes_value() {
            match classes.get(index + 1) {
                Some(Class::Positional) => {
                    value = Some(args[index + 1].clone());
                    index += 2;
                }
                _ => return error(format!("argument {}: expected one argument", act.name())),
            }
        } else {
            value = None;
            index += 1;
        }
        // take_action: every appearance is a non-default value.
        if act.group() {
            for other in [Act::Message, Act::MessageStdin, Act::Error] {
                if other != act && seen_group.contains(&other) {
                    return error(format!("argument {}: not allowed with argument {}", act.name(), other.name()));
                }
            }
            if !seen_group.contains(&act) {
                seen_group.push(act);
            }
        }
        match act {
            Act::Help => help_now = true,
            Act::Message => options.message = value,
            Act::MessageStdin => options.message_stdin = true,
            Act::Error => options.error = true,
            Act::Accept => options.accept_label = value.unwrap_or_default(),
            Act::Decline => options.decline_label = value.unwrap_or_default(),
            Act::Destructive => options.destructive = true,
        }
        if help_now {
            return Parsed::Exit { code: 0, stdout: help_with_width(prog, width), stderr: String::new() };
        }
    }
    if seen_group.is_empty() {
        return error("one of the arguments --message --message-stdin --error is required".into());
    }
    if !extras.is_empty() {
        return error(format!("unrecognized arguments: {}", extras.join(" ")));
    }
    Parsed::Run(options)
}

/// `arguments(argv)` for tests: panics on a usage error.
pub fn arguments(args: &[&str]) -> Options {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    match parse("tessel-prompt", &args) {
        Parsed::Run(options) => options,
        other => panic!("arguments {args:?} failed: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_wraps_like_argparse_at_80_columns() {
        let text = help_with_width("wayshell_prompt.py", 78);
        assert!(text.starts_with("usage: wayshell_prompt.py [-h] (--message MESSAGE | --message-stdin | --error)\n                          [--accept-label ACCEPT_LABEL]\n"));
    }

    #[test]
    fn hyphen_chunks() {
        let pieces: Vec<String> = chunks("On-demand GTK4").into_iter().map(|c| c.0).collect();
        assert_eq!(pieces, vec!["On-", "demand", "GTK4"]);
    }
}
