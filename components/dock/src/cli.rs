//! Command-line rules of tessel-dock.py (contract §1 "CLI").
//!
//! Public API (pure, no GTK):
//! - [`OPTIONS`] — every option the primary instance accepts.
//! - [`LAUNCHER_OPTIONS`] — options that make the process a GApplication
//!   `IS_LAUNCHER` (`--check --quit --menu --menu-hotkey --toggle-desktop
//!   --desktop-status`): they never start a dock when none is running.
//! - [`is_launcher(argv)`] — the `__main__` test (any argv item, including
//!   `argv[0]`, equal to one of [`LAUNCHER_OPTIONS`], exactly like
//!   `any(option in sys.argv ...)`).
//! - [`Command`] / [`parse(args)`] — `do_command_line` dispatch order:
//!   unknown option (first one) → `Unknown(arg)` (stderr
//!   `Unknown option: X\n`, exit 2), then `--check`, `--desktop-status`,
//!   `--quit`, `--settings`, `--menu`/`--menu-hotkey`, `--toggle-desktop`,
//!   else `None` (start / keep running, exit 0).
//! - [`unknown_option_message`] — the translated stderr text.

/// `do_command_line`'s accepted options.
pub const OPTIONS: [&str; 7] = ["--settings", "--check", "--quit", "--menu", "--menu-hotkey", "--toggle-desktop", "--desktop-status"];

/// Options for which the process must never become the primary instance.
pub const LAUNCHER_OPTIONS: [&str; 6] = ["--check", "--quit", "--menu", "--menu-hotkey", "--toggle-desktop", "--desktop-status"];

/// `any(option in sys.argv for option in LAUNCHER_OPTIONS)`.
pub fn is_launcher<S: AsRef<str>>(argv: &[S]) -> bool {
    argv.iter().any(|arg| LAUNCHER_OPTIONS.contains(&arg.as_ref()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// First unknown argument (exit 2).
    Unknown(String),
    Check,
    DesktopStatus,
    Quit,
    Settings,
    /// `--menu` (false) or `--menu-hotkey` (true).
    Menu { hotkey: bool },
    ToggleDesktop,
    /// No option: start the dock / nothing to do (exit 0).
    None,
}

/// `do_command_line(args)` where `args` excludes `argv[0]`.
pub fn parse<S: AsRef<str>>(args: &[S]) -> Command {
    if let Some(unknown) = args.iter().map(AsRef::as_ref).find(|arg| !OPTIONS.contains(arg)) {
        return Command::Unknown(unknown.to_owned());
    }
    let has = |flag: &str| args.iter().any(|arg| arg.as_ref() == flag);
    if has("--check") {
        Command::Check
    } else if has("--desktop-status") {
        Command::DesktopStatus
    } else if has("--quit") {
        Command::Quit
    } else if has("--settings") {
        Command::Settings
    } else if has("--menu") || has("--menu-hotkey") {
        Command::Menu { hotkey: has("--menu-hotkey") }
    } else if has("--toggle-desktop") {
        Command::ToggleDesktop
    } else {
        Command::None
    }
}

/// `tr('Unknown option: {value1}\n', value1=arg)`.
pub fn unknown_option_message(arg: &str) -> String {
    tessel_core::tr!("Unknown option: {value1}\n", value1 = arg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_follows_python_order() {
        assert_eq!(parse::<&str>(&[]), Command::None);
        assert_eq!(parse(&["--bogus", "--check"]), Command::Unknown("--bogus".into()));
        assert_eq!(parse(&["--check", "--bogus"]), Command::Unknown("--bogus".into()));
        assert_eq!(parse(&["--quit", "--check"]), Command::Check);
        assert_eq!(parse(&["--quit", "--desktop-status"]), Command::DesktopStatus);
        assert_eq!(parse(&["--settings", "--quit"]), Command::Quit);
        assert_eq!(parse(&["--menu", "--settings"]), Command::Settings);
        assert_eq!(parse(&["--menu"]), Command::Menu { hotkey: false });
        assert_eq!(parse(&["--menu", "--menu-hotkey"]), Command::Menu { hotkey: true });
        assert_eq!(parse(&["--toggle-desktop", "--menu"]), Command::Menu { hotkey: false });
        assert_eq!(parse(&["--toggle-desktop"]), Command::ToggleDesktop);
    }

    #[test]
    fn launcher_options_never_start_a_dock() {
        assert!(!is_launcher(&["tessel-dock"]));
        assert!(!is_launcher(&["tessel-dock", "--settings"]));
        for option in LAUNCHER_OPTIONS {
            assert!(is_launcher(&["tessel-dock", option]), "{option}");
        }
        assert!(is_launcher(&["tessel-dock", "--settings", "--menu"]));
    }

    #[test]
    fn unknown_option_text() {
        assert_eq!(unknown_option_message("-x"), "Unknown option: -x\n");
    }
}
