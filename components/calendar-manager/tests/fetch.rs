//! Subscription fetching and source loading (FetchTests of
//! test_calendar_events.py plus the real gio HTTP client against a local
//! 127.0.0.1 server). Uses a private XDG_CACHE_HOME; no external network.

use calendar_manager::events::{fetch_url, friendly_error, load_source, url_cache_path, HttpOpener, LoadError, Opener, Source,
                               URL_REFRESH_SECONDS};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const BODY: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nSUMMARY:A\r\nDTSTART;VALUE=DATE:20260101\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

struct FnOpener<F: Fn(&str) -> Result<Vec<u8>, LoadError> + Send + Sync>(F);

impl<F: Fn(&str) -> Result<Vec<u8>, LoadError> + Send + Sync> Opener for FnOpener<F> {
    fn get(&self, url: &str, _limit: usize) -> Result<Vec<u8>, LoadError> {
        (self.0)(url)
    }
}

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64()
}

fn serve() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                headers.push(line.trim().to_owned());
            }
            let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
            log.lock().unwrap().push(format!("{path} {}", headers.iter().find(|h| h.starts_with("User-Agent")).cloned().unwrap_or_default()));
            let response = match path.as_str() {
                "/cal.ics" => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{BODY}", BODY.len()),
                "/chunked.ics" => {
                    let (a, b) = BODY.split_at(20);
                    format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{a}\r\n{:x};x=1\r\n{b}\r\n0\r\n\r\n", a.len(), b.len())
                }
                "/close.ics" => format!("HTTP/1.0 200 OK\r\n\r\n{BODY}"),
                "/page" => "HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\n<html>".to_owned(),
                "/missing" => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_owned(),
                "/moved" => "HTTP/1.1 302 Found\r\nLocation: /cal.ics\r\nContent-Length: 0\r\n\r\n".to_owned(),
                "/loop" => "HTTP/1.1 301 Moved\r\nLocation: /loop\r\nContent-Length: 0\r\n\r\n".to_owned(),
                _ => "HTTP/1.1 500 Oops\r\nContent-Length: 0\r\n\r\n".to_owned(),
            };
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (port, seen)
}

#[test]
fn fetching_caching_and_source_errors() {
    let cache = std::path::PathBuf::from(std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into()))
        .join(format!("calendar-fetch-{}", std::process::id()));
    std::fs::create_dir_all(&cache).unwrap();
    std::env::set_var("XDG_CACHE_HOME", &cache);
    std::env::set_var("no_proxy", "127.0.0.1,localhost");
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");

    // Python: test_cache_is_reused_while_fresh_and_used_when_offline.
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let opener = FnOpener(move |_url: &str| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(BODY.as_bytes().to_vec())
    });
    assert!(fetch_url("https://example.invalid/a.ics", &opener, None).unwrap().contains("SUMMARY:A"));
    assert!(fetch_url("https://example.invalid/a.ics", &opener, None).unwrap().contains("SUMMARY:A"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let path = url_cache_path("https://example.invalid/a.ics");
    assert!(path.starts_with(cache.join("desktop-suite/calendar")));
    assert_eq!(path.file_name().unwrap().len(), 32 + 4);
    let offline = FnOpener(|_url: &str| Err(LoadError::Connect("offline".into())));
    let later = now() + URL_REFRESH_SECONDS + 60.0;
    assert!(fetch_url("https://example.invalid/a.ics", &offline, Some(later)).unwrap().contains("SUMMARY:A"));
    assert_eq!(fetch_url("https://example.invalid/never.ics", &offline, None), Err(LoadError::Connect("offline".into())));
    // Python: test_non_calendar_responses_are_rejected.
    let page = FnOpener(|_url: &str| Ok(b"<html>".to_vec()));
    assert_eq!(fetch_url("https://example.invalid/page", &page, None),
               Err(LoadError::Value("The address did not return a calendar.".into())));
    // The cache name is sha256(url)[:32] like Python.
    let expected = glib::compute_checksum_for_data(glib::ChecksumType::Sha256, b"https://example.invalid/a.ics").unwrap();
    assert_eq!(path.file_stem().unwrap().to_str().unwrap(), &expected[..32]);

    // The real gio HTTP client against a local server.
    let (port, seen) = serve();
    let base = format!("http://127.0.0.1:{port}");
    for route in ["/cal.ics", "/chunked.ics", "/close.ics", "/moved"] {
        let body = HttpOpener.get(&format!("{base}{route}"), 1 << 20).unwrap();
        assert_eq!(String::from_utf8(body).unwrap(), BODY, "{route}");
    }
    assert!(seen.lock().unwrap().iter().all(|line| line.ends_with("User-Agent: desktop-suite-calendar/1")));
    assert_eq!(HttpOpener.get(&format!("{base}/missing"), 1 << 20), Err(LoadError::Http(404)));
    assert_eq!(HttpOpener.get(&format!("{base}/loop"), 1 << 20), Err(LoadError::Http(301)));
    assert_eq!(HttpOpener.get(&format!("{base}/cal.ics"), 10).unwrap().len(), 10);
    let refused = HttpOpener.get("http://127.0.0.1:9/x.ics", 100).unwrap_err();
    assert!(matches!(refused, LoadError::Connect(_)), "{refused:?}");
    assert_eq!(friendly_error(&LoadError::Http(404)), "Server error 404");
    assert_eq!(friendly_error(&refused), "Could not connect");

    // load_source for URLs and files.
    let url = |route: &str| Source { kind: "url".into(), location: format!("{base}{route}"), name: String::new() };
    let events = load_source(&url("/cal.ics"), &HttpOpener).unwrap();
    assert_eq!((events.len(), events[0].source.as_str()), (1, format!("{base}/cal.ics").as_str()));
    assert_eq!(load_source(&url("/page"), &HttpOpener).map(|_| ()), Err(LoadError::Value("The address did not return a calendar.".into())));
    assert_eq!(load_source(&url("/missing"), &HttpOpener).map(|_| ()), Err(LoadError::Http(404)));
    let file = |location: &std::path::Path, name: &str| Source { kind: "file".into(), location: location.to_string_lossy().into(), name: name.into() };
    let ics = cache.join("work.ics");
    std::fs::write(&ics, BODY).unwrap();
    assert_eq!(load_source(&file(&ics, "Work"), &HttpOpener).unwrap()[0].source, "Work");
    let errors = |source: &Source| friendly_error(&load_source(source, &HttpOpener).map(|_| ()).unwrap_err());
    assert_eq!(errors(&file(&cache.join("missing.ics"), "")), "File not found");
    assert_eq!(errors(&file(&cache, "")), "Could not connect", "a directory is an OSError");
    let locked = cache.join("locked.ics");
    std::fs::write(&locked, BODY).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0)).unwrap();
    if std::fs::read(&locked).is_err() {
        assert_eq!(errors(&file(&locked, "")), "Permission denied");
    }
    let big = cache.join("big.ics");
    std::fs::File::create(&big).unwrap().set_len(16 * 1024 * 1024 + 1).unwrap();
    assert_eq!(errors(&file(&big, "")), "The calendar is too large.");
    assert_eq!(friendly_error(&LoadError::Other), "Could not read the calendar");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::remove_dir_all(&cache).unwrap();
}
