//! `tessel-overview`: drop-in for the C helper (see tessel_wayland::compat).
fn main() {
    std::process::exit(tessel_wayland::compat::overview_main());
}
