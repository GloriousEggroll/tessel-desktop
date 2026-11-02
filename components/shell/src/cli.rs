//! The `tessel-shell` argv grammar (tessel-shell:4801 `_cli_client` and
//! `do_command_line`), the `wayshell_cli.py` keyboard client and the
//! `tessel-shell.sock` client side (contract §2.1, §2.2).
//!
//! The parser is a small argparse work-alike: long options with unique-prefix
//! abbreviations and `--opt=value`, short options with attached values, choices,
//! required options and argparse's error text (usage + `prog: error: …`, exit 2).

use serde_json::{json, Map, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tessel_core::pyfmt::{json_dumps, JsonStyle};

pub const MODES: [&str; 6] = ["Fill", "Fit", "Stretch", "Center", "Tile", "Color Only"];
const COLOR_MODES: [&str; 6] = ["Color Only", "Fill", "Fit", "Stretch", "Center", "Tile"];

/// Exact help block (tessel-shell:4806-4814).
pub const HELP: &str = "tessel-shell commands:
  list-monitors
  set --monitor KEY|all [--file PATH] [--mode Fill|Fit|Stretch|Center|Tile|Color Only] [--color CSS]
  set-color --monitor KEY|all --color CSS [--mode Color Only|Fill|Fit|Stretch|Center|Tile]
  apply-json /path/to/wallpapers.json
  switch-window [--reverse]
  show-activities [--monitor NAME]
  switcher-status
";

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    ListMonitors,
    SwitchWindow { reverse: bool },
    ShowActivities { monitor: Option<String> },
    SwitcherStatus,
    Set { monitor: String, file: Option<String>, mode: String, color: String },
    SetColor { monitor: String, color: String, mode: String },
    ApplyJson { json_path: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParseError {
    /// argparse's stderr text (usage lines + `prog: error: message\n`).
    pub text: String,
}

/// `wayshell_cli.monitor_name`.
pub fn monitor_name(value: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().count() > 128 || !is_printable(value) || tessel_core::pyjson::py_strip(value) != value {
        return Err("Monitor must be a valid output connector name.".into());
    }
    Ok(())
}

/// Python `str.isprintable()`: no Other (C*) or Separator (Z*) except ASCII space.
pub fn is_printable(value: &str) -> bool {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"[\p{C}\p{Z}&&[^ ]]").unwrap());
    !re.is_match(value)
}

// ---------------------------------------------------------------- mini argparse

struct Opt {
    long: &'static str,
    short: Option<&'static str>,
    dest: &'static str,
    flag: bool,
    required: bool,
    choices: Option<&'static [&'static str]>,
    /// argparse `type=` validator.
    check: Option<fn(&str) -> Result<(), String>>,
}

impl Opt {
    const fn value(long: &'static str, short: Option<&'static str>, dest: &'static str) -> Self {
        Self { long, short, dest, flag: false, required: false, choices: None, check: None }
    }
    fn names(&self) -> String {
        match self.short {
            Some(s) => format!("{}/{}", self.long, s),
            None => self.long.to_string(),
        }
    }
}

struct Sub {
    name: &'static str,
    usage: String,
    opts: Vec<Opt>,
    positional: Option<&'static str>,
}

struct Parser {
    prog: String,
    usage: String,
    dest: &'static str,
    required: bool,
    subs: Vec<Sub>,
}

type Parsed = (String, Map<String, Value>);

fn looks_negative_number(arg: &str) -> bool {
    let rest = &arg[1..];
    !rest.is_empty() && rest.parse::<f64>().is_ok() && !rest.starts_with(['+', '-'])
}

fn is_option(arg: &str) -> bool {
    arg.starts_with('-') && arg != "-" && !looks_negative_number(arg)
}

impl Parser {
    fn error(&self, usage: &str, prog: &str, message: String) -> ParseError {
        ParseError { text: format!("{usage}{prog}: error: {message}\n") }
    }

