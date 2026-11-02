//! Build `libcapture-pipewire.so` (native/capture_pipewire.c) next to this crate's
//! build output when the PipeWire development files are installed. Without them the
//! build continues; tools/install.py then keeps an already installed copy.
use std::path::PathBuf;
use std::process::Command;

fn pkg_config(args: &[&str]) -> Option<Vec<String>> {
    let output = Command::new("pkg-config").args(args).output().ok()?;
    output.status.success()
        .then(|| String::from_utf8_lossy(&output.stdout).split_whitespace().map(str::to_owned).collect())
}

fn main() {
    println!("cargo:rerun-if-changed=native/capture_pipewire.c");
    let Some(flags) = pkg_config(&["--cflags", "--libs", "libpipewire-0.3"]) else {
        println!("cargo:warning=pipewire-devel is not installed; libcapture-pipewire.so is not built");
        return;
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let library = out.join("libcapture-pipewire.so");
    let compiler = cc::Build::new().get_compiler();
    let mut command = compiler.to_command();
    command.args(["-std=gnu11", "-Wall", "-Wextra", "-Werror", "-O2", "-fPIC", "-shared", "native/capture_pipewire.c"])
        .args(&flags).args(["-pthread", "-Wl,-z,relro,-z,now", "-o"]).arg(&library);
    let status = command.status().expect("a C compiler is required");
    assert!(status.success(), "building libcapture-pipewire.so failed");
}
