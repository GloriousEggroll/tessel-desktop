//! A small Python `argparse` emulation (optionals only): prefix abbreviations,
//! `--opt=value`, choices, `int` values, one mutually exclusive group, the
//! 3.13 usage wrapping and help layout, error text and exit status 2.
//! Candidate for tessel-core (tessel-power has a hand-written variant).

use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub enum Kind {
    Flag,
    Value { metavar: &'static str, choices: Option<&'static [&'static str]>, int: bool },
}

#[derive(Clone, Debug)]
pub struct Opt {
    pub name: &'static str,
    pub kind: Kind,
    pub help: Option<&'static str>,
    /// Members of the (single) mutually exclusive group.
    pub exclusive: bool,
}

impl Opt {
    pub const fn flag(name: &'static str, help: Option<&'static str>, exclusive: bool) -> Self {
        Self { name, kind: Kind::Flag, help, exclusive }
    }
    pub const fn value(name: &'static str, metavar: &'static str, help: Option<&'static str>) -> Self {
        Self { name, kind: Kind::Value { metavar, choices: None, int: false }, help, exclusive: false }
    }
    pub const fn choice(name: &'static str, choices: &'static [&'static str]) -> Self {
        Self { name, kind: Kind::Value { metavar: "", choices: Some(choices), int: false }, help: None, exclusive: false }
    }
    pub const fn int(name: &'static str, metavar: &'static str) -> Self {
        Self { name, kind: Kind::Value { metavar, choices: None, int: true }, help: None, exclusive: false }
    }

    fn invocation(&self) -> String {
        match &self.kind {
            Kind::Flag => self.name.to_owned(),
            Kind::Value { choices: Some(choices), .. } => format!("{} {{{}}}", self.name, choices.join(",")),
            Kind::Value { metavar, .. } => format!("{} {metavar}", self.name),
        }
    }
}

pub struct Parser {
    pub prog: String,
    pub description: Option<&'static str>,
    pub options: Vec<Opt>,
}

/// `Err((exit_status, text, to_stdout))`.
pub type Failure = (i32, String, bool);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parsed(pub BTreeMap<&'static str, String>);

impl Parsed {
    pub fn flag(&self, name: &str) -> bool { self.0.contains_key(name) }
    pub fn value(&self, name: &str) -> Option<&str> { self.0.get(name).map(String::as_str) }
    pub fn int(&self, name: &str) -> Option<i64> { self.value(name).and_then(python_int) }
}

const TEXT_WIDTH: usize = 78;

/// `prog` = basename of argv[0], like argparse.
pub fn prog_name(argv0: &str, fallback: &str) -> String {
    std::path::Path::new(argv0).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| fallback.into())
}

/// `int()` of Python: optional sign, surrounding whitespace, underscores between digits.
pub fn python_int(text: &str) -> Option<i64> {
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

fn looks_like_option(value: &str) -> bool {
    if !value.starts_with('-') || value == "-" || value.contains(' ') {
        return false;
    }
    // argparse treats negative numbers as values when no option looks like one.
    value[1..].parse::<f64>().is_err()
}

impl Parser {
    fn usage_parts(&self) -> Vec<String> {
        let mut parts = vec!["[-h]".to_owned()];
        let group: Vec<&Opt> = self.options.iter().filter(|o| o.exclusive).collect();
        let mut group_done = false;
        for option in &self.options {
            if option.exclusive {
                if group_done {
                    continue;
                }
                group_done = true;
                for (index, member) in group.iter().enumerate() {
                    let mut part = member.invocation();
                    if index == 0 {
                        part.insert(0, '[');
                    }
                    if index + 1 == group.len() { part.push(']') } else { part.push_str(" |") }
                    parts.push(part);
                }
            } else {
                parts.push(format!("[{}]", option.invocation()));
            }
        }
        parts
    }

    pub fn usage(&self) -> String {
        let prefix = "usage: ";
        let parts = self.usage_parts();
        let single = format!("{} {}", self.prog, parts.join(" "));
        if prefix.len() + single.len() <= TEXT_WIDTH {
            return format!("{prefix}{single}\n");
        }
        let indent = " ".repeat(prefix.len() + self.prog.len() + 1);
        let mut lines: Vec<String> = Vec::new();
        let mut line: Vec<String> = Vec::new();
        let mut line_len = prefix.len() - 1;
        for part in std::iter::once(self.prog.clone()).chain(parts) {
            if line_len + 1 + part.chars().count() > TEXT_WIDTH && !line.is_empty() {
                lines.push(format!("{indent}{}", line.join(" ")));
                line.clear();
                line_len = indent.len() - 1;
            }
            line_len += part.chars().count() + 1;
            line.push(part);
        }
        if !line.is_empty() {
            lines.push(format!("{indent}{}", line.join(" ")));
        }
        lines[0] = lines[0][indent.len()..].to_owned();
        format!("{prefix}{}\n", lines.join("\n"))
    }

    pub fn help(&self) -> String {
        let mut entries: Vec<(String, Option<&str>)> = vec![("-h, --help".into(), Some("show this help message and exit"))];
        entries.extend(self.options.iter().map(|o| (o.invocation(), o.help)));
        let longest = entries.iter().map(|e| e.0.chars().count() + 2).max().unwrap_or(0);
        let help_position = (longest + 2).min(24);
        let action_width = help_position - 4;
        let mut text = self.usage();
        if let Some(description) = self.description {
            text.push('\n');
            text.push_str(description);
            text.push('\n');
        }
        text.push_str("\noptions:\n");
        for (invocation, help) in entries {
            match help {
                None => text.push_str(&format!("  {invocation}\n")),
                Some(help) if invocation.chars().count() <= action_width => {
                    text.push_str(&format!("  {invocation:<action_width$}  {help}\n"))
                }
                Some(help) => text.push_str(&format!("  {invocation}\n{}{help}\n", " ".repeat(help_position))),
            }
        }
        text
    }

    pub fn error(&self, message: &str) -> Failure {
        (2, format!("{}{}: error: {message}\n", self.usage(), self.prog), false)
    }

    pub fn parse(&self, argv: &[String]) -> Result<Parsed, Failure> {
        let mut parsed = Parsed::default();
        let mut unrecognized: Vec<String> = Vec::new();
        let mut exclusive_seen: Option<&'static str> = None;
        let mut names: Vec<&'static str> = vec!["--help"];
        names.extend(self.options.iter().map(|o| o.name));
        let mut index = 0;
        while index < argv.len() {
            let word = &argv[index];
            index += 1;
            if word == "--" {
                unrecognized.extend(argv[index..].iter().cloned());
                break;
            }
            if word == "-h" {
                return Err((0, self.help(), true));
            }
            if !word.starts_with("--") || word.len() == 2 {
                unrecognized.push(word.clone());
                continue;
            }
            let (name, explicit) = match word.split_once('=') {
                Some((name, value)) => (name, Some(value.to_owned())),
                None => (word.as_str(), None),
            };
            let resolved: &'static str = if let Some(exact) = names.iter().find(|o| **o == name) {
                exact
            } else {
                let matches: Vec<&'static str> = names.iter().copied().filter(|o| o.starts_with(name)).collect();
                match matches.len() {
                    0 => {
                        unrecognized.push(word.clone());
                        continue;
                    }
                    1 => matches[0],
                    _ => return Err(self.error(&format!("ambiguous option: {name} could match {}", matches.join(", ")))),
                }
            };
            if resolved == "--help" {
                if let Some(explicit) = explicit {
                    return Err(self.error(&format!("argument -h/--help: ignored explicit argument '{explicit}'")));
                }
                return Err((0, self.help(), true));
            }
            let option = self.options.iter().find(|o| o.name == resolved).expect("known option");
            let value = match &option.kind {
                Kind::Flag => {
                    if let Some(explicit) = explicit {
                        return Err(self.error(&format!("argument {resolved}: ignored explicit argument '{explicit}'")));
                    }
                    String::new()
                }
                Kind::Value { choices, int, .. } => {
                    let value = match explicit {
                        Some(explicit) => explicit,
                        None => match argv.get(index) {
                            Some(next) if !looks_like_option(next) => {
                                index += 1;
                                next.clone()
                            }
                            _ => return Err(self.error(&format!("argument {resolved}: expected one argument"))),
                        },
                    };
                    if *int && python_int(&value).is_none() {
                        return Err(self.error(&format!("argument {resolved}: invalid int value: '{value}'")));
                    }
                    if let Some(choices) = choices {
                        if !choices.contains(&value.as_str()) {
                            let quoted = choices.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(", ");
                            return Err(self.error(&format!("argument {resolved}: invalid choice: '{value}' (choose from {quoted})")));
                        }
                    }
                    value
                }
            };
            if option.exclusive {
                if let Some(other) = exclusive_seen.filter(|o| *o != resolved) {
                    return Err(self.error(&format!("argument {resolved}: not allowed with argument {other}")));
                }
                exclusive_seen = Some(resolved);
            }
            parsed.0.insert(resolved, value);
        }
        if !unrecognized.is_empty() {
            return Err(self.error(&format!("unrecognized arguments: {}", unrecognized.join(" "))));
        }
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDGES: &[&str] = &["top", "bottom", "left", "right"];

    fn weather(prog: &str) -> Parser {
        Parser { prog: prog.into(), description: Some("Weather popup and settings for Tessel Dock; no background service required."),
                 options: vec![Opt::flag("--popup", Some("show current weather and the forecast (default)"), true),
                               Opt::flag("--settings", Some("open Weather settings"), true),
                               Opt::flag("--tray", Some("stay in the system tray"), true),
                               Opt::choice("--popup-edge", EDGES), Opt::int("--popup-offset", "POPUP_OFFSET")] }
    }

    fn words(text: &str) -> Vec<String> { text.split_whitespace().map(String::from).collect() }

    #[test]
    fn usage_and_help_match_python() {
        let parser = weather("weather_manager.py");
        assert_eq!(parser.usage(), "usage: weather_manager.py [-h] [--popup | --settings | --tray]\n                          \
            [--popup-edge {top,bottom,left,right}]\n                          [--popup-offset POPUP_OFFSET]\n");
        assert!(parser.help().ends_with("options:\n  -h, --help            show this help message and exit\n  \
            --popup               show current weather and the forecast (default)\n  --settings            open Weather settings\n  \
            --tray                stay in the system tray\n  --popup-edge {top,bottom,left,right}\n  --popup-offset POPUP_OFFSET\n"));
        let status = Parser { prog: "weather_status.py".into(), description: Some("Current weather as JSON for the desktop panel"),
            options: vec![Opt::flag("--watch", None, false), Opt::value("--config", "CONFIG", Some("Use a different configuration file")),
                          Opt::value("--cache-dir", "CACHE_DIR", Some("Use a different cache folder"))] };
        assert_eq!(status.usage(), "usage: weather_status.py [-h] [--watch] [--config CONFIG]\n                         [--cache-dir CACHE_DIR]\n");
        assert!(status.help().ends_with("  --config CONFIG       Use a different configuration file\n  --cache-dir CACHE_DIR\n                        Use a different cache folder\n"));
    }

    #[test]
    fn errors_match_python() {
        let parser = weather("weather_manager.py");
        let tail = |argv: &str| parser.parse(&words(argv)).unwrap_err().1.lines().last().unwrap().to_owned();
        assert_eq!(tail("--bogus"), "weather_manager.py: error: unrecognized arguments: --bogus");
        assert_eq!(tail("--popup --settings"), "weather_manager.py: error: argument --settings: not allowed with argument --popup");
        assert_eq!(tail("--popup-edge middle"),
                   "weather_manager.py: error: argument --popup-edge: invalid choice: 'middle' (choose from 'top', 'bottom', 'left', 'right')");
        assert_eq!(tail("--popup-offset x"), "weather_manager.py: error: argument --popup-offset: invalid int value: 'x'");
        assert_eq!(tail("--pop"), "weather_manager.py: error: ambiguous option: --pop could match --popup, --popup-edge, --popup-offset");
        let parsed = parser.parse(&words("--set --popup-offset -1 --popup-edge=top")).unwrap();
        assert!(parsed.flag("--settings"));
        assert_eq!(parsed.int("--popup-offset"), Some(-1));
        assert_eq!(parsed.value("--popup-edge"), Some("top"));
        assert_eq!(parser.parse(&words("--help")).unwrap_err().0, 0);
    }
}