    fn parse(&self, argv: &[String]) -> Result<Parsed, ParseError> {
        let top_err = |m: String| self.error(&self.usage, &self.prog, m);
        // Top level has no options of its own (other than -h, handled by callers).
        let mut index = 0;
        let mut extras: Vec<String> = Vec::new();
        while index < argv.len() && is_option(&argv[index]) && argv[index] != "--" {
            extras.push(argv[index].clone());
            index += 1;
        }
        if index < argv.len() && argv[index] == "--" {
            index += 1;
        }
        if index >= argv.len() {
            if !extras.is_empty() && !self.required {
                return Err(top_err(format!("unrecognized arguments: {}", extras.join(" "))));
            }
            if self.required {
                return Err(top_err(format!("the following arguments are required: {}", self.dest)));
            }
            return Ok((String::new(), Map::new()));
        }
        let name = &argv[index];
        let Some(sub) = self.subs.iter().find(|s| s.name == name) else {
            let choices: Vec<String> = self.subs.iter().map(|s| format!("'{}'", s.name)).collect();
            return Err(top_err(format!("argument {}: invalid choice: '{}' (choose from {})", self.dest, name, choices.join(", "))));
        };
        let sub_prog = format!("{} {}", self.prog, sub.name);
        let sub_err = |m: String| self.error(&sub.usage, &sub_prog, m);
        let mut values = Map::new();
        for opt in &sub.opts {
            values.insert(opt.dest.into(), if opt.flag { Value::Bool(false) } else { Value::Null });
        }
        let mut seen: Vec<&str> = Vec::new();
        let mut positional: Option<String> = None;
        let rest = &argv[index + 1..];
        let mut i = 0;
        let mut only_positional = false;
        while i < rest.len() {
            let arg = &rest[i];
            i += 1;
            if !only_positional && arg == "--" {
                only_positional = true;
                // argparse keeps "--" as an extra when nothing consumes positionals.
                if sub.positional.is_none() || positional.is_some() {
                    extras.push(arg.clone());
                }
                continue;
            }
            if only_positional || !is_option(arg) {
                if sub.positional.is_some() && positional.is_none() {
                    positional = Some(arg.clone());
                } else {
                    extras.push(arg.clone());
                }
                continue;
            }
            // Resolve the option.
            let (opt, attached): (&Opt, Option<String>) = if let Some(long) = arg.strip_prefix("--") {
                let (key, explicit) = match long.split_once('=') {
                    Some((k, v)) => (k, Some(v.to_string())),
                    None => (long, None),
                };
                let full = format!("--{key}");
                if let Some(opt) = sub.opts.iter().find(|o| o.long == full) {
                    (opt, explicit)
                } else {
                    let matches: Vec<&Opt> = sub.opts.iter().filter(|o| o.long.starts_with(&full)).collect();
                    match matches.len() {
                        1 => (matches[0], explicit),
                        0 => {
                            extras.push(arg.clone());
                            continue;
                        }
                        _ => {
                            let names: Vec<&str> = matches.iter().map(|o| o.long).collect();
                            return Err(sub_err(format!("ambiguous option: {} could match {}", full, names.join(", "))));
                        }
                    }
                }
            } else {
                let short = &arg[..2.min(arg.len())];
                match sub.opts.iter().find(|o| o.short == Some(short)) {
                    Some(opt) if arg.len() > 2 => {
                        let tail = &arg[2..];
                        (opt, Some(tail.strip_prefix('=').unwrap_or(tail).to_string()))
                    }
                    Some(opt) => (opt, None),
                    None => {
                        extras.push(arg.clone());
                        continue;
                    }
                }
            };
            seen.push(opt.dest);
            if opt.flag {
                if let Some(value) = attached {
                    return Err(sub_err(format!("argument {}: ignored explicit argument '{}'", opt.names(), value)));
                }
                values.insert(opt.dest.into(), Value::Bool(true));
                continue;
            }
            let value = match attached {
                Some(v) => v,
                None => {
                    if i < rest.len() && !(is_option(&rest[i]) || rest[i] == "--") {
                        i += 1;
                        rest[i - 1].clone()
                    } else {
                        return Err(sub_err(format!("argument {}: expected one argument", opt.names())));
                    }
                }
            };
            if let Some(choices) = opt.choices {
                if !choices.contains(&value.as_str()) {
                    let list: Vec<String> = choices.iter().map(|c| format!("'{c}'")).collect();
                    return Err(sub_err(format!("argument {}: invalid choice: '{}' (choose from {})", opt.names(), value, list.join(", "))));
                }
            }
            if let Some(check) = opt.check {
                if let Err(message) = check(&value) {
                    return Err(sub_err(format!("argument {}: {}", opt.names(), message)));
                }
            }
            values.insert(opt.dest.into(), Value::String(value));
        }
        let mut missing: Vec<String> = sub.opts.iter().filter(|o| o.required && !seen.contains(&o.dest)).map(Opt::names).collect();
        if let Some(name) = sub.positional {
            if positional.is_none() {
                missing.push(name.to_string());
            }
        }
        if !missing.is_empty() {
            return Err(sub_err(format!("the following arguments are required: {}", missing.join(", "))));
        }
        if !extras.is_empty() {
            return Err(top_err(format!("unrecognized arguments: {}", extras.join(" "))));
        }
        if let (Some(name), Some(value)) = (sub.positional, positional) {
            values.insert(name.into(), Value::String(value));
        }
        Ok((sub.name.to_string(), values))
    }
}

