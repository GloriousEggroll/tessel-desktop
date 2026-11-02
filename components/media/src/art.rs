//! lock_media_art.py: bounded, asynchronous album artwork from the player's
//! own MPRIS metadata. Local files or HTTP(S); decoded, EXIF-oriented and
//! downsized to ≤256 px PNG off the main thread; LRU of 12; TTL 1 h / 60 s.

use crate::mpris::{unquote, Media, Url};
use gtk::gdk_pixbuf;
use gtk::gdk_pixbuf::prelude::*;
use std::cell::{Cell, RefCell};
use std::io::Read;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

pub const MAX_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_PIXELS: i64 = 16 * 1024 * 1024;
const CACHE_SIZE: usize = 12;

pub fn album_name(media: Option<&Media>) -> String {
    media.filter(|m| m.kind != "video").map(|m| m.album.clone()).unwrap_or_default()
}

fn read_local(url: &Url) -> Option<Vec<u8>> {
    let path = std::path::PathBuf::from(unquote(&url.path));
    if !path.is_absolute() {
        return None;
    }
    let mut file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&path).ok()?;
    let info = file.metadata().ok()?;
    let kind = info.file_type();
    if !kind.is_file() || kind.is_fifo() || info.len() > MAX_BYTES as u64 {
        return None;
    }
    let mut data = Vec::new();
    Read::by_ref(&mut file).take(MAX_BYTES as u64 + 1).read_to_end(&mut data).ok()?;
    Some(data)
}

/// Read ordinary local files or HTTP(S); decode and downsize. Blocking.
pub fn load_artwork(url: &str) -> Option<Vec<u8>> {
    let parts = Url::split(url);
    let data = if parts.scheme == "file" && (parts.netloc.is_empty() || parts.netloc == "localhost") {
        read_local(&parts)?
    } else if (parts.scheme == "https" || parts.scheme == "http") && !parts.hostname().is_empty() && !parts.has_userinfo() {
        crate::http::get(url, "Kasumi-Media/1.0", "image/*", Duration::from_secs(4), MAX_BYTES, true).ok()?
    } else {
        return None;
    };
    if data.len() > MAX_BYTES {
        return None;
    }
    thumbnail(&data)
}

/// `Image.thumbnail((256, 256))`, `exif_transpose`, RGBA PNG.
pub fn thumbnail(data: &[u8]) -> Option<Vec<u8>> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    let too_large = Rc::new(Cell::new(false));
    let flag = too_large.clone();
    loader.connect_size_prepared(move |loader, width, height| {
        if width as i64 * height as i64 > MAX_PIXELS {
            flag.set(true);
            loader.set_size(1, 1);
        }
    });
    let written = loader.write(data).is_ok();
    let closed = loader.close().is_ok();
    if !written || !closed || too_large.get() {
        return None;
    }
    let pixbuf = loader.pixbuf()?;
    let (width, height) = (pixbuf.width(), pixbuf.height());
    let pixbuf = if width > 256 || height > 256 {
        // PIL keeps the aspect ratio, rounding the shorter side.
        let scale = (256.0 / width as f64).min(256.0 / height as f64);
        let (w, h) = (((width as f64 * scale).round() as i32).max(1), ((height as f64 * scale).round() as i32).max(1));
        pixbuf.scale_simple(w, h, gdk_pixbuf::InterpType::Bilinear)?
    } else {
        pixbuf
    };
    let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
    let pixbuf = if pixbuf.has_alpha() { pixbuf } else { pixbuf.add_alpha(false, 0, 0, 0).ok()? };
    pixbuf.save_to_bufferv("png", &[]).ok()
}

pub type Loader = std::sync::Arc<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>;

struct Inner {
    changed: RefCell<Rc<dyn Fn()>>,
    loader: Loader,
    url: RefCell<String>,
    icon: RefCell<Option<gio::Icon>>,
    cache: RefCell<Vec<(String, Instant, Option<gio::Icon>)>>,
    pending: Cell<bool>,
    closed: Cell<bool>,
}

#[derive(Clone)]
pub struct AlbumArt(Rc<Inner>);

impl AlbumArt {
    pub fn new(changed: impl Fn() + 'static, loader: Option<Loader>) -> Self {
        Self(Rc::new(Inner {
            changed: RefCell::new(Rc::new(changed)), loader: loader.unwrap_or_else(|| std::sync::Arc::new(load_artwork)),
            url: RefCell::new(String::new()), icon: RefCell::new(None), cache: RefCell::new(Vec::new()),
            pending: Cell::new(false), closed: Cell::new(false),
        }))
    }

