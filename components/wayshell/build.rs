//! Link order only (see crates/suite-gtk/build.rs): libgtk4-layer-shell must
//! precede libwayland-client, which suite-wayland also links for its in-process
//! clients. Python gets this by loading layer shell RTLD_GLOBAL first.
fn main() {
    println!("cargo:rustc-link-lib=dylib=gtk4-layer-shell");
    println!("cargo:rustc-link-lib=wayland-client");
}
