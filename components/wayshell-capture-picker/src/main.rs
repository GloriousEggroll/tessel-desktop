//! `wayshell-capture-picker`: reads dmenu records on stdin, prints exactly the
//! chosen record (or nothing) on stdout, exits 0, or 1 on error.

use std::io::Write;
use std::process::ExitCode;
use wayshell_capture_picker::{entry, gui, output, selection};

fn main() -> ExitCode {
    // Before anything can print, and before any thread or GTK exists.
    entry::parent_pid();
    let mut stdout = entry::reserve_stdout();
    entry::configure_portal_environment();
    suite_core::i18n::initialize();
    let argv_len = std::env::args_os().count();
    let mut stdin = std::io::stdin().lock();
    let mut stderr = std::io::stderr();
    let mut requested = |session: &str, sources: &[_]| selection::requested_selection(session, sources);
    let mut choose = |sources: &[_], remote: u8, _: &mut dyn Write| gui::choose(sources, remote);
    let mut report = |session: &str, outputs: &[String]| selection::report_selection(session, outputs);
    let mut outputs = |source: &_| output::selected_outputs(Some(source));
    let status = entry::run(argv_len, &mut stdin, &mut stdout, &mut stderr, &|name| std::env::var(name).ok(), entry::Hooks {
        requested_selection: &mut requested, choose: &mut choose, report_selection: &mut report, selected_outputs: &mut outputs,
    });
    ExitCode::from(status as u8)
}