fn usage_width() -> usize {
    std::env::var("COLUMNS").ok().and_then(|c| c.parse::<usize>().ok()).unwrap_or(80).saturating_sub(2).max(11)
}

fn wrap_usage(prog: &str, parts: &[&str]) -> String {
    // argparse wraps the usage at COLUMNS-2 (default 78) with continuation lines
    // indented under the first argument.
    let width = usage_width();
    let prefix = format!("usage: {prog} ");
    let one = format!("{prefix}{}", parts.join(" "));
    if one.len() <= width {
        return format!("{one}\n");
    }
    let indent = " ".repeat(prefix.len());
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for part in parts {
        if !line.is_empty() && prefix.len() + line.len() + 1 + part.len() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(part);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    let mut out = String::new();
    for (n, l) in lines.iter().enumerate() {
        out.push_str(if n == 0 { &prefix } else { &indent });
        out.push_str(l);
        out.push('\n');
    }
    out
}

fn top_usage(prog: &str, help: bool, choices: &str) -> String {
    // argparse puts the positional group on its own line when it doesn't fit.
    let width = usage_width();
    let prefix = format!("usage: {prog} ");
    let optionals = if help { "[-h]" } else { "" };
    let positional = format!("{choices} ...");
    let joined = [optionals, positional.as_str()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" ");
    if prefix.len() + joined.len() <= width {
        return format!("{prefix}{joined}\n");
    }
    let indent = " ".repeat(prefix.len());
    if optionals.is_empty() {
        format!("usage: {prog}\n{indent}{positional}\n")
    } else {
        format!("{prefix}{optionals}\n{indent}{positional}\n")
    }
}

fn tessel_shell_parser() -> Parser {
    let prog = "tessel-shell".to_string();
    let monitor = |required| Opt { required, ..Opt::value("--monitor", Some("-m"), "monitor") };
    let subs = vec![
        Sub { name: "list-monitors", usage: wrap_usage("tessel-shell list-monitors", &["[-h]"]), opts: vec![], positional: None },
        Sub { name: "switch-window", usage: wrap_usage("tessel-shell switch-window", &["[-h]", "[--reverse]"]),
              opts: vec![Opt { flag: true, ..Opt::value("--reverse", None, "reverse") }], positional: None },
        Sub { name: "show-activities", usage: wrap_usage("tessel-shell show-activities", &["[-h]", "[--monitor MONITOR]"]),
              opts: vec![Opt::value("--monitor", None, "monitor")], positional: None },
        Sub { name: "switcher-status", usage: wrap_usage("tessel-shell switcher-status", &["[-h]"]), opts: vec![], positional: None },
        Sub { name: "set",
              usage: wrap_usage("tessel-shell set", &["[-h]", "--monitor MONITOR", "[--file FILE]", "[--mode {Fill,Fit,Stretch,Center,Tile,Color Only}]", "[--color COLOR]"]),
              opts: vec![monitor(true), Opt::value("--file", Some("-f"), "file"),
                         Opt { choices: Some(&MODES), ..Opt::value("--mode", None, "mode") },
                         Opt::value("--color", Some("-c"), "color")],
              positional: None },
        Sub { name: "set-color",
              usage: wrap_usage("tessel-shell set-color", &["[-h]", "--monitor MONITOR", "--color COLOR", "[--mode {Color Only,Fill,Fit,Stretch,Center,Tile}]"]),
              opts: vec![monitor(true), Opt { required: true, ..Opt::value("--color", Some("-c"), "color") },
                         Opt { choices: Some(&COLOR_MODES), ..Opt::value("--mode", None, "mode") }],
              positional: None },
        Sub { name: "apply-json", usage: wrap_usage("tessel-shell apply-json", &["[-h]", "json_path"]), opts: vec![], positional: Some("json_path") },
    ];
    let choices = format!("{{{}}}", subs.iter().map(|s| s.name).collect::<Vec<_>>().join(","));
    Parser { usage: top_usage(&prog, false, &choices), prog, dest: "cmd", required: false, subs }
}

fn text(values: &Map<String, Value>, key: &str) -> Option<String> {
    values.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Parse the `tessel-shell` grammar. `Ok(None)` means no subcommand (start the daemon).
/// `file_default` is `None` in `do_command_line` and `Some("")` in the client.
pub fn parse_tessel_shell(argv: &[String], file_default: Option<&str>) -> Result<Option<Command>, ParseError> {
    let (name, v) = tessel_shell_parser().parse(argv)?;
    Ok(Some(match name.as_str() {
        "" => return Ok(None),
        "list-monitors" => Command::ListMonitors,
        "switch-window" => Command::SwitchWindow { reverse: v.get("reverse") == Some(&Value::Bool(true)) },
        "show-activities" => Command::ShowActivities { monitor: text(&v, "monitor") },
        "switcher-status" => Command::SwitcherStatus,
        "set" => Command::Set {
            monitor: text(&v, "monitor").unwrap_or_default(),
            file: text(&v, "file").or_else(|| file_default.map(str::to_string)),
            mode: text(&v, "mode").unwrap_or_else(|| "Fill".into()),
            color: text(&v, "color").unwrap_or_else(|| "black".into()),
        },
        "set-color" => Command::SetColor {
            monitor: text(&v, "monitor").unwrap_or_default(),
            color: text(&v, "color").unwrap_or_default(),
            mode: text(&v, "mode").unwrap_or_else(|| "Color Only".into()),
        },
        "apply-json" => Command::ApplyJson { json_path: text(&v, "json_path").unwrap_or_default() },
        _ => return Ok(None),
    }))
}

/// The JSON request `_cli_client` sends for a command.
pub fn client_request(command: &Command) -> Value {
    match command {
        Command::ListMonitors => json!({"cmd": "list-monitors"}),
        Command::SwitchWindow { reverse } => json!({"cmd": "switch-window", "reverse": reverse}),
        Command::SwitcherStatus => json!({"cmd": "switcher-status"}),
        Command::ShowActivities { monitor } => json!({"cmd": "show-activities", "monitor": monitor}),
        Command::Set { monitor, file, mode, color } =>
            json!({"cmd": "set", "monitor": monitor, "file": file.clone().unwrap_or_default(), "mode": mode, "color": color}),
        Command::SetColor { monitor, color, mode } =>
            json!({"cmd": "set", "monitor": monitor, "file": "", "mode": mode, "color": color}),
        Command::ApplyJson { json_path } => json!({"cmd": "apply-json", "json_path": json_path}),
    }
}

pub fn dumps(value: &Value) -> String {
    json_dumps(value, &JsonStyle::DEFAULT)
}

/// `_runtime_sock_path()` (client side of the daemon binary).
pub fn runtime_sock_path() -> PathBuf {
    let base = std::env::var("XDG_RUNTIME_DIR").ok().filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("/run/user/{}", unsafe { libc::getuid() }));
    let base = if Path::new(&base).is_dir() { base } else { "/tmp".into() };
    Path::new(&base).join("tessel-shell.sock")
}

#[derive(Debug)]
pub enum RequestError {
    Io(std::io::Error),
    Message(String),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestError::Io(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => f.write_str("timed out"),
            RequestError::Io(e) => f.write_str(&tessel_core::pyerr::os_error_text(e)),
            RequestError::Message(m) => f.write_str(m),
        }
    }
}

