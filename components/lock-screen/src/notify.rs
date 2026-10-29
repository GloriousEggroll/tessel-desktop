//! sd_notify READY=1 (lock_screen.py `_locked`): SOCK_DGRAM to $NOTIFY_SOCKET,
//! a leading `@` selects the abstract namespace. Sent only after the
//! compositor confirmed the lock. Errors are ignored.
use std::os::unix::net::UnixDatagram;

pub fn ready() {
    let Some(address) = std::env::var_os("NOTIFY_SOCKET").filter(|a| !a.is_empty()) else { return };
    let Ok(socket) = UnixDatagram::unbound() else { return };
    let bytes = address.as_encoded_bytes();
    if let Some(name) = bytes.strip_prefix(b"@") {
        use std::os::linux::net::SocketAddrExt;
        if let Ok(addr) = std::os::unix::net::SocketAddr::from_abstract_name(name) {
            let _ = socket.send_to_addr(b"READY=1", &addr);
        }
    } else {
        let _ = socket.send_to(b"READY=1", std::path::Path::new(&address));
    }
}
