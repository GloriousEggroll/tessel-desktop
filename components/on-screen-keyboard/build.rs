//! Build the native Wayland helper `waykeys-input` (native/input.c, unchanged
//! from btmanager/on-screen-keyboard) next to this crate's build output.
//! It is a separate executable: the GTK process never binds the
//! virtual-keyboard / input-method protocols itself.
use std::path::PathBuf;
use std::process::Command;

fn pkg_config(args: &[&str]) -> Vec<String> {
    let output = Command::new("pkg-config").args(args).output().expect("pkg-config is required");
    assert!(output.status.success(), "pkg-config {args:?} failed");
    String::from_utf8(output.stdout).unwrap().split_whitespace().map(str::to_owned).collect()
}

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let mut sources = vec!["native/input.c".to_string()];
    println!("cargo:rerun-if-changed=native/input.c");
    for name in ["input-method-unstable-v2", "virtual-keyboard-unstable-v1"] {
        let protocol = format!("native/protocols/{name}.xml");
        println!("cargo:rerun-if-changed={protocol}");
        for (mode, file) in [("client-header", format!("{name}.h")), ("private-code", format!("{name}.c"))] {
            let target = out.join(&file);
            let status = Command::new("wayland-scanner").args([mode, &protocol, target.to_str().unwrap()]).status()
                .expect("wayland-scanner is required");
            assert!(status.success());
        }
        sources.push(out.join(format!("{name}.c")).to_string_lossy().into_owned());
    }
    let helper = out.join("waykeys-input");
    let compiler = cc::Build::new().get_compiler();
    let mut command = compiler.to_command();
    // Same flags as the Python component's Makefile.
    command.args(["-O2", "-Wall", "-Wextra", "-Werror", "-I"]).arg(&out).args(&sources)
        .args(pkg_config(&["--cflags", "--libs", "wayland-client", "xkbcommon"])).arg("-o").arg(&helper);
    let status = command.status().expect("a C compiler is required");
    assert!(status.success(), "building waykeys-input failed");
    println!("cargo:rustc-env=WAYKEYS_INPUT_BUILT={}", helper.display());
}
