//! service_readiness.py: tell systemd (`Type=notify`) the dock is ready only
//! once startup finished AND the StatusNotifier tray accepts registrations
//! (`ready` and `host_registered`). Sent once:
//! `READY=1\nSTATUS=Dock and system tray ready` to `$NOTIFY_SOCKET` (a path,
//! or `@name` = abstract socket); no socket (manual launch) is fine. A delivery
//! failure is returned as an error and readiness stays un-announced.
//!
//! API: `ServiceReadiness::new()`, `.update(initialized, Option<&dyn TrayReadiness>)
//! -> io::Result<bool>`, `.update_with(address, ..)` (explicit socket), `.announced()`.

use std::cell::Cell;
use std::io;
use std::os::unix::net::UnixDatagram;
use std::time::Duration;

pub const MESSAGE: &[u8] = b"READY=1\nSTATUS=Dock and system tray ready";

/// What readiness needs from the tray backend (`StatusTrayBackend` implements it).
pub trait TrayReadiness {
    fn ready(&self) -> bool;
    fn host_registered(&self) -> bool;
}

#[derive(Default)]
pub struct ServiceReadiness {
    announced: Cell<bool>,
}

fn send(address: &str) -> io::Result<()> {
    let channel = UnixDatagram::unbound()?;
    channel.set_write_timeout(Some(Duration::from_secs(1)))?;
    if let Some(name) = address.strip_prefix('@') {
        use std::os::linux::net::SocketAddrExt;
        let socket = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())?;
        channel.connect_addr(&socket)?;
    } else {
        channel.connect(address)?;
    }
    channel.send(MESSAGE)?;
    Ok(())
}

impl ServiceReadiness {
    pub fn new() -> Self { Self::default() }

    pub fn announced(&self) -> bool { self.announced.get() }

    /// Uses `$NOTIFY_SOCKET` (non-empty).
    pub fn update(&self, initialized: bool, tray: Option<&dyn TrayReadiness>) -> io::Result<bool> {
        let address = std::env::var("NOTIFY_SOCKET").ok().filter(|value| !value.is_empty());
        self.update_with(address.as_deref(), initialized, tray)
    }

    pub fn update_with(&self, address: Option<&str>, initialized: bool, tray: Option<&dyn TrayReadiness>) -> io::Result<bool> {
        if self.announced.get() {
            return Ok(true);
        }
        let Some(tray) = tray else { return Ok(false) };
        if !initialized || !tray.ready() || !tray.host_registered() {
            return Ok(false);
        }
        if let Some(address) = address {
            send(address)?;
        }
        self.announced.set(true);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[derive(Default)]
    struct Tray { ready: Cell<bool>, host: Cell<bool> }
    impl TrayReadiness for Tray {
        fn ready(&self) -> bool { self.ready.get() }
        fn host_registered(&self) -> bool { self.host.get() }
    }

    fn scratch() -> std::path::PathBuf {
        let base = std::env::var_os("TMPDIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("ready-{}-{}", std::process::id(), tessel_core::secrets::token_urlsafe(4)));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn notify_waits_for_startup_and_host_then_sends_once() {
        let dir = scratch();
        let path = dir.join("notify");
        let server = UnixDatagram::bind(&path).unwrap();
        server.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        let address = path.to_str().unwrap();
        let state = ServiceReadiness::new();
        let tray = Tray::default();
        assert!(!state.update_with(Some(address), false, None).unwrap());
        assert!(!state.update_with(Some(address), true, Some(&tray)).unwrap());
        tray.ready.set(true);
        assert!(!state.update_with(Some(address), true, Some(&tray)).unwrap());
        tray.host.set(true);
        assert!(!state.update_with(Some(address), false, Some(&tray)).unwrap());
        let mut buffer = [0u8; 256];
        assert!(server.recv(&mut buffer).is_err());
        assert!(state.update_with(Some(address), true, Some(&tray)).unwrap());
        let count = server.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..count], MESSAGE);
        assert!(state.update_with(Some(address), true, Some(&tray)).unwrap());
        assert!(server.recv(&mut buffer).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn manual_launch_needs_no_notification_socket() {
        let state = ServiceReadiness::new();
        let tray = Tray { ready: Cell::new(true), host: Cell::new(true) };
        assert!(state.update_with(None, true, Some(&tray)).unwrap());
        assert!(state.announced());
    }

    #[test]
    fn abstract_socket_and_delivery_failure() {
        use std::os::linux::net::SocketAddrExt;
        let name = format!("tessel-dock-test-ready-{}-{}", std::process::id(), tessel_core::secrets::token_urlsafe(4));
        let tray = Tray { ready: Cell::new(true), host: Cell::new(true) };
        {
            let socket = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
            let server = UnixDatagram::bind_addr(&socket).unwrap();
            server.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
            let state = ServiceReadiness::new();
            assert!(state.update_with(Some(&format!("@{name}")), true, Some(&tray)).unwrap());
            let mut buffer = [0u8; 256];
            let count = server.recv(&mut buffer).unwrap();
            assert!(buffer[..count].starts_with(b"READY=1"));
        }
        let state = ServiceReadiness::new();
        assert!(state.update_with(Some(&format!("@{name}")), true, Some(&tray)).is_err());
        assert!(!state.announced());
    }
}
