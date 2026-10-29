//! Drop-in replacements for the C helpers' stdio line protocols:
//! `waydock-toplevel` / `wayshell-toplevel` (toplevel_helper.c),
//! `waydock-workspaces` (workspace_helper.c) and `wayshell-overview`
//! (overview_capture.c). Output bytes, ids, ordering, messages, argument and
//! stdin rules and exit codes follow the C sources.

use crate::client::{connect, Client, Protocols};
use crate::model::{error_json, Render};
use std::io::Write;
use std::os::fd::AsRawFd;

fn print(line: &str) -> bool {
    let mut out = std::io::stdout().lock();
    out.write_all(line.as_bytes()).and_then(|()| out.write_all(b"\n")).and_then(|()| out.flush()).is_ok()
}

/// Print rendered model lines; false once a fatal error was seen (or stdout broke).
fn drain(client: &mut Client) -> bool {
    let mut running = true;
    for line in client.model.take_lines() {
        running &= print(&line);
    }
    for event in client.model.take_events() {
        if let crate::model::Event::Error { fatal: true, .. } = event {
            running = false;
        }
    }
    running
}

fn is_space(b: u8) -> bool { matches!(b, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r') }

/// `sscanf(line, "%19s %" SCNu64 " %c", action, &id, &extra) == 2`.
pub fn parse_window_request(line: &[u8]) -> Option<(String, u64)> {
    let line = &line[..line.iter().position(|b| *b == 0).unwrap_or(line.len())];
    let mut i = 0;
    while i < line.len() && is_space(line[i]) {
        i += 1;
    }
    let start = i;
    while i < line.len() && !is_space(line[i]) && i - start < 19 {
        i += 1;
    }
    if i == start {
        return None;
    }
    let action = String::from_utf8_lossy(&line[start..i]).into_owned();
    while i < line.len() && is_space(line[i]) {
        i += 1;
    }
    let negative = match line.get(i) {
        Some(b'-') => { i += 1; true }
        Some(b'+') => { i += 1; false }
        _ => false,
    };
    let digits = i;
    let mut value: u64 = 0;
    let mut overflow = false;
    while i < line.len() && line[i].is_ascii_digit() {
        match value.checked_mul(10).and_then(|v| v.checked_add((line[i] - b'0') as u64)) {
            Some(v) => value = v,
            None => overflow = true,
        }
        i += 1;
    }
    if i == digits {
        return None;
    }
    if overflow {
        value = u64::MAX;
    } else if negative {
        value = value.wrapping_neg();
    }
    while i < line.len() && is_space(line[i]) {
        i += 1;
    }
    (i == line.len()).then_some((action, value))
}

/// Strict `activate <id>` of workspace_helper.c; Err is the error message.
pub fn parse_workspace_request(line: &[u8]) -> Result<u64, &'static str> {
    let number = line.strip_prefix(b"activate ").ok_or("Invalid workspace request.")?;
    if !matches!(number.first(), Some(b'1'..=b'9')) || !number.iter().all(u8::is_ascii_digit) {
        return Err("Invalid workspace ID.");
    }
    std::str::from_utf8(number).ok().and_then(|n| n.parse::<u64>().ok()).ok_or("Invalid workspace ID.")
}

struct LineReader {
    buffer: Vec<u8>,
    limit: usize,
    reject_nul: bool,
}

enum Read {
    Lines(Vec<Vec<u8>>),
    /// Lines before the failure still run; then the helper stops.
    Stop(Vec<Vec<u8>>, Option<&'static str>),
}

impl LineReader {
    fn read(&mut self, too_long: &'static str) -> Read {
        let mut incoming = [0u8; 512];
        let count = unsafe { libc::read(libc::STDIN_FILENO, incoming.as_mut_ptr().cast(), incoming.len()) };
        if count == 0 {
            return Read::Stop(Vec::new(), None);
        }
        if count < 0 {
            let errno = std::io::Error::last_os_error().raw_os_error();
            return if matches!(errno, Some(libc::EINTR) | Some(libc::EAGAIN)) { Read::Lines(Vec::new()) } else { Read::Stop(Vec::new(), None) };
        }
        let mut lines = Vec::new();
        for &byte in &incoming[..count as usize] {
            if byte == 0 && self.reject_nul {
                return Read::Stop(lines, Some("Invalid workspace request."));
            }
            if byte == b'\n' {
                lines.push(std::mem::take(&mut self.buffer));
            } else if self.buffer.len() + 1 < self.limit {
                self.buffer.push(byte);
            } else {
                return Read::Stop(lines, Some(too_long));
            }
        }
        Read::Lines(lines)
    }
}

/// Snapshot (`--snapshot`) or event loop shared by both stdio helpers.
fn run(mut client: Client, mut reader: LineReader, too_long: &'static str, mut command: impl FnMut(&mut Client, &[u8])) -> i32 {
    loop {
        // wl_display_prepare_read loop: dispatch whatever is queued first.
        if client.dispatch_pending().is_err() || !drain(&mut client) {
            break;
        }
        let flushed = client.connection().flush();
        let blocked = match &flushed {
            Err(wayland_client::backend::WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => true,
            Err(_) => break,
            Ok(()) => false,
        };
        let Some(guard) = client.connection().prepare_read() else { continue };
        let fd = guard.connection_fd().as_raw_fd();
        let mut fds = [libc::pollfd { fd, events: libc::POLLIN | if blocked { libc::POLLOUT } else { 0 }, revents: 0 },
                       libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 }];
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
            drop(guard);
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            break;
        }
        if fds[0].revents & libc::POLLIN != 0 {
            if guard.read().is_err() {
                break;
            }
        } else {
            drop(guard);
        }
        if client.dispatch_pending().is_err() {
            break;
        }
        // A fatal event ends the loop only after this iteration's stdin (C order).
        let running = drain(&mut client);
        if fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            break;
        }
        if fds[1].revents & libc::POLLIN != 0 {
            let (lines, stop) = match reader.read(too_long) {
                Read::Lines(lines) => (lines, None),
                Read::Stop(lines, message) => (lines, Some(message)),
            };
            for line in lines {
                command(&mut client, &line);
            }
            let _ = client.flush();
            if let Some(message) = stop {
                if let Some(message) = message {
                    print(&error_json(message));
                }
                break;
            }
        } else if fds[1].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            break;
        }
        if !running {
            break;
        }
    }
    0
}

