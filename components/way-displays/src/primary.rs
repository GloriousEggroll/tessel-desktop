//! XWayland / application primary output: the labwc `environment` block, the
//! labwc `xinitrc` block, the `CALLBACK_CMD` managed block and `xrandr`
//! (way-displays-gui:44-182, 368-539).

use crate::compat::fsx::{self, Replace};
use crate::compat::shlex;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::Duration;
use suite_core::shell::shell_quote;
use suite_core::tr;

pub const PRIMARY_CALLBACK_BEGIN: &str = "# way-displays-gui primary output begin";
pub const PRIMARY_CALLBACK_END: &str = "# way-displays-gui primary output end";
pub const PRIMARY_CALLBACK_OUTPUT: &str = "# output: ";
pub const LABWC_PRIMARY_ENV_BEGIN: &str = "# way-displays-gui primary output environment begin";
pub const LABWC_PRIMARY_ENV_END: &str = "# way-displays-gui primary output environment end";
pub const LABWC_PRIMARY_ENV_VARS: [&str; 3] = ["SDL_VIDEO_DISPLAY_PRIORITY", "WAYLANDDRV_PRIMARY_MONITOR", "PROTON_WAYLAND_MONITOR"];

/// `(?m)^BEGIN\r?\n.*?^END[ \t]*(?:\r?\n|$)` with DOTALL.
fn managed_block(begin: &str, end: &str) -> Regex {
    Regex::new(&format!(r"(?ms)^{}\r?\n.*?^{}[ \t]*(?:\r?\n|$)", regex::escape(begin), regex::escape(end))).unwrap()
}

static ENV_PATTERN: LazyLock<Regex> = LazyLock::new(|| managed_block(LABWC_PRIMARY_ENV_BEGIN, LABWC_PRIMARY_ENV_END));
static XINITRC_PATTERN: LazyLock<Regex> = LazyLock::new(|| managed_block(PRIMARY_CALLBACK_BEGIN, PRIMARY_CALLBACK_END));
static CALLBACK_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"(?s)(?:^|\n){}\n(?P<body>.*?)\n{}(?:\n|$)",
                        regex::escape(PRIMARY_CALLBACK_BEGIN), regex::escape(PRIMARY_CALLBACK_END))).unwrap()
});
static XRANDR_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^[ \t]*(?:xrandr|/usr/bin/xrandr)[ \t]+[^\n]*(?:\n|$)").unwrap());
static XRANDR_PRIMARY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\S+)\s+connected\s+primary(?:\s|$)").unwrap());

/// `(ok, error, changed)` of the Python writers.
pub type WriteResult = (bool, Option<String>, bool);

/// `$XDG_CONFIG_HOME/labwc/environment` (absolute).
pub fn labwc_environment_path() -> PathBuf {
    let config = suite_core::paths::config_home();
    std::path::absolute(&config).unwrap_or(config).join("labwc").join("environment")
}

/// The primary output stored in our managed environment block.
pub fn parse_labwc_primary_environment(text: &str) -> Option<String> {
    let found = ENV_PATTERN.find(text)?;
    let mut values = std::collections::HashMap::new();
    for line in fsx::splitlines(found.as_str()) {
        let Some((name, value)) = line.split_once('=') else { continue };
        let name = name.trim();
        if LABWC_PRIMARY_ENV_VARS.contains(&name) {
            values.insert(name.to_owned(), value.trim().to_owned());
        }
    }
    LABWC_PRIMARY_ENV_VARS.iter().find_map(|name| values.get(*name).filter(|v| !v.is_empty()).cloned())
}

pub fn read_labwc_primary_environment(path: &Path) -> Option<String> {
    fsx::read_text(path).ok().and_then(|text| parse_labwc_primary_environment(&text))
}

fn clean_output(output: &str) -> String {
    output.replace(['\r', '\n'], "").trim().to_owned()
}

/// The labwc environment block for Wayland-aware applications.
pub fn compose_labwc_primary_environment(primary_output: Option<&str>) -> String {
    let output = clean_output(primary_output.unwrap_or(""));
    if output.is_empty() {
        return String::new();
    }
    let mut lines = vec![LABWC_PRIMARY_ENV_BEGIN.to_owned()];
    lines.extend(LABWC_PRIMARY_ENV_VARS.iter().map(|name| format!("{name}={output}")));
    lines.push(LABWC_PRIMARY_ENV_END.to_owned());
    lines.join("\n")
}

/// Atomically add, update or remove our managed labwc environment block.
pub fn write_labwc_primary_environment(path: &Path, primary_output: Option<&str>) -> WriteResult {
    write_labwc_primary_environment_with(path, primary_output, &fsx::os_replace)
}

