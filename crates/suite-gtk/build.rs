//! Build the compositor-blur bridge (shared/background_blur.c) into this crate,
//! so binaries need no runtime libwaydock-background-blur.so lookup.
use std::path::PathBuf;
use std::process::Command;

fn pkg_config(args: &[&str]) -> Vec<String> {
    let output = Command::new("pkg-config").args(args).output().expect("pkg-config is required");
    assert!(output.status.success(), "pkg-config {args:?} failed");
    String::from_utf8(output.stdout).unwrap().split_whitespace().map(str::to_owned).collect()
}

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let mut build = cc::Build::new();
    for name in ["ext-background-effect-v1", "waydock-blur-v1"] {
        let protocol = format!("native/protocols/{name}.xml");
        println!("cargo:rerun-if-changed={protocol}");
        let header = out.join(format!("{name}-client.h"));
        let code = out.join(format!("{name}-protocol.c"));
        for (mode, target) in [("client-header", &header), ("private-code", &code)] {
            let status = Command::new("wayland-scanner").args([mode, &protocol, target.to_str().unwrap()]).status()
                .expect("wayland-scanner is required");
            assert!(status.success());
        }
        build.file(code);
    }
    println!("cargo:rerun-if-changed=native/background_blur.c");
    build.file("native/background_blur.c").include(&out).flag("-std=c11").warnings(false);
    for flag in pkg_config(&["--cflags", "gtk4", "wayland-client"]) {
        build.flag(&flag);
    }
    build.compile("waydock-background-blur");
    // libgtk4-layer-shell must precede libwayland-client in the link (and so in
    // symbol lookup order): it interposes wl_* calls. The Python suite gets the
    // same order by loading it RTLD_GLOBAL before GTK.
    println!("cargo:rustc-link-lib=dylib=gtk4-layer-shell");
    println!("cargo:rustc-link-lib=wayland-client");
    println!("cargo:rustc-link-lib=m");
}