fn snapshot_flag() -> Result<bool, i32> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [] => Ok(false),
        [flag] if flag == "--snapshot" => Ok(true),
        _ => Err(2),
    }
}

/// Three startup roundtrips, printing as events arrive. False if one failed
/// or a fatal error was reported (the roundtrips still continue after that).
fn startup(client: &mut Client) -> bool {
    let mut running = true;
    for _ in 0..3 {
        let ok = client.roundtrip().is_ok();
        running &= drain(client);
        if !ok {
            return false;
        }
    }
    running
}

/// `waydock-toplevel [--snapshot]` (also installed as `wayshell-toplevel`).
pub fn toplevel_main() -> i32 {
    let snapshot = match snapshot_flag() {
        Ok(s) => s,
        Err(code) => return code,
    };
    let Ok(conn) = connect() else {
        print(&error_json("Cannot connect to the Wayland compositor."));
        return 2;
    };
    let mut client = Client::new(conn, Protocols::TOPLEVELS);
    client.model.render = Some(Render::Toplevel);
    let mut running = startup(&mut client);
    if !client.has_toplevels() {
        print(&error_json("This compositor does not support foreign-toplevel window management."));
        running = false;
    }
    if running {
        print(&format!("{{\"event\":\"ready\",\"seat\":{},\"protocol_version\":{}}}",
                       if client.has_seat() { "true" } else { "false" }, client.toplevel_version()));
    }
    let status = if running { 0 } else { 2 };
    if running && !snapshot {
        run(client, LineReader { buffer: Vec::new(), limit: 4096, reject_nul: false }, "Window request is too long.",
            |client, line| {
                let Some((action, id)) = parse_window_request(line) else {
                    print(&error_json("Invalid window request."));
                    return;
                };
                if client.model.window(id).is_none_or(|w| !w.published) {
                    print(&error_json("That window is no longer available."));
                    return;
                }
                let result = match action.as_str() {
                    "activate" => client.activate(id),
                    "minimize" => client.set_minimized(id, true),
                    "unminimize" => client.set_minimized(id, false),
                    "close" => client.close(id),
                    _ => {
                        print(&error_json("Unknown window request."));
                        return;
                    }
                };
                if let Err(error) = result {
                    print(&error_json(&error.to_string()));
                }
            });
    }
    status
}

