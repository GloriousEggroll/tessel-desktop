// tessel-lock-pam links libpam by soname only (no pam-devel headers on this
// system; the FFI is hand-written in src/bin/tessel-lock-pam.rs). Nothing else
// in this crate links libpam.
fn main() {
    println!("cargo:rustc-link-arg-bin=tessel-lock-pam=-l:libpam.so.0");
    println!("cargo:rerun-if-changed=build.rs");
}
