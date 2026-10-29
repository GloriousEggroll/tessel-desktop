//! Link order only. The compositor-blur bridge (formerly native/background_blur.c)
//! now lives in `suite-wayland::blur` (pure Rust on GTK's wl_display).
fn main() {
    // libgtk4-layer-shell must precede libwayland-client in the link (and so in
    // symbol lookup order): it interposes wl_* calls. The Python suite gets the
    // same order by loading it RTLD_GLOBAL before GTK.
    println!("cargo:rustc-link-lib=dylib=gtk4-layer-shell");
    println!("cargo:rustc-link-lib=wayland-client");
    println!("cargo:rustc-link-lib=m");
}
