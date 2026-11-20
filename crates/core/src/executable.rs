//! What opening a local file should do when it is a program (issue #25):
//! native programs and AppImages run instead of opening in an archive
//! manager, and executable scripts ask whether to run or open them.
//! Shared by Tessel Files and the desktop icons.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// How many leading bytes [`classify`] reads.
pub const HEADER_LENGTH: usize = 64;

/// What a double click on a local file should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A native (ELF) program, AppImages included, that may run: run it.
    Program,
    /// An AppImage without the execute permission: offer to allow it and run it.
    AppImageNotExecutable,
    /// A `#!` text file with the execute permission: ask Run / Run in Terminal / Open.
    Script,
    /// Everything else: open it with its default application.
    Other,
}

/// Classify a file from its permissions and at most its first
/// [`HEADER_LENGTH`] bytes. Directories, `.desktop` files, unreadable and
/// non-regular files are [`Kind::Other`].
pub fn classify(path: &Path) -> Kind {
    let Ok(metadata) = std::fs::metadata(path) else { return Kind::Other };
    if !metadata.is_file() || has_suffix(path, ".desktop") {
        return Kind::Other;
    }
    let Ok(file) = std::fs::File::open(path) else { return Kind::Other };
    let mut header = Vec::with_capacity(HEADER_LENGTH);
    if file.take(HEADER_LENGTH as u64).read_to_end(&mut header).is_err() {
        return Kind::Other;
    }
    classify_header(&header, crate::shell::is_executable(path), has_suffix(path, ".appimage"))
}

/// The pure decision behind [`classify`].
pub fn classify_header(header: &[u8], executable: bool, appimage_name: bool) -> Kind {
    if header.starts_with(b"\x7fELF") {
        if executable {
            return Kind::Program;
        }
        // AppImage type 1 and 2 put "AI" and the type at offset 8.
        let signed = header.len() >= 11 && &header[8..10] == b"AI" && matches!(header[10], 1 | 2);
        return if signed || appimage_name { Kind::AppImageNotExecutable } else { Kind::Other };
    }
    if executable && header.starts_with(b"#!") && !header.contains(&0) {
        return Kind::Script;
    }
    Kind::Other
}

fn has_suffix(path: &Path, suffix: &str) -> bool {
    path.file_name().is_some_and(|name| {
        let name = std::os::unix::ffi::OsStrExt::as_bytes(name);
        name.len() > suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
    })
}

/// The mode with the execute bits its read bits allow: always u+x, and g+x /
/// o+x only where group / others may read.
pub fn executable_mode(mode: u32) -> u32 {
    mode | 0o100 | ((mode & 0o040) >> 2) | ((mode & 0o004) >> 2)
}

/// Give `path` the execute bits of [`executable_mode`]. The error text is
/// the system's message (shown to the user as is).
pub fn allow_execution(path: &Path) -> Result<(), String> {
    let mode = std::fs::metadata(path).map_err(|e| e.to_string())?.permissions().mode();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(executable_mode(mode) & 0o7777)).map_err(|e| e.to_string())
}

