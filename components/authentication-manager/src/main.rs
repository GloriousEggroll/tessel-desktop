fn main() {
    // Hardening (not in Python): no core dumps / same-user ptrace of the process
    // that briefly holds PAM responses. Exit status and contract are unchanged.
    unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
    std::process::exit(authentication_manager::app::main_with(Default::default()));
}
