// pam-devel is not installed here: link the runtime soname directly (as tessel-locking does).
fn main() {
    println!("cargo:rustc-link-arg=-l:libpam.so.0");
    println!("cargo:rerun-if-changed=build.rs");
}