/// `terminal` (a terminal's argv that already opens in the right folder)
/// extended to run `command` in it, by the terminal's executable file name.
/// Terminals that take the command as their trailing arguments get it after
/// `--` or as is; others after `-e` / `-x`, the convention of xterm and
/// `x-terminal-emulator`.
pub fn terminal_with_command(name: &str, terminal: &[String], command: &[String]) -> Vec<String> {
    let mut argv = terminal.to_vec();
    match name {
        "xdg-terminal-exec" | "xdg-terminal" | "kitty" | "foot" | "footclient" => {}
        "ptyxis" | "kgx" | "gnome-terminal" | "wezterm" => argv.push("--".into()),
        "xfce4-terminal" | "tilix" | "terminator" => argv.push("-x".into()),
        "ghostty" => {
            // `+new-window` is an action that takes no command.
            argv.retain(|a| a != "+new-window");
            argv.push("-e".into());
        }
        _ => argv.push("-e".into()),
    }
    argv.extend(command.iter().cloned());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Scratch under the repository's own `target/` (never `$TMPDIR`).
    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch").join(format!("tessel-core-executable-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file(dir: &Path, name: &str, content: &[u8], mode: u32) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    fn elf(appimage_type: Option<u8>) -> Vec<u8> {
        let mut bytes = b"\x7fELF\x02\x01\x01\x00".to_vec();
        match appimage_type {
            Some(kind) => bytes.extend([b'A', b'I', kind]),
            None => bytes.extend([0, 0, 0]),
        }
        bytes.resize(4096, 0);
        bytes
    }

    #[test]
    fn programs_appimages_and_scripts_by_content_and_permission() {
        let dir = scratch("classify");
        assert_eq!(classify(&file(&dir, "tool", &elf(None), 0o755)), Kind::Program);
        assert_eq!(classify(&file(&dir, "Game.AppImage", &elf(Some(2)), 0o755)), Kind::Program);
        assert_eq!(classify(&file(&dir, "game-renamed", &elf(Some(2)), 0o644)), Kind::AppImageNotExecutable, "detected by content");
        assert_eq!(classify(&file(&dir, "old-type", &elf(Some(1)), 0o600)), Kind::AppImageNotExecutable);
        assert_eq!(classify(&file(&dir, "Named.appIMAGE", &elf(None), 0o644)), Kind::AppImageNotExecutable, "unsigned, but named .appimage");
        assert_eq!(classify(&file(&dir, "other-type", &elf(Some(3)), 0o644)), Kind::Other);
        assert_eq!(classify(&file(&dir, "library.so", &elf(None), 0o644)), Kind::Other, "other ELF files without the permission");
        assert_eq!(classify(&file(&dir, "Fake.AppImage", b"not an elf", 0o644)), Kind::Other, "the name alone is not enough");
        assert_eq!(classify(&file(&dir, "run.sh", b"#!/bin/sh\necho hi\n", 0o755)), Kind::Script);
        assert_eq!(classify(&file(&dir, "plain.sh", b"#!/bin/sh\necho hi\n", 0o644)), Kind::Other, "scripts without the permission open");
        assert_eq!(classify(&file(&dir, "binary", b"#!\0\x01\x02", 0o755)), Kind::Other, "not a text file");
        assert_eq!(classify(&file(&dir, "notes.txt", b"hello", 0o755)), Kind::Other);
        assert_eq!(classify(&file(&dir, "empty", b"", 0o755)), Kind::Other);
        assert_eq!(classify(&file(&dir, "launcher.desktop", b"#!/usr/bin/env xdg-open\n[Desktop Entry]\n", 0o755)), Kind::Other, "desktop files keep their handling");
        std::fs::create_dir(dir.join("folder")).unwrap();
        std::fs::set_permissions(dir.join("folder"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(classify(&dir.join("folder")), Kind::Other);
        assert_eq!(classify(&dir.join("missing")), Kind::Other);
        std::os::unix::fs::symlink(dir.join("tool"), dir.join("link")).unwrap();
        assert_eq!(classify(&dir.join("link")), Kind::Program, "links to programs run their target");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn header_rules() {
        assert_eq!(classify_header(b"\x7fELF", true, false), Kind::Program);
        assert_eq!(classify_header(b"\x7fELF", false, true), Kind::AppImageNotExecutable);
        assert_eq!(classify_header(b"\x7fELF\x02\x01\x01\x00AI", false, false), Kind::Other, "truncated signature");
        assert_eq!(classify_header(b"\x7fEL", true, false), Kind::Other);
        assert_eq!(classify_header(b"#!", true, false), Kind::Script);
        assert_eq!(classify_header(b" #!/bin/sh", true, false), Kind::Other);
    }

    #[test]
    fn execute_bits_follow_read_bits() {
        assert_eq!(executable_mode(0o644), 0o755);
        assert_eq!(executable_mode(0o600), 0o700);
        assert_eq!(executable_mode(0o640), 0o750);
        assert_eq!(executable_mode(0o604), 0o705);
        assert_eq!(executable_mode(0o200), 0o300);
        assert_eq!(executable_mode(0o100644), 0o100755, "file type bits are kept");
        let dir = scratch("chmod");
        let path = file(&dir, "Game.AppImage", &elf(Some(2)), 0o640);
        allow_execution(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777, 0o750);
        assert_eq!(classify(&path), Kind::Program);
        assert!(allow_execution(&dir.join("missing")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn terminals_run_a_command() {
        let s = |items: &[&str]| items.iter().map(|i| i.to_string()).collect::<Vec<_>>();
        let script = s(&["/home/u/My Scripts/run.sh"]);
        assert_eq!(terminal_with_command("xdg-terminal-exec", &s(&["/usr/bin/xdg-terminal-exec", "--dir=/d"]), &script), s(&["/usr/bin/xdg-terminal-exec", "--dir=/d", "/home/u/My Scripts/run.sh"]));
        assert_eq!(terminal_with_command("ptyxis", &s(&["ptyxis", "--new-window", "--working-directory", "/d"]), &script)[4..], s(&["--", "/home/u/My Scripts/run.sh"]));
        assert_eq!(terminal_with_command("konsole", &s(&["konsole", "--workdir", "/d"]), &script)[3..], s(&["-e", "/home/u/My Scripts/run.sh"]));
        assert_eq!(terminal_with_command("xfce4-terminal", &s(&["xfce4-terminal"]), &script)[1..], s(&["-x", "/home/u/My Scripts/run.sh"]));
        assert_eq!(terminal_with_command("foot", &s(&["foot", "--working-directory", "/d"]), &script)[3..], script);
        assert_eq!(terminal_with_command("ghostty", &s(&["ghostty", "+new-window"]), &script), s(&["ghostty", "-e", "/home/u/My Scripts/run.sh"]));
        assert_eq!(terminal_with_command("xterm", &s(&["xterm"]), &script), s(&["xterm", "-e", "/home/u/My Scripts/run.sh"]));
    }
}
