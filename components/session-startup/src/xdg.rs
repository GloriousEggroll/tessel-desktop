//! Port of `configure_xdg_autostart.py`: labwc's systemd-backed XDG application autostart.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::cli::{Parsed, Parser};
use crate::fsops::{self, Snapshot};
use crate::pystr::rstrip;
use crate::session::Error;

pub const TARGET: &str = "labwc-xdg-autostart.target";
/// The target unit itself (`data/units/`).
pub const TARGET_UNIT: &str = include_str!("../../../data/units/labwc-xdg-autostart.target");
pub const ENV_BEGIN: &str = "# Begin labwc XDG session environment";
pub const ENV_END: &str = "# End labwc XDG session environment";
pub const START_BEGIN: &str = "# Begin labwc XDG application autostart";
pub const START_END: &str = "# End labwc XDG application autostart";
pub const STOP_BEGIN: &str = "# Begin labwc XDG session cleanup";
pub const STOP_END: &str = "# End labwc XDG session cleanup";

pub const ENV_BLOCK: &str = r#"# Begin labwc XDG session environment
# Keep the existing desktop first so portal backend selection is preserved.
case ":${XDG_CURRENT_DESKTOP:-}:" in
    *:labwc:*) ;;
    *) XDG_CURRENT_DESKTOP="${XDG_CURRENT_DESKTOP:+$XDG_CURRENT_DESKTOP:}labwc" ;;
esac
export XDG_CURRENT_DESKTOP
# Import the actual compositor session before regenerating autostart units.
dbus-update-activation-environment --systemd \
    DISPLAY WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_SESSION_TYPE PATH LABWC_PID \
    ${XDG_CONFIG_HOME+XDG_CONFIG_HOME} ${XDG_CONFIG_DIRS+XDG_CONFIG_DIRS}
systemctl --user daemon-reload
# End labwc XDG session environment"#;
pub const START_BLOCK: &str = "# Begin labwc XDG application autostart
# Wait for Tessel Dock's tray host before starting applications which only try
# to register their tray icon once. Type=notify makes this a readiness wait,
# even if Tessel Dock's start was already requested in the background above.
systemctl --user start tessel-dock.service
# The installed generator handles user overrides, Hidden, desktop filters,
# TryExec and Exec argument parsing. Active services are not launched twice.
systemctl --user --no-block start labwc-xdg-autostart.target
# End labwc XDG application autostart";
pub const STOP_BLOCK: &str = "# Begin labwc XDG session cleanup
# Generated apps belong to graphical-session.target; stop them before the
# compositor exits so they do not carry over into the next desktop session.
systemctl --user stop labwc-xdg-autostart.target graphical-session.target
# End labwc XDG session cleanup";

/// Replace the managed block, or insert it before `before`, or append it.
pub fn block(text: &str, begin: &str, end: &str, content: &str, before: Option<&str>) -> Result<String, Error> {
    if text.contains(begin) || text.contains(end) {
        if text.matches(begin).count() != 1 || text.matches(end).count() != 1 {
            return Err(format!("Unexpected managed block: {begin}"));
        }
        if text.find(end) < text.find(begin) {
            return Err(format!("Reversed managed block: {begin}"));
        }
        let pattern = Regex::new(&format!("(?s){}.*?{}", regex::escape(begin), regex::escape(end))).unwrap();
        return Ok(pattern.replace_all(text, regex::NoExpand(content)).into_owned());
    }
    if let Some(before) = before.filter(|before| !before.is_empty() && text.contains(before)) {
        return Ok(text.replacen(before, &format!("{content}\n\n{before}"), 1));
    }
    Ok(format!("{}\n\n{content}\n", rstrip(text)))
}

static DESKTOP_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^XDG_CURRENT_DESKTOP=([^\n]*)").unwrap());