/// `waydock-workspaces [--snapshot]`.
pub fn workspaces_main() -> i32 {
    let snapshot = match snapshot_flag() {
        Ok(s) => s,
        Err(code) => return code,
    };
    let Ok(conn) = connect() else {
        print(&error_json("Cannot connect to the Wayland compositor."));
        return 2;
    };
    let mut client = Client::new(conn, Protocols::WORKSPACES);
    client.model.workspaces_published = false;
    client.model.render = Some(Render::Workspaces);
    let mut running = startup(&mut client);
    let supported = client.has_workspaces();
    if !supported && running {
        print("{\"event\":\"unsupported\",\"message\":\"This compositor does not expose desktop workspaces.\"}");
        running = false;
    }
    let status = if supported && !running { 2 } else { 0 };
    if running {
        client.model.publish_workspaces();
        drain(&mut client);
        print("{\"event\":\"ready\",\"protocol_version\":1}");
    }
    if running && !snapshot {
        run(client, LineReader { buffer: Vec::new(), limit: 128, reject_nul: true }, "Workspace request is too long.",
            |client, line| {
                // IDs are generated locally; never send a request for an unknown/stale object.
                let id = match parse_workspace_request(line) {
                    Ok(id) => id,
                    Err(message) => return { print(&error_json(message)); },
                };
                if let Err(error) = client.activate_workspace(id) {
                    print(&error_json(&error.to_string()));
                }
            });
    }
    status
}

/// JSON string as overview_capture.c's `quoted()` prints it (same rules).
#[cfg(feature = "capture-png")]
fn quoted(out: &mut String, value: Option<&str>) { crate::model::json_string(out, value.unwrap_or("")) }

/// Standard base64 with padding (`g_base64_encode`).
pub fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Scaled thumbnail size: fit in `max` without upscaling, at least 1×1.
pub fn thumbnail_size(width: u32, height: u32, max: (f64, f64)) -> (i32, i32) {
    let ratio = 1f64.min((max.0 / width as f64).min(max.1 / height as f64));
    ((width as f64 * ratio).round().max(1.0) as i32, (height as f64 * ratio).round().max(1.0) as i32)
}