impl From<std::io::Error> for RequestError {
    fn from(e: std::io::Error) -> Self {
        RequestError::Io(e)
    }
}

fn connect(path: &Path, timeout: Duration) -> Result<UnixStream, RequestError> {
    let stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    Ok(stream)
}

/// `_cli_client`'s exchange: 3 s timeout, read until a newline, parse the stripped reply.
pub fn daemon_request(path: &Path, payload: &Value) -> Result<Value, RequestError> {
    let mut stream = connect(path, Duration::from_secs(3))?;
    stream.write_all(format!("{}\n", dumps(payload)).as_bytes())?;
    let mut response = Vec::new();
    let mut chunk = vec![0u8; 65536];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..n]);
        if response.contains(&b'\n') {
            break;
        }
    }
    let text = String::from_utf8_lossy(&response);
    serde_json::from_str(tessel_core::pyjson::py_strip(&text)).map_err(|e| RequestError::Message(e.to_string()))
}

/// `wayshell_cli.request`: 2 s timeout, reply ≤ 64 KiB, must be an object.
pub fn cli_request(payload: &Value, timeout: Duration) -> Result<Map<String, Value>, RequestError> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").ok().filter(|v| !v.is_empty())
        .ok_or_else(|| RequestError::Message("No desktop session runtime directory is available.".into()))?;
    let mut stream = connect(&Path::new(&runtime).join("tessel-shell.sock"), timeout)?;
    let mut line = dumps(payload).into_bytes();
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    while !data.contains(&b'\n') {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(RequestError::Message("Tessel Shell closed the command connection.".into()));
        }
        data.extend_from_slice(&chunk[..n]);
        if data.len() > 65536 {
            return Err(RequestError::Message("Tessel Shell returned an oversized response.".into()));
        }
    }
    let first = data.split(|b| *b == b'\n').next().unwrap_or_default();
    let reply: Value = serde_json::from_slice(first).map_err(|e| RequestError::Message(e.to_string()))?;
    match reply {
        Value::Object(map) => Ok(map),
        _ => Err(RequestError::Message("Tessel Shell returned an invalid response.".into())),
    }
}