pub fn desktop_identity(text: &str) -> String {
    if DESKTOP_LINE.is_match(text) {
        return DESKTOP_LINE
            .replace_all(text, |caps: &Captures| {
                let mut value = caps[1].to_owned();
                if !value.split(':').any(|part| part == "labwc") {
                    value = if value.is_empty() { "labwc".into() } else { format!("{value}:labwc") };
                }
                format!("XDG_CURRENT_DESKTOP={value}")
            })
            .into_owned();
    }
    format!("{}\nXDG_CURRENT_DESKTOP=labwc\n", rstrip(text))
}

static ENTRY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\[Desktop Entry\][ \t]*\n").unwrap());
static ONLY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^OnlyShowIn=([^\n]*)").unwrap());
static ONLY_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^OnlyShowIn=[^\n]*\n?").unwrap());
static HIDDEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^Hidden=[^\n]*").unwrap());
static NOT_SHOW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^NotShowIn=([^\n]*)").unwrap());

/// End of the group body: the first line start holding `[`, else the end (`(?=^\[|\Z)`).
fn group_end(text: &str, start: usize) -> usize {
    let bytes = text.as_bytes();
    (start..text.len())
        .find(|&index| bytes[index] == b'[' && (index == 0 || bytes[index - 1] == b'\n'))
        .unwrap_or(text.len())
}

/// Keep a replaced app's autostart entry available in other desktops.
pub fn exclude_labwc(text: &str) -> Result<String, Error> {
    let header = ENTRY.find(text).ok_or("Missing Desktop Entry group")?;
    let (start, end) = (header.end(), group_end(text, header.end()));
    let mut group = text[start..end].to_owned();
    if let Some(caps) = ONLY.captures(&group) {
        // OnlyShowIn and NotShowIn may not coexist. Remove only labwc from
        // an allowlist. An empty allowlist is treated as unfiltered by some
        // launchers, so use Hidden when labwc was its sole allowed desktop.
        let allowed = caps[1].to_owned();
        let existing: Vec<&str> = allowed.split(';').filter(|x| !x.is_empty()).collect();
        let values: Vec<&str> = existing.iter().copied().filter(|x| *x != "labwc").collect();
        if values.contains(&"wlroots") {
            return Err("Review OnlyShowIn=wlroots: it also matches the labwc session".into());
        }
        if !values.is_empty() {
            let line = format!("OnlyShowIn={};", values.join(";"));
            group = Regex::new(r"(?m)^OnlyShowIn=[^\n]*").unwrap().replace_all(&group, regex::NoExpand(&line)).into_owned();
        } else if !existing.is_empty() {
            group = ONLY_LINE.replace_all(&group, "").into_owned();
            if HIDDEN.is_match(&group) {
                group = HIDDEN.replace_all(&group, "Hidden=true").into_owned();
            } else {
                group = format!("{}\nHidden=true\n", rstrip(&group));
            }
        } else {
            group = ONLY_LINE.replace_all(&group, "").into_owned();
            group = format!("{}\nNotShowIn=labwc;\n", rstrip(&group));
        }
    } else if NOT_SHOW.is_match(&group) {
        group = NOT_SHOW
            .replace_all(&group, |caps: &Captures| {
                let mut values: Vec<&str> = caps.get(1).unwrap().as_str().split(';').filter(|x| !x.is_empty()).collect();
                if !values.contains(&"labwc") {
                    values.push("labwc");
                }
                format!("NotShowIn={};", values.join(";"))
            })
            .into_owned();
    } else {
        group = format!("{}\nNotShowIn=labwc;\n", rstrip(&group));
    }
    Ok(format!("{}{group}{}", &text[..start], &text[end..]))
}

pub type FileEntry = (PathBuf, Vec<u8>, u32);

