//! `wayshell-cli`: drop-in for wayshell_cli.py (the Alt+Tab / hot-corner client).
//! No GTK: it only talks to `$XDG_RUNTIME_DIR/wayshell.sock`.
fn main() {
    let mut args = std::env::args();
    let prog = args.next().map(|p| std::path::Path::new(&p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(p))
        .unwrap_or_else(|| "wayshell_cli.py".into());
    let argv: Vec<String> = args.collect();
    let outcome = wayshell::cli::keyboard_main(&prog, &argv, |p| wayshell::cli::cli_request(p, std::time::Duration::from_secs(2)));
    std::process::exit(wayshell::cli::emit(&outcome));
}
