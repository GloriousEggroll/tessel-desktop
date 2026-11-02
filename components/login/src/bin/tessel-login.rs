//! login_ui.py entry point: `tessel-login [--preview] [--config CONFIG]`.
use std::path::PathBuf;

use gtk::prelude::*;
use tessel_locking::login_model::{Appearance, CONFIG};
use tessel_login::ui::{App, Options};

const USAGE: &str = "[-h] [--preview] [--config CONFIG]";

fn main() {
    let mut args = std::env::args();
    let argv0 = args.next().unwrap_or_else(|| "tessel-login".into());
    let prog = std::path::Path::new(&argv0).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (mut preview, mut config) = (false, PathBuf::from(CONFIG));
    let mut unknown = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("usage: {prog} {USAGE}\n\nTessel Login — Wayland login greeter\n\noptions:\n  -h, --help       show this help message and exit\n  --preview        Windowed preview; no login or power actions\n  --config CONFIG");
                return;
            }
            "--preview" => preview = true,
            "--config" => match args.next() {
                Some(value) => config = value.into(),
                None => usage_error(&prog, "argument --config: expected one argument"),
            },
            other if other.starts_with("--config=") => config = other["--config=".len()..].into(),
            other => unknown.push(other.to_owned()),
        }
    }
    if !unknown.is_empty() {
        usage_error(&prog, &format!("unrecognized arguments: {}", unknown.join(" ")));
    }
    // Also block same-UID ptrace/core inspection while handling credentials.
    if let Err(message) = tessel_login::hardening::protect_process() {
        eprintln!("{message}");
        std::process::exit(1);
    }
    let appearance = match Appearance::load(Some(&config)) {
        Ok(appearance) => appearance,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };
    let app = App::new(Options { preview, appearance: Some(appearance), ..Options::default() });
    let status = app.application().run_with_args(&[argv0]);
    if let Some(error) = app.0.startup_error.borrow().as_ref() {
        eprintln!("{error}");
        std::process::exit(1);
    }
    std::process::exit(i32::from(status));
}

fn usage_error(prog: &str, message: &str) -> ! {
    eprintln!("usage: {prog} {USAGE}\n{prog}: error: {message}");
    std::process::exit(2);
}
