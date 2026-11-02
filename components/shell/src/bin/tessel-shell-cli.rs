//! `tessel-shell-cli`: the Alt+Tab / hot-corner client of the running `tessel-shell`.
//! No GTK: it only talks to `$XDG_RUNTIME_DIR/tessel-shell.sock`.
fn main() {
    let mut args = std::env::args();
    let prog = args.next().map(|p| std::path::Path::new(&p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(p))
        .unwrap_or_else(|| "tessel-shell-cli".into());
    let argv: Vec<String> = args.collect();
    let outcome = tessel_shell::cli::keyboard_main(&prog, &argv, |p| tessel_shell::cli::cli_request(p, std::time::Duration::from_secs(2)));
    std::process::exit(tessel_shell::cli::emit(&outcome));
}