pub fn plan(home: &Path, config_dirs: Option<Vec<PathBuf>>) -> Result<Vec<FileEntry>, Error> {
    let config = home.join(".config");
    let startup = config.join("labwc/autostart");
    let mut text = fsops::read_text(&startup)?;
    let anchor = "# Keep systemd's graphical session active for desktop portals.";
    if !text.contains(ENV_BEGIN) && !text.contains(anchor) {
        return Err("Review the startup script: expected session anchor was not found".into());
    }
    text = block(&text, ENV_BEGIN, ENV_END, ENV_BLOCK, Some(anchor))?;
    text = block(&text, START_BEGIN, START_END, START_BLOCK, None)?;
    let shutdown = config.join("labwc/shutdown");
    let environment = config.join("labwc/environment");
    let mut changes = vec![(startup, text.into_bytes(), 0o755)];
    let shutdown_text = block(&fsops::read_text(&shutdown)?, STOP_BEGIN, STOP_END, STOP_BLOCK, None)?;
    changes.push((shutdown, shutdown_text.into_bytes(), 0o755));
    let environment_text = desktop_identity(&fsops::read_text(&environment)?);
    changes.push((environment, environment_text.into_bytes(), 0o644));
    changes.push((config.join("systemd/user").join(TARGET), TARGET_UNIT.as_bytes().to_vec(), 0o644));
    let config_dirs = config_dirs.unwrap_or_else(|| {
        std::env::var("XDG_CONFIG_DIRS")
            .unwrap_or_else(|_| "/etc/xdg".into())
            .split(':')
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .collect()
    });
    for name in ["nm-applet.desktop", "blueman.desktop"] {
        let destination = config.join("autostart").join(name);
        let found = std::iter::once(config.clone())
            .chain(config_dirs.iter().cloned())
            .map(|dir| dir.join("autostart").join(name))
            .find(|path| path.is_file());
        if let Some(found) = found {
            changes.push((destination, exclude_labwc(&fsops::read_text(&found)?)?.into_bytes(), 0o644));
        }
    }
    Ok(changes)
}

pub fn parser(prog: &str) -> Parser {
    let usage = format!("usage: {prog} [-h] [--apply]");
    let help = format!(
        "{usage}\n\nPreview or install labwc's systemd-backed XDG application autostart.\n\noptions:\n  -h, --help  show this help message and exit\n  --apply\n"
    );
    Parser { usage, help, flags: &["--apply"], exclusive: false }
}