/// `wayshell-overview [connector]`.
#[cfg(feature = "capture-png")]
pub fn overview_main() -> i32 {
    use std::time::{Duration, Instant};
    let Ok(conn) = connect() else { return 1 };
    let mut client = Client::new(conn, Protocols::CAPTURE);
    client.keep_closed_listed = true;
    client.listed_limit = Some(128);
    if client.roundtrip().is_err() || client.roundtrip().is_err() || !client.can_capture_toplevels() {
        return 2;
    }
    let capture = |client: &mut Client, id: Option<crate::client::CaptureId>, window: Option<crate::model::HandleKey>| {
        let Some(id) = id else { return };
        let closed = |client: &Client| window.is_some_and(|key| client.model.listed_toplevel(key).is_none_or(|t| t.closed));
        let deadline = Instant::now() + Duration::from_millis(1200);
        let mut result = None;
        while result.is_none() && !closed(client) && Instant::now() < deadline {
            if client.wait(Some(Duration::from_millis(50))).is_err() {
                break;
            }
            for (done, frame) in client.take_captured() {
                if done == id {
                    result = Some(frame);
                }
            }
        }
        client.cancel_capture(id);
        let _ = client.flush();
        if let Some(Ok(frame)) = result.filter(|_| !closed(client)) {
            let listed = window.and_then(|key| client.model.listed_toplevel(key).cloned());
            emit_thumbnail(&frame, listed.as_ref());
        }
    };
    let connector = std::env::args().nth(1);
    if let Some(connector) = connector.filter(|_| client.can_capture_outputs()) {
        if client.model.output_by_name(&connector).is_some() {
            let id = client.capture_output(&connector, false).ok();
            capture(&mut client, id, None);
        }
    }
    // Emit the complete identity set before frames: duplicate titles must not
    // accidentally attach one window's image to a different close target.
    let mut line = String::from("{\"kind\":\"identities\",\"windows\":[");
    let mut first = true;
    for t in client.model.listed().iter().filter(|t| !t.closed) {
        line.push_str(if first { "{\"app_id\":" } else { ",{\"app_id\":" });
        quoted(&mut line, t.app_id.as_deref());
        line.push_str(",\"title\":");
        quoted(&mut line, t.title.as_deref());
        line.push('}');
        first = false;
    }
    line.push_str("]}");
    print(&line);
    let deadline = Instant::now() + Duration::from_secs(10);
    let initial: Vec<_> = client.model.listed().iter().map(|t| t.key).collect();
    for key in initial {
        if Instant::now() >= deadline {
            break;
        }
        if client.model.listed_toplevel(key).is_some_and(|t| !t.closed) {
            let id = client.capture_listed(key, false).ok();
            capture(&mut client, id, Some(key));
        }
    }
    0
}

/// Scale with cairo (bilinear) to PNG; window ≤ 640×400, desktop ≤ 1600×1000.
/// Non-normal transforms are skipped (fail closed to the labelled icon).
#[cfg(feature = "capture-png")]
pub fn thumbnail_png(frame: &crate::client::Frame, window: bool) -> Option<Vec<u8>> {
    if frame.transform != 0 || frame.width == 0 || frame.height == 0 {
        return None;
    }
    let (width, height) = thumbnail_size(frame.width, frame.height, if window { (640., 400.) } else { (1600., 1000.) });
    let format = if frame.has_alpha() { cairo::Format::ARgb32 } else { cairo::Format::Rgb24 };
    let source = cairo::ImageSurface::create_for_data(frame.data.clone(), format, frame.width as i32, frame.height as i32,
                                                      frame.stride as i32).ok()?;
    let small = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height).ok()?;
    {
        let cr = cairo::Context::new(&small).ok()?;
        cr.scale(width as f64 / frame.width as f64, height as f64 / frame.height as f64);
        cr.set_source_surface(&source, 0., 0.).ok()?;
        cr.source().set_filter(cairo::Filter::Bilinear);
        cr.paint().ok()?;
    }
    let mut png = Vec::new();
    small.write_to_png(&mut png).ok()?;
    Some(png)
}

#[cfg(feature = "capture-png")]
fn emit_thumbnail(frame: &crate::client::Frame, window: Option<&crate::model::ListedToplevel>) {
    let Some(png) = thumbnail_png(frame, window.is_some()) else { return };
    let mut line = format!("{{\"kind\":\"{}\",\"app_id\":", if window.is_some() { "window" } else { "desktop" });
    quoted(&mut line, window.and_then(|w| w.app_id.as_deref()));
    line.push_str(",\"title\":");
    quoted(&mut line, window.and_then(|w| w.title.as_deref()));
    line.push_str(",\"identifier\":");
    quoted(&mut line, window.and_then(|w| w.identifier.as_deref()));
    line.push_str(&format!(",\"image\":\"{}\"}}", base64(&png)));
    print(&line);
}
