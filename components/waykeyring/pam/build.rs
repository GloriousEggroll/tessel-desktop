// pam-devel is not installed here: link the runtime soname directly (as suite-lock does).
fn main() {
    println!("cargo:rustc-link-arg=-l:libpam.so.0");
    println!("cargo:rerun-if-changed=build.rs");
}
