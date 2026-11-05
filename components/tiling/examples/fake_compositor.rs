//! A fake labwc tiling control socket for measurements
//! (tests/measure_headless.sh): answers every request with a fixed ready
//! reply at `$XDG_RUNTIME_DIR/waytile-$WAYLAND_DISPLAY.sock`. Refuses the
//! live runtime directory.

use std::os::unix::net::UnixDatagram;

fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
    assert!(runtime != "/run/user/1000" && !runtime.is_empty(), "refusing the live session");
    let display = std::env::var("WAYLAND_DISPLAY").expect("WAYLAND_DISPLAY");
    let path = std::path::Path::new(&runtime).join(format!("waytile-{display}.sock"));
    let _ = std::fs::remove_file(&path);
    let server = UnixDatagram::bind(&path).unwrap();
    if let Some(ready) = std::env::args().nth(1) {
        std::fs::write(ready, b"").unwrap();
    }
    let reply = format!("{{\"version\":1,\"ready\":true,\"compositor_pid\":{},\"enabled\":false,\"parking_supported\":true,\
        \"tile_bounds_supported\":true,\"outline_supported\":true,\"outline_enabled\":true,\"error\":\"\",\"windows\":[],\
        \"capture_output_supported\":true,\"capture_output\":null}}", std::process::id());
    let mut buffer = [0u8; 512];
    loop {
        let Ok((_, peer)) = server.recv_from(&mut buffer) else { continue };
        if let Some(peer) = peer.as_pathname() {
            let _ = server.send_to(reply.as_bytes(), peer);
        }
    }
}
