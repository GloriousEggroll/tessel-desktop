//! tessel-clipboard: keep copied content after the application that copied it closes.
//!
//!   tessel-clipboard [--verbose] [--settings FILE]   run (the tessel-clipboard.service unit)
//!   tessel-clipboard --check                         report the data-control protocol, exit 1 without one
use std::os::fd::AsRawFd;
use std::path::PathBuf;

const USAGE: &str = "Usage: tessel-clipboard [--verbose] [--settings FILE] | --check | --help";

/// One tessel-clipboard per Wayland display: the lock lives in the runtime dir, named after the display.
fn single_instance() -> Result<std::fs::File, String> {
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    let name = display.rsplit('/').next().unwrap_or("wayland-0").to_owned();
    let path = tessel_core::paths::runtime_dir().join(format!("tessel-clipboard-{name}.lock"));
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path)
        .map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
    // SAFETY: flock on a descriptor we own.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(format!("tessel-clipboard is already running on {display}."));
    }
    Ok(file)
}

fn main() {
    let mut verbose = false;
    let mut settings: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--verbose" | "-v" => verbose = true,
            "--settings" => match args.next() {
                Some(path) => settings = Some(PathBuf::from(path)),
                None => {
                    eprintln!("{USAGE}");
                    std::process::exit(2);
                }
            },
            "--check" => match tessel_clipboard::daemon::check() {
                Ok(protocol) => {
                    println!("tessel-clipboard: clipboard protocol {protocol}");
                    std::process::exit(0);
                }
                Err(error) => {
                    eprintln!("tessel-clipboard: {error}");
                    std::process::exit(1);
                }
            },
            "--help" | "-h" => {
                println!("{USAGE}");
                return;
            }
            other => {
                eprintln!("tessel-clipboard: unknown argument {other}\n{USAGE}");
                std::process::exit(2);
            }
        }
    }
    let _lock = match single_instance() {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("tessel-clipboard: {error}");
            std::process::exit(0);
        }
    };
    let store = tessel_clipboard_settings::Store::new(settings);
    if let Err(error) = tessel_clipboard::daemon::run(store, verbose) {
        eprintln!("tessel-clipboard: {error}");
        std::process::exit(1);
    }
}