pub fn write_labwc_primary_environment_with(path: &Path, primary_output: Option<&str>, replace: Replace) -> WriteResult {
    let (text, mode) = match fsx::read_text(path) {
        Ok(text) => match fsx::mode_of(path) {
            Ok(mode) => (text, mode),
            Err(error) => return (false, Some(fsx::read_error(&error, path)), false),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (String::new(), 0o600),
        Err(error) => return (false, Some(fsx::read_error(&error, path)), false),
    };
    let block = compose_labwc_primary_environment(primary_output);
    let updated = if ENV_PATTERN.is_match(&text) {
        let replacement = if block.is_empty() { String::new() } else { format!("{block}\n") };
        let updated = ENV_PATTERN.replacen(&text, 1, regex::NoExpand(&replacement)).into_owned();
        if block.is_empty() { ENV_PATTERN.replace_all(&updated, "").into_owned() } else { updated }
    } else if !block.is_empty() {
        let mut updated = text.clone();
        if !updated.is_empty() && !updated.ends_with('\n') {
            updated.push('\n');
        }
        if !updated.is_empty() && !updated.ends_with("\n\n") {
            updated.push('\n');
        }
        updated + &block + "\n"
    } else {
        text.clone()
    };
    if updated == text {
        return (true, None, false);
    }
    match fsx::atomic_write_with(path, updated.as_bytes(), mode, ".environment.", replace) {
        Ok(()) => (true, None, true),
        Err(error) => (false, Some(error), false),
    }
}

/// `(user command, persisted primary output)` of a `CALLBACK_CMD`.
pub fn split_primary_callback(command: &str) -> (String, Option<String>) {
    let mut primary = None;
    for found in CALLBACK_PATTERN.captures_iter(command) {
        for line in fsx::splitlines(&found["body"]) {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix(PRIMARY_CALLBACK_OUTPUT) {
                let rest = rest.trim();
                primary = (!rest.is_empty()).then(|| rest.to_owned());
                break;
            }
        }
    }
    let user = CALLBACK_PATTERN.replace_all(command, "\n").trim().to_owned();
    (user, primary)
}

/// Combine a user callback with the managed XWayland primary command.
pub fn compose_primary_callback(user_command: &str, primary_output: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(output) = primary_output.filter(|o| !o.is_empty()) {
        let output = clean_output(output);
        let quoted = shell_quote(&output);
        parts.push(format!(
            "{PRIMARY_CALLBACK_BEGIN}\n{PRIMARY_CALLBACK_OUTPUT}{output}\nif command -v xrandr >/dev/null 2>&1; then\n  xrandr --output {quoted} --primary >/dev/null 2>&1 || true\nfi\n{PRIMARY_CALLBACK_END}"));
    }
    let user = user_command.trim();
    if !user.is_empty() {
        parts.push(user.to_owned());
    }
    parts.join("\n")
}

/// Persist the selected primary whenever labwc starts/restarts XWayland.
pub fn write_labwc_primary_xinitrc(path: &Path, primary_output: Option<&str>) -> WriteResult {
    write_labwc_primary_xinitrc_with(path, primary_output, &fsx::os_replace)
}

pub fn write_labwc_primary_xinitrc_with(path: &Path, primary_output: Option<&str>, replace: Replace) -> WriteResult {
    let primary_output = primary_output.filter(|o| !o.is_empty());
    let (text, mode) = match fsx::read_text(path) {
        Ok(text) => match fsx::mode_of(path) {
            Ok(mode) => (text, mode),
            Err(error) => return (false, Some(fsx::read_error(&error, path)), false),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if primary_output.is_none() {
                return (true, None, false);
            }
            ("#!/bin/sh\n".to_owned(), 0o700)
        }
        Err(error) => return (false, Some(fsx::read_error(&error, path)), false),
    };
    let block = compose_primary_callback("", primary_output);
    let mut spans: Vec<(usize, usize)> = XINITRC_PATTERN.find_iter(&text).map(|m| (m.start(), m.end())).collect();
    // Adopt an existing standalone command too. Do not touch compound
    // commands, other XRandR settings, or examples in comments.
    let managed = spans.clone();
    for found in XRANDR_LINE.find_iter(&text) {
        if managed.iter().any(|(start, end)| *start <= found.start() && found.start() < *end) {
            continue;
        }
        let Ok(args) = shlex::split_comments(found.as_str()) else { continue };
        let tail: Vec<&str> = args.iter().skip(4).map(String::as_str).collect();
        let allowed: [&[&str]; 4] = [&[], &["&"], &[">/dev/null", "2>&1"], &[">/dev/null", "2>&1", "||", "true"]];
        if args.len() >= 4 && args[1] == "--output" && args[3] == "--primary" && allowed.contains(&tail.as_slice()) {
            spans.push((found.start(), found.end()));
        }
    }
    let updated = if !spans.is_empty() {
        spans.sort();
        let mut updated = String::new();
        let mut offset = 0;
        for (index, (start, end)) in spans.iter().enumerate() {
            updated.push_str(&text[offset.min(*start)..*start]);
            if index == 0 && !block.is_empty() {
                updated.push_str(&block);
                updated.push('\n');
            }
            offset = *end;
        }
        updated.push_str(&text[offset..]);
        updated
    } else if !block.is_empty() {
        format!("{text}{}\n{block}\n", if text.ends_with('\n') { "" } else { "\n" })
    } else {
        text.clone()
    };
    if updated == text {
        return (true, None, false);
    }
    match fsx::atomic_write_with(path, updated.as_bytes(), mode, ".xinitrc.", replace) {
        Ok(()) => (true, None, true),
        Err(error) => (false, Some(error), false),
    }
}

/// `subprocess.run(..., capture_output=True, text=True, timeout=…)`.
pub struct Run {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

/// `Err(None)` = FileNotFoundError, `Err(Some(text))` = SubprocessError text.
pub fn run(argv: &[&str], timeout: Duration) -> Result<Run, Option<String>> {
    let mut child = match Command::new(argv[0]).args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(None),
        Err(error) => return Err(Some(suite_core::pyerr::os_error_text(&error))),
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let read = |stream: Option<std::process::ChildStdout>| std::thread::spawn(move || {
        let mut text = Vec::new();
        if let Some(mut stream) = stream {
            let _ = std::io::Read::read_to_end(&mut stream, &mut text);
        }
        String::from_utf8_lossy(&text).into_owned()
    });
    let out = read(stdout);
    let err = std::thread::spawn(move || {
        let mut text = Vec::new();
        if let Some(mut stream) = stderr {
            let _ = std::io::Read::read_to_end(&mut stream, &mut text);
        }
        String::from_utf8_lossy(&text).into_owned()
    });
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let quoted: Vec<String> = argv.iter().map(|a| format!("'{a}'")).collect();
                return Err(Some(format!("Command '[{}]' timed out after {} seconds", quoted.join(", "), timeout.as_secs())));
            }
            Err(error) => return Err(Some(suite_core::pyerr::os_error_text(&error))),
        }
    };
    use std::os::unix::process::ExitStatusExt;
    let code = status.code().unwrap_or_else(|| -status.signal().unwrap_or(0));
    Ok(Run { status: code, stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() })
}

