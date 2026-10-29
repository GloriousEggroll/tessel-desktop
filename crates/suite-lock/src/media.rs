//! `lock_media.py`, `lock_media_icons.py`, `lock_media_art.py`.
//!
//! These are already ported (and tested on a private bus) in media-manager's
//! library, whose `MediaSession` is `media_session.MediaSession`, a superset
//! of `LockMedia`: without `choose()` (the pinned player) it behaves exactly
//! like `LockMedia` — event-driven, never launches players (NO_AUTO_START),
//! commands go to the current unique owner, `readonly` blocks all commands,
//! `start(pause_playing)` pauses already-playing players once and close()
//! never resumes anything.
pub use media_manager::art::{album_name, load_artwork, thumbnail, AlbumArt, Loader as ArtLoader, MAX_BYTES, MAX_PIXELS};
pub use media_manager::icons::{application_icon, desktop_icon, fallback as fallback_icon, matching_icon, MediaIcons};
pub use media_manager::mpris::{text, track_kind, Media, MediaSession as LockMedia, Py, PATH, PLAYER, PREFIX, ROOT};

#[cfg(test)]
mod tests {
    use super::*;

    fn in_context(f: impl FnOnce()) {
        let context = glib::MainContext::new();
        context.with_thread_default(f).unwrap();
    }
    use gio::prelude::*;
    use gtk::gdk_pixbuf::prelude::*;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    fn music() -> Media {
        Media { name: "fixture".into(), title: "Track".into(), artist: "Artist".into(), playing: true, controllable: true,
                album: "Album".into(), art_url: "file:///fixture.png".into(), ..Media::default() }
    }