/// What a CLI front end printed and its exit status (testable without a process).
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

/// `_cli_client(argv)`: `None` = not handled (start/contact the GApplication).
pub fn client_main(argv: &[String], sock_path: &Path) -> Option<Outcome> {
    if argv.is_empty() || argv[0] == "--gapplication-service" {
        return None;
    }
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        return Some(Outcome { stdout: HELP.into(), ..Default::default() });
    }
    if !sock_path.exists() {
        return None;
    }
    let command = match parse_tessel_shell(argv, Some("")) {
        Ok(Some(c)) => c,
        Ok(None) => return None,
        Err(e) => return Some(Outcome { stderr: format!("{}Invalid arguments. Use --help.\n", e.text), code: 2, ..Default::default() }),
    };
    let request = client_request(&command);
    let data = match daemon_request(sock_path, &request) {
        Ok(d) => d,
        Err(e) => return Some(Outcome { stderr: format!("CLI error: {e}\n"), code: 1, ..Default::default() }),
    };
    let ok = data.get("ok").map(tessel_core::pyjson::truthy).unwrap_or(false);
    let msg = |default: &str| data.get("msg").map(tessel_core::pyjson::py_str).unwrap_or_else(|| default.to_string());
    Some(match command {
        Command::SwitcherStatus => Outcome { stdout: format!("{}\n", dumps(&data)), code: if ok { 0 } else { 1 }, ..Default::default() },
        Command::ListMonitors if ok => {
            let mut out = String::new();
            if let Some(Value::Array(list)) = data.get("monitors") {
                for item in list {
                    out.push_str(&tessel_core::pyjson::py_str(item));
                    out.push('\n');
                }
            }
            Outcome { stdout: out, ..Default::default() }
        }
        Command::ListMonitors => Outcome { stderr: format!("{}\n", msg("error")), code: 1, ..Default::default() },
        _ if ok => Outcome { stdout: format!("{}\n", msg("OK")), ..Default::default() },
        _ => Outcome { stderr: format!("{}\n", msg("error")), code: 1, ..Default::default() },
    })
}

// ------------------------------------------------------------ wayshell_cli.py

const CLI_DESCRIPTION: &str = "Lightweight keyboard launcher for the existing Tessel Shell daemon.";
const CLI_CHOICES: &str = "{switch-window,show-activities,switcher-status}";

fn keyboard_parser(prog: &str) -> Parser {
    let subs = vec![
        Sub { name: "switch-window", usage: wrap_usage(&format!("{prog} switch-window"), &["[-h]", "[--reverse]"]),
              opts: vec![Opt { flag: true, ..Opt::value("--reverse", None, "reverse") }], positional: None },
        Sub { name: "show-activities", usage: wrap_usage(&format!("{prog} show-activities"), &["[-h]", "[--monitor MONITOR]"]),
              opts: vec![Opt { check: Some(monitor_name), ..Opt::value("--monitor", None, "monitor") }], positional: None },
        Sub { name: "switcher-status", usage: wrap_usage(&format!("{prog} switcher-status"), &["[-h]"]), opts: vec![], positional: None },
    ];
    Parser { usage: top_usage(prog, true, CLI_CHOICES), prog: prog.to_string(), dest: "command", required: true, subs }
}