/// XRandR's primary output, or None when XWayland has none.
pub fn get_xwayland_primary_output() -> Option<String> {
    let result = run(&["xrandr", "--query", "--current"], Duration::from_secs(2)).ok()?;
    if result.status != 0 {
        return None;
    }
    fsx::splitlines(&result.stdout).iter().find_map(|line| XRANDR_PRIMARY.captures(line).map(|c| c[1].to_owned()))
}

/// Apply the selected primary output to the current XWayland session.
pub fn apply_xwayland_primary_output(primary_output: Option<&str>) -> (bool, Option<String>) {
    let current = get_xwayland_primary_output();
    let primary = primary_output.map(|o| o.replace('\n', "").trim().to_owned()).filter(|o| !o.is_empty());
    if primary == current {
        return (true, None);
    }
    let command: Vec<&str> = match (&primary, &current) {
        (Some(output), _) => vec!["xrandr", "--output", output, "--primary"],
        (None, Some(current)) => vec!["xrandr", "--output", current, "--noprimary"],
        (None, None) => return (true, None),
    };
    let result = match run(&command, Duration::from_secs(2)) {
        Ok(result) => result,
        Err(None) => return (false, Some("xrandr is not installed".into())),
        Err(Some(error)) => return (false, Some(error)),
    };
    if result.status != 0 {
        let detail = [result.stderr.trim(), result.stdout.trim()].into_iter().find(|s| !s.is_empty())
            .map(String::from).unwrap_or_else(|| format!("xrandr exited with status {}", result.status));
        return (false, Some(detail));
    }
    let actual = get_xwayland_primary_output();
    if actual != primary {
        return (false, Some(tr!("XRandR reports {value1} after applying {value2}",
            value1 = actual.unwrap_or_else(|| "no primary output".into()),
            value2 = primary.unwrap_or_else(|| "no primary output".into()))));
    }
    (true, None)
}
