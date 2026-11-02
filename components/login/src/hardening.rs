//! Process hardening shared by the greeter (login_ui.py main()).

/// `resource.setrlimit(RLIMIT_CORE, (0, 0))` and `prctl(PR_SET_DUMPABLE, 0)`:
/// no core files, and same-UID processes cannot ptrace the greeter or read its
/// memory while it handles credentials. Err only if prctl fails (Python then
/// exits with `Unable to protect the authentication process`).
pub fn protect_process() -> Result<(), &'static str> {
    let limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: plain syscalls with valid arguments.
    unsafe {
        if libc::setrlimit(libc::RLIMIT_CORE, &limit) != 0 {
            return Err("Unable to protect the authentication process");
        }
        if libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0 {
            return Err("Unable to protect the authentication process");
        }
    }
    Ok(())
}