fn keyboard_help(prog: &str, argv: &[String]) -> Option<String> {
    // argparse processes -h when it is reached: top-level before the command,
    // otherwise the subcommand's own help.
    let position = argv.iter().position(|a| a == "-h" || a == "--help")?;
    let command = argv[..position].iter().find(|a| !a.starts_with('-'));
    Some(match command.map(String::as_str) {
        None => format!("{}\n\n{CLI_DESCRIPTION}\n\npositional arguments:\n  {CLI_CHOICES}\n    switch-window       advance the Alt+Tab switcher\n    show-activities     show the persistent window overview\n    switcher-status     check the running switcher without opening it\n\noptions:\n  -h, --help            show this help message and exit\n",
                        top_usage(prog, true, CLI_CHOICES).trim_end()),
        Some("switch-window") => format!("usage: {prog} switch-window [-h] [--reverse]\n\noptions:\n  -h, --help  show this help message and exit\n  --reverse\n"),
        Some("show-activities") => format!("usage: {prog} show-activities [-h] [--monitor MONITOR]\n\noptions:\n  -h, --help         show this help message and exit\n  --monitor MONITOR\n"),
        Some("switcher-status") => format!("usage: {prog} switcher-status [-h]\n\noptions:\n  -h, --help  show this help message and exit\n"),
        Some(_) => return None,
    })
}

/// `wayshell_cli.main(argv)`; `request` performs the socket exchange.
pub fn keyboard_main(prog: &str, argv: &[String], request: impl FnOnce(&Value) -> Result<Map<String, Value>, RequestError>) -> Outcome {
    if let Some(help) = keyboard_help(prog, argv) {
        return Outcome { stdout: help, ..Default::default() };
    }
    let (name, values) = match keyboard_parser(prog).parse(argv) {
        Ok(p) => p,
        Err(e) => return Outcome { stderr: e.text, code: 2, ..Default::default() },
    };
    let mut payload = Map::new();
    payload.insert("cmd".into(), Value::String(name.clone()));
    if name == "switch-window" {
        payload.insert("reverse".into(), values.get("reverse").cloned().unwrap_or(Value::Bool(false)));
    } else if name == "show-activities" {
        if let Some(Value::String(monitor)) = values.get("monitor") {
            payload.insert("monitor".into(), Value::String(monitor.clone()));
        }
    }
    let mut out = Outcome::default();
    match request(&Value::Object(payload)) {
        Ok(reply) => {
            if name == "switcher-status" {
                out.stdout = format!("{}\n", dumps(&Value::Object(reply.clone())));
            }
            if !reply.get("ok").map(tessel_core::pyjson::truthy).unwrap_or(false) {
                let msg = reply.get("msg").map(tessel_core::pyjson::py_str)
                    .unwrap_or_else(|| "The desktop shell could not handle the request.".into());
                out.stderr = format!("{msg}\n");
                out.code = 1;
            }
        }
        Err(error) => {
            out.stderr = format!("Tessel Shell: {error}\n");
            out.code = 1;
        }
    }
    out
}

