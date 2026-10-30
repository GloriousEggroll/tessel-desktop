// libarchive ships no headers or pkg-config file here: link the runtime soname
// directly (the FFI in src/ffi.rs is written by hand, like the suite's libpam FFI).
fn main() {
    println!("cargo:rustc-link-arg=-l:libarchive.so.13");
    println!("cargo:rerun-if-changed=build.rs");
}
