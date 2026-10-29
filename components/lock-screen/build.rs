// waylock-pam links libpam by soname only (no pam-devel headers on this
// system; the FFI is hand-written in src/bin/waylock-pam.rs). Nothing else
// in this crate links libpam.
fn main() {
    println!("cargo:rustc-link-arg-bin=waylock-pam=-l:libpam.so.0");
    println!("cargo:rerun-if-changed=build.rs");
}
