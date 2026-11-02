//! Minimal blocking HTTP/1.1 GET over gio sockets (TLS from glib-networking,
//! proxies from GProxyResolver), standing in for Python's `urllib.request.urlopen`:
//! redirects 301/302/303/307/308 (at most 10, a URL at most 4 times), non-2xx
//! → `Http(code)`, connection/TLS/timeout failures → `Connect`. Runs on a
//! worker thread only.

use crate::events::{LoadError, USER_AGENT};
use gio::prelude::*;
use std::io::{BufRead, BufReader, Read, Write};

const MAX_REDIRECTIONS: usize = 10;
const MAX_REPEATS: usize = 4;

fn connect_error(error: impl std::fmt::Display) -> LoadError {
    LoadError::Connect(error.to_string())
}

struct Response {
    status: u16,
    location: Option<String>,
    body: Vec<u8>,
}

fn request(url: &str, limit: usize, timeout: u32) -> Result<Response, LoadError> {
    let uri = glib::Uri::parse(url, glib::UriFlags::NONE).map_err(|e| LoadError::Value(e.message().to_owned()))?;
    let scheme = uri.scheme().to_ascii_lowercase();
    let https = match scheme.as_str() {
        "https" => true,
        "http" => false,
        other => return Err(LoadError::Connect(format!("unknown url type: {other}"))),
    };
    let host = uri.host().map(|h| h.to_string()).filter(|h| !h.is_empty()).ok_or_else(|| connect_error("no host given"))?;
    let port = uri.port();
    let default_port = if https { 443 } else { 80 };
    let mut selector = uri.path().to_string();
    if selector.is_empty() {
        selector = "/".into();
    }
    if let Some(query) = uri.query() {
        selector = format!("{selector}?{query}");
    }
    let host_header = if port > 0 && port != default_port { format!("{host}:{port}") } else { host.clone() };
    let client = gio::SocketClient::new();
    client.set_timeout(timeout);
    client.set_tls(https);
    let target = format!("{scheme}://{host_header}");
    let connection = client.connect_to_uri(&target, default_port as u16, gio::Cancellable::NONE).map_err(connect_error)?;
    let stream = connection.upcast_ref::<gio::IOStream>();
    let mut writer = stream.output_stream().into_write();
    let head = format!("GET {selector} HTTP/1.1\r\nAccept-Encoding: identity\r\nHost: {host_header}\r\n\
                        User-Agent: {USER_AGENT}\r\nConnection: close\r\n\r\n");
    writer.write_all(head.as_bytes()).map_err(connect_error)?;
    writer.flush().map_err(connect_error)?;
    let mut reader = BufReader::new(stream.input_stream().into_read());
    let mut status;
    let mut headers: Vec<(String, String)>;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).map_err(connect_error)?;
        let mut parts = line.trim_end().splitn(3, ' ');
        let version = parts.next().unwrap_or("");
        status = parts.next().and_then(|s| s.parse::<u16>().ok()).ok_or_else(|| connect_error("bad status line"))?;
        if !version.starts_with("HTTP/") {
            return Err(connect_error("bad status line"));
        }
        headers = Vec::new();
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).map_err(connect_error)? == 0 {
                break;
            }
            let header = header.trim_end_matches(['\r', '\n']);
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
            }
        }
        if status != 100 {
            break;
        }
    }
    let header = |name: &str| headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone());
    let location = header("location").or_else(|| header("uri"));
    let mut body = Vec::new();
    let chunked = header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    if (200..300).contains(&status) {
        if chunked {
            loop {
                let mut size_line = String::new();
                if reader.read_line(&mut size_line).map_err(connect_error)? == 0 {
                    break;
                }
                let size_text = size_line.trim().split(';').next().unwrap_or("");
                let size = usize::from_str_radix(size_text, 16).map_err(|_| connect_error("bad chunk"))?;
                if size == 0 {
                    break;
                }
                let want = size.min(limit.saturating_sub(body.len()));
                let mut chunk = vec![0u8; size];
                reader.read_exact(&mut chunk).map_err(connect_error)?;
                body.extend_from_slice(&chunk[..want]);
                let mut crlf = String::new();
                reader.read_line(&mut crlf).map_err(connect_error)?;
                if body.len() >= limit {
                    break;
                }
            }
        } else {
            let length = header("content-length").and_then(|v| v.parse::<usize>().ok());
            let want = length.map_or(limit, |l| l.min(limit));
            reader.take(want as u64).read_to_end(&mut body).map_err(connect_error)?;
        }
    }
    let _ = stream.close(gio::Cancellable::NONE);
    Ok(Response { status, location, body })
}

/// GET `url` returning at most `limit` bytes of the body.
pub fn get(url: &str, limit: usize, timeout: u32) -> Result<Vec<u8>, LoadError> {
    let mut current = url.split('#').next().unwrap_or(url).to_owned();
    let mut visited: Vec<String> = Vec::new();
    loop {
        let response = request(&current, limit, timeout)?;
        if (200..300).contains(&response.status) {
            return Ok(response.body);
        }
        if !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            return Err(LoadError::Http(response.status));
        }
        let Some(location) = response.location else { return Err(LoadError::Http(response.status)) };
        let next = glib::Uri::resolve_relative(Some(&current), &location, glib::UriFlags::NONE)
            .map(|u| u.to_string()).map_err(|_| LoadError::Http(response.status))?;
        let scheme = next.split(':').next().unwrap_or("").to_ascii_lowercase();
        if !matches!(scheme.as_str(), "http" | "https" | "ftp") {
            return Err(LoadError::Http(response.status));
        }
        visited.push(next.clone());
        if visited.iter().filter(|u| **u == next).count() > MAX_REPEATS || visited.len() > MAX_REDIRECTIONS {
            return Err(LoadError::Http(response.status));
        }
        current = next.split('#').next().unwrap_or(&next).to_owned();
    }
}