    pub fn resolve(&self, media: Option<&Media>) -> Option<gio::Icon> {
        // Radio stations and podcasts can provide artwork without an album name.
        let url = media.filter(|m| m.kind != "video").map(|m| m.art_url.clone()).unwrap_or_default();
        if self.0.closed.get() {
            return None;
        }
        if url != *self.0.url.borrow() {
            self.0.url.replace(url);
            self.0.icon.replace(None);
        }
        self.load();
        self.0.icon.borrow().clone()
    }

    fn load(&self) {
        let url = self.0.url.borrow().clone();
        if self.0.closed.get() || url.is_empty() {
            return;
        }
        let hit = {
            let mut cache = self.0.cache.borrow_mut();
            match cache.iter().position(|(u, expires, _)| *u == url && Instant::now() < *expires) {
                Some(index) => {
                    let entry = cache.remove(index);
                    let icon = entry.2.clone();
                    cache.push(entry);
                    Some(icon)
                }
                None => None,
            }
        };
        if let Some(icon) = hit {
            self.0.icon.replace(icon);
            return;
        }
        if self.0.pending.get() {
            return;
        }
        self.0.pending.set(true);
        let loader = self.0.loader.clone();
        let requested = url.clone();
        let handle = gio::spawn_blocking(move || loader(&requested));
        let weak: Weak<Inner> = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let data = handle.await.ok().flatten();
            if let Some(inner) = weak.upgrade() {
                AlbumArt(inner).ready(url, data);
            }
        });
    }

    fn ready(&self, url: String, data: Option<Vec<u8>>) {
        self.0.pending.set(false);
        if self.0.closed.get() {
            return;
        }
        let icon: Option<gio::Icon> = data.map(|d| gio::BytesIcon::new(&glib::Bytes::from_owned(d)).upcast());
        let ttl = Duration::from_secs(if icon.is_some() { 3600 } else { 60 });
        {
            let mut cache = self.0.cache.borrow_mut();
            cache.retain(|(u, _, _)| *u != url);
            cache.push((url.clone(), Instant::now() + ttl, icon));
            while cache.len() > CACHE_SIZE {
                cache.remove(0);
            }
        }
        self.load();
        // A late response for an old song can populate the cache but must not
        // show that cover on the new song, nor re-show media that has stopped.
        if url == *self.0.url.borrow() {
            let changed = self.0.changed.borrow().clone();
            changed();
        }
    }

    pub fn cached(&self, url: &str) -> bool { self.0.cache.borrow().iter().any(|(u, _, _)| u == url) }

    pub fn close(&self) {
        self.0.closed.set(true);
        self.0.changed.replace(Rc::new(|| {}));
        self.0.icon.replace(None);
        self.0.cache.borrow_mut().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: i32, height: i32) -> Vec<u8> {
        let pixbuf = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, width, height).unwrap();
        pixbuf.fill(0x56856cff);
        pixbuf.save_to_bufferv("png", &[]).unwrap()
    }

    #[test]
    fn artwork_is_bounded_downsized_rgba_png() {
        let dir = std::env::temp_dir().join(format!("media-art-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cover = dir.join("cover.png");
        std::fs::write(&cover, png(512, 256)).unwrap();
        let data = load_artwork(&format!("file://{}", cover.display())).unwrap();
        let loader = gdk_pixbuf::PixbufLoader::new();
        loader.write(&data).unwrap();
        loader.close().unwrap();
        let pixbuf = loader.pixbuf().unwrap();
        assert_eq!((pixbuf.width(), pixbuf.height(), pixbuf.has_alpha()), (256, 128, true));
        assert!(load_artwork("file:///missing-cover.png").is_none());
        assert!(load_artwork("file://remote.host/etc/passwd").is_none());
        assert!(load_artwork("https://user:pw@example.test/a.png").is_none());
        assert!(load_artwork("ftp://example.test/a.png").is_none());
        assert!(load_artwork("file:///dev/null").is_none());
        std::fs::write(&cover, b"not an image").unwrap();
        assert!(load_artwork(&format!("file://{}", cover.display())).is_none());
        std::fs::write(&cover, vec![0u8; MAX_BYTES + 1]).unwrap();
        assert!(load_artwork(&format!("file://{}", cover.display())).is_none());
        let _ = std::fs::remove_dir_all(dir);
        assert_eq!(album_name(Some(&Media { album: "A".into(), kind: "video".into(), ..Media::default() })), "");
        assert_eq!(album_name(Some(&Media { album: "A".into(), ..Media::default() })), "A");
    }
}
