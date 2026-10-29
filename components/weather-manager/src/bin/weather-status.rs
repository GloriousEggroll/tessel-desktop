//! `weather_status.py`: current weather as JSON lines for the desktop panel.
//! No GTK, no i18n (the panel translates), shared cache; HTTPS only when the
//! shared TTL expires. `--watch` re-reads config/cache every 2 s and prints
//! only changes; exits 0 on SIGINT/SIGTERM or a closed stdout.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use weather_manager::backend::{ConfigStore, WeatherClient};
use weather_manager::compat::argparse::{prog_name, Opt, Parser};
use weather_manager::status::{line, status_payload};

static STOPPING: AtomicBool = AtomicBool::new(false);

extern "C" fn stop(_: libc::c_int) { STOPPING.store(true, Ordering::SeqCst); }

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let parser = Parser { prog: prog_name(argv.first().map(String::as_str).unwrap_or(""), "weather-status"),
        description: Some("Current weather as JSON for the desktop panel"),
        options: vec![Opt::flag("--watch", None, false), Opt::value("--config", "CONFIG", Some("Use a different configuration file")),
                      Opt::value("--cache-dir", "CACHE_DIR", Some("Use a different cache folder"))] };
    let args = match parser.parse(&argv[1..]) {
        Ok(args) => args,
        Err((code, text, stdout)) => {
            if stdout { print!("{text}") } else { eprint!("{text}") }
            std::process::exit(code);
        }
    };
    for signal in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: the handler only stores to an atomic.
        unsafe { libc::signal(signal, stop as extern "C" fn(libc::c_int) as libc::sighandler_t) };
    }
    // SAFETY: restore default SIGPIPE so EPIPE surfaces as a write error.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
    let store = Arc::new(ConfigStore::new(args.value("--config").map(PathBuf::from)));
    let client = WeatherClient::new(store.clone(), args.value("--cache-dir").map(PathBuf::from), None, None);
    let mut previous: Option<String> = None;
    while !STOPPING.load(Ordering::SeqCst) {
        let config = store.load();
        let snapshot = match client.read(Some(&config), false) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        };
        let mut data = status_payload(&snapshot);
        if let Some(error) = store.error() {
            let tooltip = format!("{}\nSettings could not be read: {error}", data["tooltip"].as_str().unwrap_or(""));
            data["tooltip"] = tooltip.into();
        }
        let serialized = line(&data);
        if previous.as_deref() != Some(serialized.as_str()) {
            let mut stdout = std::io::stdout().lock();
            if writeln!(stdout, "{serialized}").and_then(|_| stdout.flush()).is_err() {
                std::process::exit(0);
            }
            previous = Some(serialized);
        }
        if !args.flag("--watch") {
            return;
        }
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until && !STOPPING.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
