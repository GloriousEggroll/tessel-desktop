//! `wayshell-toplevel`: drop-in for the C helper (see suite_wayland::compat).
fn main() {
    std::process::exit(suite_wayland::compat::toplevel_main());
}