    fn png(size: i32) -> Vec<u8> {
        let pixbuf = gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, size, size).unwrap();
        pixbuf.fill(0x638583ff);
        pixbuf.save_to_bufferv("png", &[]).unwrap()
    }

    fn spin(mut predicate: impl FnMut() -> bool) -> bool {
        suite_core::timer::wait_for(&glib::MainContext::ref_thread_default(), Duration::from_secs(2), &mut predicate)
    }

    fn dict(items: &[(&str, Py)]) -> Py { Py::Dict(items.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()) }

    #[test]
    fn types_and_album_visibility() {
        let s = |v: &str| Py::Str(v.into());
        for (metadata, url, expected) in [
            (dict(&[("xesam:contentType", Py::List(vec![s("http://xesam.org/Audio")]))]), "", "audio"),
            (dict(&[("xesam:mimeType", s("video/mp4"))]), "spotify:track:test", "video"),
            (dict(&[("xesam:contentType", Py::List(vec![s("http://xesam.org/Video")]))]), "", "video"),
            (dict(&[]), "file:///movie.MKV", "video"),
            (dict(&[]), "https://www.youtube.com/watch?v=test", "video"),
            (dict(&[]), "https://music.youtube.com/watch?v=test", ""),
            (dict(&[]), "https://open.spotify.com/track/test", "audio"),
            (dict(&[]), "file:///test.FLAC", "audio"),
            (dict(&[]), "https://[malformed", ""),
            (dict(&[]), "", ""),
        ] {
            assert_eq!(track_kind(&metadata, url), expected, "{url}");
        }
        assert_eq!(album_name(Some(&music())), "Album");
        assert_eq!(album_name(Some(&Media { kind: "video".into(), ..music() })), "");
        assert_eq!(album_name(None), "");
    }

    #[test]
    fn local_art_is_resized_and_invalid_sources_are_rejected() {
        let dir = std::env::temp_dir().join(format!("suite-lock-art-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("album cover.png");
        std::fs::write(&path, png(512)).unwrap();
        let uri = glib::filename_to_uri(&path, None).unwrap().to_string();
        let data = load_artwork(&uri).unwrap();
        let loader = gtk::gdk_pixbuf::PixbufLoader::new();
        loader.write(&data).unwrap();
        loader.close().unwrap();
        let art = loader.pixbuf().unwrap();
        assert_eq!((art.width(), art.height()), (256, 256));
        std::fs::write(&path, "not an image").unwrap();
        assert!(load_artwork(&uri).is_none());
        let fifo = dir.join("pipe");
        let name = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: plain mkfifo on a test path.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(load_artwork(&glib::filename_to_uri(&fifo, None).unwrap()).is_none());
        for url in ["file://remote/cover.png", "file:relative", "data:image/png;base64,123", "ftp://example.com/cover.png",
                    "https://user:pass@example.com/cover.png"] {
            assert!(load_artwork(url).is_none(), "{url}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn late_art_cannot_replace_new_track_or_reappear_after_stop() {
        in_context(|| {
            let gate = Arc::new((Mutex::new((false, false)), Condvar::new())); // (started, release)
            let calls = Arc::new(Mutex::new(Vec::<String>::new()));
            let changed = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
            let (g, c) = (gate.clone(), calls.clone());
            let first = music().art_url;
            let loader: ArtLoader = Arc::new(move |url: &str| {
                c.lock().unwrap().push(url.to_owned());
                if url == first {
                    let mut state = g.0.lock().unwrap();
                    state.0 = true;
                    g.1.notify_all();
                    let _ = g.1.wait_timeout_while(state, Duration::from_secs(2), |s| !s.1).unwrap();
                }
                Some(png(16))
            });
            let art_cell: std::rc::Rc<std::cell::RefCell<Option<AlbumArt>>> = Default::default();
            let (record, holder) = (changed.clone(), art_cell.clone());
            let second = Media { art_url: "file:///second.png".into(), ..music() };
            let second_url = second.art_url.clone();
            let art = AlbumArt::new(move || {
                let _ = &holder;
                record.borrow_mut().push(second_url.clone());
            }, Some(loader));
            art_cell.replace(Some(art.clone()));
            assert!(art.resolve(Some(&music())).is_none());
            {
                let state = gate.0.lock().unwrap();
                let (state, _) = gate.1.wait_timeout_while(state, Duration::from_secs(1), |s| !s.0).unwrap();
                assert!(state.0);
            }
            assert!(art.resolve(Some(&second)).is_none());
            gate.0.lock().unwrap().1 = true;
            gate.1.notify_all();
            assert!(spin(|| !changed.borrow().is_empty()));
            assert_eq!(*changed.borrow(), vec![second.art_url.clone()]);
            assert!(art.resolve(Some(&second)).is_some());
            assert_eq!(*calls.lock().unwrap(), vec![music().art_url, second.art_url.clone()]);
            assert!(art.resolve(Some(&Media { kind: "video".into(), ..second.clone() })).is_none());
            assert!(art.resolve(None).is_none());
            art.close();
            assert!(art.resolve(Some(&second)).is_none());
        });
    }

    #[test]
    fn failures_are_cached_video_never_requests_art_and_stations_without_album_get_art() {
        in_context(|| {
            let calls = Arc::new(Mutex::new(Vec::<String>::new()));
            let c = calls.clone();
            let art = AlbumArt::new(|| {}, Some(Arc::new(move |url: &str| { c.lock().unwrap().push(url.into()); None })));
            art.resolve(Some(&Media { kind: "video".into(), ..music() }));
            assert!(calls.lock().unwrap().is_empty());
            art.resolve(Some(&music()));
            assert!(spin(|| art.cached(&music().art_url)));
            for _ in 0..10 {
                assert!(art.resolve(Some(&music())).is_none());
            }
            assert_eq!(*calls.lock().unwrap(), vec![music().art_url]);
            art.close();
            let calls = Arc::new(Mutex::new(Vec::<String>::new()));
            let c = calls.clone();
            let art = AlbumArt::new(|| {}, Some(Arc::new(move |url: &str| { c.lock().unwrap().push(url.into()); Some(png(16)) })));
            let station = Media { album: String::new(), title: "Station".into(), kind: "audio".into(), ..music() };
            art.resolve(Some(&station));
            assert!(spin(|| art.cached(&station.art_url)));
            assert!(art.resolve(Some(&station)).is_some());
            assert_eq!(*calls.lock().unwrap(), vec![music().art_url]);
            art.close();
        });
    }

    #[test]
    fn icon_cache_ignores_track_changes_and_falls_back_to_player_icon() {
        let icons = MediaIcons::default();
        let media = Media { name: "org.mpris.MediaPlayer2.no-such-player-fixture".into(), ..music() };
        let first = icons.resolve(Some(&media));
        assert!(first.equal(Some(&fallback_icon())));
        let other_track = Media { title: "Another".into(), art_url: "file:///x.png".into(), ..media.clone() };
        assert!(icons.resolve(Some(&other_track)) == first);
        assert!(icons.resolve(None).equal(Some(&fallback_icon())));
        icons.close();
    }
}
