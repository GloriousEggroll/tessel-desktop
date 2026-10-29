//! `waytile` / `tiling_manager.py main()`: one mutually exclusive mode.

use crate::client::{self, status_text};
use crate::compat::{prog, ArgSpec, Outcome};
use crate::config::TilingConfig;
use crate::outline::OutlineSync;
use std::io::Write;
use std::sync::Arc;

pub const SPEC: ArgSpec = ArgSpec {
    description: "Waytile: optional automatic window tiling for the labwc desktop.",
    modes: &["toggle", "enable", "disable", "restore", "status", "watch", "settings", "popup"],
};

extern "C" fn interrupted(_: libc::c_int) {
    // KeyboardInterrupt in the watch loop: `return 0`.
    unsafe { libc::_exit(0) }
}

/// `--watch`: every second print the status JSON line when it changed, and
/// keep the focus outline in step with the palette. Ends on SIGINT or a
/// closed pipe (exit 0).
pub fn watch(interval: std::time::Duration) -> i32 {
    // Python only turns SIGINT into KeyboardInterrupt when it is not ignored
    // (background jobs of non-interactive shells inherit SIG_IGN).
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGINT, std::ptr::null(), &mut current) == 0 && current.sa_sigaction != libc::SIG_IGN {
            libc::signal(libc::SIGINT, interrupted as *const () as libc::sighandler_t);
        }
    }
    let mut previous = None;
    let mut outline = OutlineSync::new(None, None, Arc::new(|command: &str| client::request(command)));
    let mut stdout = std::io::stdout().lock();
    loop {
        let current = client::status();
        let _ = outline.sync(&current);
        if previous.as_ref() != Some(&current) {
            let line = status_text(&current) + "\n";
            if stdout.write_all(line.as_bytes()).and_then(|_| stdout.flush()).is_err() {
                return 0;
            }
            previous = Some(current);
        }
        std::thread::sleep(interval);
    }
}

/// `--toggle | --enable | --disable | --restore`; errors go to stderr, exit 1.
pub fn change(mode: &str) -> i32 {
    let config = TilingConfig::default();
    let result = (|| -> crate::config::Result<()> {
        if mode == "restore" {
            // Restore is invoked only by labwc autostart, never a globally
            // enabled systemd or XDG service in another desktop.
            if config.wanted()? {
                config.set_enabled(true, false)?;
            }
        } else {
            let state = client::status();
            if !crate::compat::truthy(state.get("ready")) {
                let error = state.get("error").map(crate::compat::py_str).unwrap_or_default();
                return Err(error.into());
            }
            let enabled = match mode {
                "toggle" => !crate::compat::truthy(state.get("enabled")),
                other => other == "enable",
            };
            config.set_enabled(enabled, true)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

pub fn main(argv: &[String]) -> i32 {
    let name = prog(argv.first().map(String::as_str).unwrap_or("tiling_manager.py"));
    let parsed = match SPEC.parse(argv.get(1..).unwrap_or(&[]), false) {
        Outcome::Run(parsed) => parsed,
        Outcome::Help => {
            print!("{}", SPEC.help(&name));
            return 0;
        }
        Outcome::Error(message) => {
            eprint!("{}", SPEC.error_text(&name, &message));
            return 2;
        }
    };
    match parsed.mode {
        Some("status") => {
            println!("{}", status_text(&client::status()));
            0
        }
        Some("watch") => watch(std::time::Duration::from_secs(1)),
        Some(mode @ ("toggle" | "enable" | "disable" | "restore")) => change(mode),
        _ => crate::ui::run(&parsed.popup_edge, parsed.popup_offset.max(0) as i32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_matches_argparse() {
        assert_eq!(SPEC.usage("tiling_manager.py"),
                   "usage: tiling_manager.py [-h] [--toggle | --enable | --disable | --restore |\n                         --status | --watch | --settings | --popup]\n                         [--popup-edge {top,bottom,left,right}]\n                         [--popup-offset POPUP_OFFSET]\n");
        assert!(SPEC.help("tiling_manager.py").contains("\nWaytile: optional automatic window tiling for the labwc desktop.\n\noptions:\n  -h, --help            show this help message and exit\n  --toggle\n"));
    }

    #[test]
    fn modes_and_errors() {
        let run = |argv: &[&str]| match SPEC.parse(argv, false) {
            Outcome::Run(p) => p,
            other => panic!("{other:?}"),
        };
        assert_eq!(run(&["--wat"]).mode, Some("watch"));
        assert_eq!(run(&["--popup-offset", "-5"]).popup_offset, -5, "clamped later, never an error");
        assert_eq!(run(&["--settings", "--popup-edge", "left", "--popup-offset", "40"]).popup_edge, "left");
        assert_eq!(SPEC.parse(&["--status", "--toggle"], false),
                   Outcome::Error("argument --toggle: not allowed with argument --status".into()));
        assert_eq!(SPEC.parse(&["--popup-offset", "x"], false),
                   Outcome::Error("argument --popup-offset: invalid int value: 'x'".into()));
        assert_eq!(SPEC.parse(&["--popup-edge", "x"], false),
                   Outcome::Error("argument --popup-edge: invalid choice: 'x' (choose from 'top', 'bottom', 'left', 'right')".into()));
        assert_eq!(SPEC.parse(&["--re"], false), Outcome::Run(crate::compat::Parsed { mode: Some("restore"), ..Default::default() }));
        assert_eq!(SPEC.parse(&["--pop"], false),
                   Outcome::Error("ambiguous option: --pop could match --popup, --popup-edge, --popup-offset".into()));
    }
}
