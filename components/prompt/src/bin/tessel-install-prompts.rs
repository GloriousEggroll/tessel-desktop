//! Read-only preview of the prompt's installation: lists the files
//! that would change (launcher, desktop entry, rc.xml). There is no --apply.
//! Usage: tessel-install-prompts [--home DIR] [--config-home DIR] [--binary PATH] [--show]

use std::path::PathBuf;
use tessel_prompt::install;

fn main() {
    let mut home = tessel_core::paths::home();
    let mut config_home = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from);
    let mut binary = None;
    let mut show = false;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--home") => home = args.next().map(PathBuf::from).unwrap_or(home),
            Some("--config-home") => config_home = args.next().map(PathBuf::from),
            Some("--binary") => binary = args.next().map(PathBuf::from),
            Some("--show") => show = true,
            _ => {
                eprintln!("usage: tessel-install-prompts [--home DIR] [--config-home DIR] [--binary PATH] [--show]");
                std::process::exit(2);
            }
        }
    }
    let files = match install::plan(&home, config_home.as_deref(), binary.as_deref()) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let changes = install::pending(&files);
    for file in &changes {
        println!("{}", file.path.display());
        if show {
            println!("--- mode {:o}\n{}", file.mode, String::from_utf8_lossy(&file.data));
        }
    }
    println!("{} files prepared. No services or autostart entries are added.", changes.len());
}