pub fn main(args: &[String], prog: &str, home: &Path) -> Result<i32, Error> {
    let apply = match parser(prog).parse(prog, args) {
        Parsed::Exit(code) => return Ok(code),
        Parsed::Flags(flags) => flags.contains("--apply"),
    };
    let mut changes: Vec<(PathBuf, Vec<u8>, u32, Snapshot)> = Vec::new();
    for (path, data, mut mode) in plan(home, None)? {
        let original = fsops::snapshot(&path)?;
        if let Some((_, original_mode)) = &original {
            mode = *original_mode;
        }
        if original.as_ref() == Some(&(data.clone(), mode)) {
            continue;
        }
        let old = match &original {
            Some((bytes, _)) => crate::pystr::decode(bytes)?,
            None => String::new(),
        };
        let new = crate::pystr::decode(&data)?;
        print!("{}", crate::difflib::file_diff(&old, &new, &path.display().to_string()));
        changes.push((path, data, mode, original));
    }
    println!("{} files prepared; applies on the next labwc login.", changes.len());
    if !apply || changes.is_empty() {
        return Ok(0);
    }
    let backup = home.join(".local/state/tessel/backups").join(fsops::timestamp("xdg-autostart-"));
    let mut written: Vec<(PathBuf, Snapshot)> = Vec::new();
    let result = (|| -> Result<(), Error> {
        for (path, data, mode, original) in &changes {
            if &fsops::snapshot(path)? != original {
                return Err(format!("File changed during installation: {}", path.display()));
            }
            if let Some((bytes, original_mode)) = original {
                fsops::atomic_write(&backup.join(fsops::relative_to(path, home)?), bytes, *original_mode)?;
            }
            fsops::atomic_write(path, data, *mode)?;
            written.push((path.clone(), original.clone()));
        }
        for name in ["autostart", "shutdown"] {
            fsops::run(&["sh", "-n", &home.join(".config/labwc").join(name).display().to_string()], true, None)?;
        }
        for (path, data, _, _) in &changes {
            if &fsops::read_bytes(path)? != data {
                return Err("Installed file verification failed".into());
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        for (path, original) in written.iter().rev() {
            match original {
                Some((data, mode)) => fsops::atomic_write(path, data, *mode)?,
                None => fsops::unlink_missing_ok(path)?,
            }
        }
        return Err(error);
    }
    println!("Installed. No running applications were started or stopped.");
    println!("Backup: {}", backup.display());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_identity_preserves_portal_priority_and_is_idempotent() {
        let before = "# Session\nXDG_CURRENT_DESKTOP=wlroots\nOTHER=unchanged\n";
        let after = desktop_identity(before);
        assert!(after.contains("XDG_CURRENT_DESKTOP=wlroots:labwc\n"));
        assert!(after.contains("OTHER=unchanged\n"));
        assert_eq!(desktop_identity(&after), after);
        assert_eq!(desktop_identity("A=1\n\n"), "A=1\nXDG_CURRENT_DESKTOP=labwc\n");
    }

    #[test]
    fn invalid_managed_block_is_not_silently_overwritten() {
        for text in [ENV_BEGIN.to_owned(), ENV_END.to_owned(), format!("{ENV_END}\n{ENV_BEGIN}")] {
            assert!(block(&text, ENV_BEGIN, ENV_END, ENV_BLOCK, None).is_err(), "{text}");
        }
    }

    #[test]
    fn block_replaces_inserts_before_anchor_or_appends() {
        let text = format!("a\n{ENV_BEGIN}\nold\n{ENV_END}\nb\n");
        assert_eq!(block(&text, ENV_BEGIN, ENV_END, "NEW $1 \\", None).unwrap(), "a\nNEW $1 \\\nb\n");
        assert_eq!(block("x\nANCHOR\n", ENV_BEGIN, ENV_END, "C", Some("ANCHOR")).unwrap(), "x\nC\n\nANCHOR\n");
        assert_eq!(block("x\n\n", ENV_BEGIN, ENV_END, "C", None).unwrap(), "x\n\nC\n");
    }

    #[test]
    fn app_exclusion_preserves_other_desktops_and_action_groups() {
        let original = "[Desktop Entry]\nType=Application\nName=Example\nExec=example\nNotShowIn=KDE;GNOME;\n\n[Desktop Action Open]\nName=Open\nExec=example --open\n";
        let updated = exclude_labwc(original).unwrap();
        assert!(updated.contains("NotShowIn=KDE;GNOME;labwc;\n"));
        assert!(updated.ends_with("[Desktop Action Open]\nName=Open\nExec=example --open\n"));
        assert_eq!(exclude_labwc(&updated).unwrap(), updated);
        let restricted = exclude_labwc("[Desktop Entry]\nOnlyShowIn=labwc;KDE;\n").unwrap();
        assert!(restricted.contains("OnlyShowIn=KDE;\n"));
        assert!(!restricted.contains("NotShowIn="));
        let labwc_only = exclude_labwc("[Desktop Entry]\nOnlyShowIn=labwc;\n").unwrap();
        assert!(labwc_only.contains("Hidden=true\n"));
        assert!(!labwc_only.contains("OnlyShowIn="));
        let unfiltered = exclude_labwc("[Desktop Entry]\nOnlyShowIn=;\n").unwrap();
        assert!(unfiltered.contains("NotShowIn=labwc;\n"));
        assert!(!unfiltered.contains("Hidden=true"));
        assert!(exclude_labwc("[Desktop Entry]\nOnlyShowIn=wlroots;\n").is_err());
        assert!(exclude_labwc("Name=x\n").is_err());
    }
}