pub fn emit(outcome: &Outcome) -> i32 {
    if !outcome.stdout.is_empty() {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(outcome.stdout.as_bytes());
        let _ = stdout.flush();
    }
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    outcome.code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn grammar_matches_argparse() {
        assert_eq!(parse_tessel_shell(&args(&["switch-window", "--rev"]), None).unwrap(), Some(Command::SwitchWindow { reverse: true }));
        assert_eq!(parse_tessel_shell(&args(&["set", "--mon", "A", "--fi", "b"]), Some("")).unwrap(),
                   Some(Command::Set { monitor: "A".into(), file: Some("b".into()), mode: "Fill".into(), color: "black".into() }));
        assert_eq!(parse_tessel_shell(&args(&["set", "-mA", "-fB", "--color=red"]), None).unwrap(),
                   Some(Command::Set { monitor: "A".into(), file: Some("B".into()), mode: "Fill".into(), color: "red".into() }));
        assert_eq!(parse_tessel_shell(&args(&["set", "-m", "all"]), None).unwrap(),
                   Some(Command::Set { monitor: "all".into(), file: None, mode: "Fill".into(), color: "black".into() }));
        assert_eq!(parse_tessel_shell(&args(&["set-color", "-m", "A", "-c", "red"]), None).unwrap(),
                   Some(Command::SetColor { monitor: "A".into(), color: "red".into(), mode: "Color Only".into() }));
        assert_eq!(parse_tessel_shell(&args(&["set", "-m", "A", "--mode", "Color Only"]), None).unwrap().unwrap(),
                   Command::Set { monitor: "A".into(), file: None, mode: "Color Only".into(), color: "black".into() });
        assert_eq!(parse_tessel_shell(&args(&["apply-json", "/x.json"]), None).unwrap(), Some(Command::ApplyJson { json_path: "/x.json".into() }));
        assert_eq!(parse_tessel_shell(&args(&["show-activities"]), None).unwrap(), Some(Command::ShowActivities { monitor: None }));
        assert_eq!(parse_tessel_shell(&[], None).unwrap(), None);
    }

    #[test]
    fn argparse_error_text_is_python_text() {
        std::env::remove_var("COLUMNS");
        let top = "usage: tessel-shell\n                    {list-monitors,switch-window,show-activities,switcher-status,set,set-color,apply-json} ...\n";
        let set = "usage: tessel-shell set [-h] --monitor MONITOR [--file FILE]\n                        [--mode {Fill,Fit,Stretch,Center,Tile,Color Only}]\n                        [--color COLOR]\n";
        let cases: Vec<(&[&str], String)> = vec![
            (&["bogus"], format!("{top}tessel-shell: error: argument cmd: invalid choice: 'bogus' (choose from 'list-monitors', 'switch-window', 'show-activities', 'switcher-status', 'set', 'set-color', 'apply-json')\n")),
            (&["set"], format!("{set}tessel-shell set: error: the following arguments are required: --monitor/-m\n")),
            (&["set", "-m"], format!("{set}tessel-shell set: error: argument --monitor/-m: expected one argument\n")),
            (&["set", "-m", "X", "--mode", "Bad"], format!("{set}tessel-shell set: error: argument --mode: invalid choice: 'Bad' (choose from 'Fill', 'Fit', 'Stretch', 'Center', 'Tile', 'Color Only')\n")),
            (&["switch-window", "--reverse=1"], "usage: tessel-shell switch-window [-h] [--reverse]\ntessel-shell switch-window: error: argument --reverse: ignored explicit argument '1'\n".into()),
            (&["list-monitors", "x"], format!("{top}tessel-shell: error: unrecognized arguments: x\n")),
            (&["apply-json"], "usage: tessel-shell apply-json [-h] json_path\ntessel-shell apply-json: error: the following arguments are required: json_path\n".into()),
            (&["set", "--m", "A"], format!("{set}tessel-shell set: error: ambiguous option: --m could match --monitor, --mode\n")),
            (&["--x"], format!("{top}tessel-shell: error: unrecognized arguments: --x\n")),
            (&["set-color", "-m", "A"], "usage: tessel-shell set-color [-h] --monitor MONITOR --color COLOR\n                              [--mode {Color Only,Fill,Fit,Stretch,Center,Tile}]\ntessel-shell set-color: error: the following arguments are required: --color/-c\n".into()),
            (&["show-activities", "--monitor"], "usage: tessel-shell show-activities [-h] [--monitor MONITOR]\ntessel-shell show-activities: error: argument --monitor: expected one argument\n".into()),
            (&["set", "-m", "A", "--", "x"], format!("{top}tessel-shell: error: unrecognized arguments: -- x\n")),
            (&["switch-window", "-r"], format!("{top}tessel-shell: error: unrecognized arguments: -r\n")),
        ];
        for (argv, expected) in cases {
            let error = parse_tessel_shell(&args(argv), Some("")).unwrap_err();
            assert_eq!(error.text, expected, "{argv:?}");
        }
    }

    #[test]
    fn requests_match_python_payloads() {
        let set = Command::Set { monitor: "A".into(), file: Some("".into()), mode: "Fill".into(), color: "black".into() };
        assert_eq!(dumps(&client_request(&set)), r#"{"cmd": "set", "monitor": "A", "file": "", "mode": "Fill", "color": "black"}"#);
        let color = Command::SetColor { monitor: "all".into(), color: "red".into(), mode: "Color Only".into() };
        assert_eq!(dumps(&client_request(&color)), r#"{"cmd": "set", "monitor": "all", "file": "", "mode": "Color Only", "color": "red"}"#);
        assert_eq!(dumps(&client_request(&Command::ShowActivities { monitor: None })), r#"{"cmd": "show-activities", "monitor": null}"#);
        assert_eq!(dumps(&client_request(&Command::SwitchWindow { reverse: false })), r#"{"cmd": "switch-window", "reverse": false}"#);
    }

    #[test]
    fn monitor_name_validation() {
        assert!(monitor_name("HDMI-A-1").is_ok());
        assert!(monitor_name("").is_err());
        assert!(monitor_name(" X").is_err());
        assert!(monitor_name("X ").is_err());
        assert!(monitor_name("A\u{7}").is_err());
        assert!(monitor_name("A\u{200b}B").is_err());
        assert!(monitor_name("A B").is_ok());
        assert!(monitor_name(&"x".repeat(129)).is_err());
        assert!(monitor_name(&"é".repeat(128)).is_ok());
    }

    #[test]
    fn keyboard_client_payloads_and_errors() {
        std::env::remove_var("COLUMNS");
        let ok = |v: Value| v.as_object().unwrap().clone();
        let mut sent = Vec::new();
        let out = keyboard_main("wayshell_cli.py", &args(&["show-activities"]), |p| { sent.push(dumps(p)); Ok(ok(json!({"ok": true}))) });
        assert_eq!(out, Outcome::default());
        assert_eq!(sent, vec![r#"{"cmd": "show-activities"}"#.to_string()]);
        let out = keyboard_main("wayshell_cli.py", &args(&["show-activities", "--monitor", " X"]), |_| unreachable!());
        assert_eq!(out.code, 2);
        assert_eq!(out.stderr, "usage: wayshell_cli.py show-activities [-h] [--monitor MONITOR]\nwayshell_cli.py show-activities: error: argument --monitor: Monitor must be a valid output connector name.\n");
        let out = keyboard_main("wayshell_cli.py", &[], |_| unreachable!());
        assert_eq!(out.stderr, "usage: wayshell_cli.py [-h]\n                       {switch-window,show-activities,switcher-status} ...\nwayshell_cli.py: error: the following arguments are required: command\n");
        let out = keyboard_main("wayshell_cli.py", &args(&["bogus"]), |_| unreachable!());
        assert_eq!(out.stderr, "usage: wayshell_cli.py [-h]\n                       {switch-window,show-activities,switcher-status} ...\nwayshell_cli.py: error: argument command: invalid choice: 'bogus' (choose from 'switch-window', 'show-activities', 'switcher-status')\n");
        let out = keyboard_main("wayshell_cli.py", &args(&["-h"]), |_| unreachable!());
        assert_eq!(out.stdout, "usage: wayshell_cli.py [-h]\n                       {switch-window,show-activities,switcher-status} ...\n\nLightweight keyboard launcher for the existing Tessel Shell daemon.\n\npositional arguments:\n  {switch-window,show-activities,switcher-status}\n    switch-window       advance the Alt+Tab switcher\n    show-activities     show the persistent window overview\n    switcher-status     check the running switcher without opening it\n\noptions:\n  -h, --help            show this help message and exit\n");
        let mut sent = Vec::new();
        let out = keyboard_main("wayshell_cli.py", &args(&["switch-window", "--reverse"]), |p| { sent.push(dumps(p)); Ok(ok(json!({"ok": false, "msg": "nope"}))) });
        assert_eq!(sent, vec![r#"{"cmd": "switch-window", "reverse": true}"#.to_string()]);
        assert_eq!((out.code, out.stderr.as_str()), (1, "nope\n"));
        let out = keyboard_main("wayshell_cli.py", &args(&["switcher-status"]), |_| Ok(ok(json!({"ok": true, "ready": true}))));
        assert_eq!(out.stdout, "{\"ok\": true, \"ready\": true}\n");
        let out = keyboard_main("wayshell_cli.py", &args(&["switcher-status"]), |_| Err(RequestError::Io(std::io::Error::from_raw_os_error(111))));
        assert_eq!(out.stderr, "Tessel Shell: [Errno 111] Connection refused\n");
    }
}
