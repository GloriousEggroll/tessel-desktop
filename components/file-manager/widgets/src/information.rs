//! Port of files_information.py: cancellable file previews (images, PDFs,
//! text/code, audio/video) and selection details.
//!
//! Strings are the Python literals (files_information.py does not translate).

use crate::compat::{self, clip, format_modified, format_size, general, item_count, thousands, Item};
use crate::poppler;
use gtk::prelude::*;
use gtk::{gdk, gdk_pixbuf, pango};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Read;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

pub const TEXT_LIMIT: usize = 1024 * 1024;
pub const PDF_LIMIT: u64 = 256 * 1024 * 1024;

/// Renderer kinds; `None` means only details are shown.
pub const IMAGE: &str = "image";
pub const PDF: &str = "pdf";
pub const MEDIA: &str = "media";
pub const TEXT: &str = "text";

/// `preview_kind(item)`.
pub fn preview_kind(item: Option<&Item>) -> Option<&'static str> {
    let item = item?;
    if item.is_dir() {
        return None;
    }
    kind_for_type(&item.content_type())
}

/// The content-type rule behind [`preview_kind`].
pub fn kind_for_type(kind: &str) -> Option<&'static str> {
    if kind.starts_with("image/") {
        Some(IMAGE)
    } else if gio::content_type_is_a(kind, "application/pdf") {
        Some(PDF)
    } else if kind.starts_with("video/") || kind.starts_with("audio/") {
        Some(MEDIA)
    } else if gio::content_type_is_a(kind, "text/plain") {
        Some(TEXT)
    } else {
        None
    }
}

pub type Details = Vec<(String, String)>;

fn row(title: &str, value: impl Into<String>) -> (String, String) {
    (title.to_string(), value.into())
}

// ---------------------------------------------------------------- photo headers

/// Pillow's `Image.mode` for the common photo formats, read from the header.
pub fn pil_mode(header: &[u8]) -> Option<&'static str> {
    if header.starts_with(b"\x89PNG\r\n\x1a\n") && header.len() >= 26 {
        let (depth, color) = (header[24], header[25]);
        return Some(match (color, depth) {
            (0, 1) => "1",
            (0, 16) => "I;16",
            (0, _) => "L",
            (2, _) => "RGB",
            (3, _) => "P",
            (4, _) => "LA",
            (6, _) => "RGBA",
            _ => return None,
        });
    }
    if header.starts_with(b"\xff\xd8") {
        let mut index = 2;
        while index + 9 < header.len() {
            if header[index] != 0xff {
                index += 1;
                continue;
            }
            let marker = header[index + 1];
            if marker == 0xff {
                index += 1;
                continue;
            }
            let length = u16::from_be_bytes([header[index + 2], header[index + 3]]) as usize;
            if (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker) {
                return Some(match header[index + 9] {
                    1 => "L",
                    3 => "RGB",
                    4 => "CMYK",
                    _ => return None,
                });
            }
            if marker == 0xd9 || marker == 0xda {
                return None;
            }
            index += 2 + length;
        }
        return None;
    }
    if header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a") {
        return Some("P");
    }
    if header.len() >= 30 && header.starts_with(b"RIFF") && &header[8..12] == b"WEBP" {
        return Some(match &header[12..16] {
            b"VP8X" => if header[20] & 0x10 != 0 { "RGBA" } else { "RGB" },
            b"VP8L" => if header.len() > 24 && header[24] & 0x10 != 0 { "RGBA" } else { "RGB" },
            _ => "RGB",
        });
    }
    if header.starts_with(b"BM") && header.len() >= 30 {
        let bits = u16::from_le_bytes([header[28], header[29]]);
        return Some(if bits <= 8 { "P" } else { "RGB" });
    }
    None
}

fn exif_text(value: &exif::Value) -> Option<String> {
    use exif::Value;
    let latin1 = |bytes: &[u8]| bytes.iter().map(|b| *b as char).collect::<String>();
    let ratio = |n: f64, d: f64| if d == 0.0 { "nan".to_string() } else { suite_core::pyfmt::float_repr(n / d) };
    let tuple = |items: Vec<String>| if items.len() == 1 { items.into_iter().next().unwrap() } else { format!("({})", items.join(", ")) };
    Some(match value {
        Value::Ascii(parts) => parts.first().map(|p| latin1(p)).unwrap_or_default(),
        Value::Byte(v) => tuple(v.iter().map(|x| x.to_string()).collect()),
        Value::Short(v) => tuple(v.iter().map(|x| x.to_string()).collect()),
        Value::Long(v) => tuple(v.iter().map(|x| x.to_string()).collect()),
        Value::SShort(v) => tuple(v.iter().map(|x| x.to_string()).collect()),
        Value::SLong(v) => tuple(v.iter().map(|x| x.to_string()).collect()),
        Value::Rational(v) => tuple(v.iter().map(|r| ratio(r.num as f64, r.denom as f64)).collect()),
        Value::SRational(v) => tuple(v.iter().map(|r| ratio(r.num as f64, r.denom as f64)).collect()),
        Value::Float(v) => tuple(v.iter().map(|x| suite_core::pyfmt::float_repr(*x as f64)).collect()),
        Value::Double(v) => tuple(v.iter().map(|x| suite_core::pyfmt::float_repr(*x)).collect()),
        _ => return None,
    })
}

/// A single numeric value (`float(tags[tag])`); `Err` for tuples (Python TypeError).
fn exif_number(value: &exif::Value) -> Result<Option<f64>, ()> {
    use exif::Value;
    let single = |count: usize| if count == 1 { Ok(()) } else { Err(()) };
    Ok(Some(match value {
        Value::Rational(v) => { single(v.len())?; if v[0].denom == 0 { return Ok(None) } v[0].num as f64 / v[0].denom as f64 }
        Value::SRational(v) => { single(v.len())?; if v[0].denom == 0 { return Ok(None) } v[0].num as f64 / v[0].denom as f64 }
        Value::Short(v) => { single(v.len())?; v[0] as f64 }
        Value::Long(v) => { single(v.len())?; v[0] as f64 }
        Value::Float(v) => { single(v.len())?; v[0] as f64 }
        Value::Double(v) => { single(v.len())?; v[0] }
        Value::Ascii(parts) => {
            let text = parts.first().map(|p| String::from_utf8_lossy(p).trim().to_string()).unwrap_or_default();
            text.parse::<f64>().map_err(|_| ())?
        }
        _ => return Err(()),
    }))
}

/// `photo_details(path)`: optional photo headers without decoding a second image.
pub fn photo_details(path: Option<&std::path::Path>) -> Details {
    let Some(path) = path else { return Vec::new() };
    let Ok(mut file) = std::fs::File::open(path) else { return Vec::new() };
    let mut header = vec![0u8; 65536];
    let Ok(read) = file.read(&mut header) else { return Vec::new() };
    header.truncate(read);
    let Some(mode) = pil_mode(&header) else { return Vec::new() };
    let mut details = vec![row("Color mode", mode)];
    let fields: Vec<exif::Field> = std::fs::File::open(path).ok().and_then(|f| {
        let mut reader = exif::Reader::new();
        reader.continue_on_error(true);
        match reader.read_from_container(&mut std::io::BufReader::new(f)) {
            Ok(exif) => Some(exif),
            Err(exif::Error::PartialResult(partial)) => Some(partial.into_inner().0),
            Err(_) => None,
        }
    }).map(|e| e.fields().filter(|f| f.ifd_num == exif::In::PRIMARY).cloned().collect()).unwrap_or_default();
    let ifd0 = |number: u16| fields.iter().find(|f| f.tag.number() == number && f.tag.context() == exif::Context::Tiff);
    // `tags = dict(exif); tags.update(exif.get_ifd(34665))`: the Exif IFD wins.
    let tag = |number: u16| fields.iter().find(|f| f.tag.number() == number && f.tag.context() == exif::Context::Exif)
        .or_else(|| ifd0(number));
    let mut seen: Vec<String> = Vec::new();
    for number in [271u16, 272] {
        let value = ifd0(number).and_then(|f| exif_text(&f.value)).unwrap_or_default().trim().to_string();
        if !seen.contains(&value) {
            seen.push(value);
        }
    }
    let camera = seen.join(" ").trim().to_string();
    if !camera.is_empty() {
        details.push(row("Camera", camera));
    }
    for (number, title) in [(36867u16, "Date taken"), (42036, "Lens"), (34855, "ISO")] {
        if let Some(text) = tag(number).and_then(|f| exif_text(&f.value)) {
            if !text.is_empty() && text != "0" {
                details.push(row(title, text));
            }
        }
    }
    type Format = fn(f64) -> String;
    let formats: [(u16, &str, Format); 3] = [
        (33434, "Exposure", |n| if n < 1.0 { format!("1/{} s", (1.0 / n).round_ties_even()) } else { format!("{} s", general(n)) }),
        (33437, "Aperture", |n| format!("f/{}", general(n))),
        (37386, "Focal length", |n| format!("{} mm", general(n))),
    ];
    for (number, title, format) in formats {
        let Some(field) = tag(number) else { continue };
        match exif_number(&field.value) {
            Ok(Some(n)) if n > 0.0 => details.push(row(title, format(n))),
            Ok(_) => {}
            Err(()) => return Vec::new(),
        }
    }
    details.into_iter().map(|(title, value)| (title, clip(&value, 512))).collect()
}

// ---------------------------------------------------------------- decoders

/// A decoded preview, built off the main thread.
#[derive(Clone, Debug)]
pub enum Payload {
    /// Image or rendered PDF page.
    Texture(gdk::Texture),
    Text(String),
}

/// `(payload, details, error)` as the Python decoders return it.
pub type Decoded = (Option<Payload>, Details, String);

fn unavailable() -> Decoded {
    (None, Vec::new(), "Preview unavailable".into())
}

fn failed(message: &str) -> Decoded {
    (None, Vec::new(), message.into())
}

/// `decode_preview(file, cancellable)`: stream an image at ≤ 1200 px, keep orientation.
pub fn decode_preview(file: &gio::File, cancellable: &gio::Cancellable) -> Decoded {
    let loader = gdk_pixbuf::PixbufLoader::new();
    let dimensions: Rc<RefCell<Vec<i32>>> = Rc::default();
    let sink = dimensions.clone();
    loader.connect_size_prepared(move |decoder, width, height| {
        *sink.borrow_mut() = vec![width, height];
        let scale = (1200.0 / width.max(height) as f64).min(1.0);
        decoder.set_size(((width as f64 * scale).round_ties_even() as i32).max(1),
                         ((height as f64 * scale).round_ties_even() as i32).max(1));
    });
    enum Failure { Glib, Value(&'static str) }
    let mut closed = false;
    let mut stream: Option<gio::FileInputStream> = None;
    let result = (|| -> Result<Decoded, Failure> {
        let input = file.read(Some(cancellable)).map_err(|_| Failure::Glib)?;
        stream = Some(input.clone());
        let mut total = 0usize;
        loop {
            let chunk = input.read_bytes(65536, Some(cancellable)).map_err(|_| Failure::Glib)?;
            if chunk.is_empty() {
                break;
            }
            total += chunk.len();
            if total > 128 * 1024 * 1024 {
                return Err(Failure::Value("This image is too large to preview."));
            }
            loader.write_bytes(&chunk).map_err(|_| Failure::Glib)?;
        }
        closed = true;
        loader.close().map_err(|_| Failure::Glib)?;
        let pixbuf = loader.pixbuf();
        let mut size = dimensions.borrow().clone();
        let Some(mut pixbuf) = pixbuf.filter(|_| !size.is_empty()) else {
            return Err(Failure::Value("No image preview is available."));
        };
        if matches!(pixbuf.option("orientation").as_deref(), Some("5" | "6" | "7" | "8")) {
            size.reverse();
        }
        if let Some(rotated) = pixbuf.apply_embedded_orientation() {
            pixbuf = rotated;
        }
        let format = loader.format().and_then(|f| f.name()).map(|n| n.to_uppercase()).unwrap_or_else(|| "Image".into());
        let mut details = vec![row("Dimensions", format!("{} × {} pixels", thousands(size[0]), thousands(size[1]))),
                               row("Format", format)];
        if !cancellable.is_cancelled() {
            details.extend(photo_details(file.path().as_deref()));
        }
        #[allow(deprecated)]
        let texture = gdk::Texture::for_pixbuf(&pixbuf);
        Ok((Some(Payload::Texture(texture)), details, String::new()))
    })();
    if let Some(stream) = stream {
        let _ = stream.close(gio::Cancellable::NONE);
    }
    if !closed {
        let _ = loader.close();
    }
    match result {
        Ok(value) => value,
        Err(Failure::Glib) => unavailable(),
        Err(Failure::Value(message)) => failed(message),
    }
}

/// Error of [`read_text`]: a GLib error or a Python `ValueError` message.
#[derive(Debug)]
pub enum ReadError {
    Glib(glib::Error),
    Value(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Glib(error) => f.write_str(error.message()),
            ReadError::Value(text) => f.write_str(text),
        }
    }
}

/// `read_text(file, cancellable, limit)`: at most `limit` bytes; `(text, truncated)`.
pub fn read_text(file: &gio::File, cancellable: Option<&gio::Cancellable>, limit: usize) -> Result<(String, bool), ReadError> {
    let stream = file.read(cancellable).map_err(ReadError::Glib)?;
    let mut data: Vec<u8> = Vec::new();
    let result = (|| {
        while data.len() <= limit {
            let chunk = stream.read_bytes(65536.min(limit + 1 - data.len()), cancellable).map_err(ReadError::Glib)?;
            if chunk.is_empty() {
                break;
            }
            data.extend_from_slice(&chunk);
        }
        Ok(())
    })();
    let _ = stream.close(gio::Cancellable::NONE);
    result?;
    if data[..data.len().min(8192)].contains(&0) {
        return Err(ReadError::Value("This file contains binary data.".into()));
    }
    let truncated = data.len() > limit;
    if truncated {
        data.truncate(limit);
        // Never end on a partial line or a split multi-byte character.
        if let Some(end) = data.iter().rposition(|b| *b == b'\n') {
            data.truncate(end + 1);
        }
    }
    Ok((String::from_utf8_lossy(&data).into_owned(), truncated))
}

/// `decode_text(file, cancellable)`.
pub fn decode_text(file: &gio::File, cancellable: &gio::Cancellable) -> Decoded {
    let (text, truncated) = match read_text(file, Some(cancellable), TEXT_LIMIT) {
        Ok(value) => value,
        Err(ReadError::Glib(_)) => return unavailable(),
        Err(ReadError::Value(message)) => return (None, Vec::new(), message),
    };
    let lines = text.matches('\n').count() + usize::from(!text.is_empty() && !text.ends_with('\n'));
    let details = vec![row("Lines", thousands(lines as i64) + if truncated { "+ (first 1 MiB shown)" } else { "" })];
    (Some(Payload::Text(text)), details, String::new())
}

/// `render_pdf_page(page, longest)`: the page on white, `longest` pixels on its long side.
pub fn render_pdf_page(page: &poppler::Page, longest: f64) -> Option<gdk::Texture> {
    use gtk::cairo;
    let (width, height) = page.size();
    let scale = longest / width.max(height).max(1.0);
    let pixel_width = ((width * scale).round_ties_even() as i32).max(1);
    let pixel_height = ((height * scale).round_ties_even() as i32).max(1);
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, pixel_width, pixel_height).ok()?;
    {
        let context = cairo::Context::new(&surface).ok()?;
        context.set_source_rgb(1.0, 1.0, 1.0);
        context.paint().ok()?;
        context.scale(scale, scale);
        page.render(&context);
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let data = glib::Bytes::from(&*surface.data().ok()?);
    Some(gdk::MemoryTexture::new(pixel_width, pixel_height, gdk::MemoryFormat::B8g8r8a8Premultiplied, &data, stride).upcast())
}

/// `decode_pdf(file, cancellable)`: render page 1 in the worker; the document is not retained.
pub fn decode_pdf(file: &gio::File, cancellable: &gio::Cancellable) -> Decoded {
    let size = match file.query_info("standard::size", gio::FileQueryInfoFlags::NONE, Some(cancellable)) {
        Ok(info) => info.size(),
        Err(_) => return unavailable(),
    };
    if size as u64 > PDF_LIMIT {
        return failed("This document is too large to preview.");
    }
    let Ok(document) = poppler::Document::from_gfile(file, Some(cancellable)) else { return unavailable() };
    if document.n_pages() < 1 {
        return failed("This document has no pages.");
    }
    let Some(page) = document.page(0) else { return unavailable() };
    let (width, height) = page.size();
    let Some(texture) = render_pdf_page(&page, 1200.0) else { return unavailable() };
    let mut details = vec![row("Pages", thousands(document.n_pages())),
                           row("Page size", format!("{} × {} mm", compat::fixed0(width * 25.4 / 72.0), compat::fixed0(height * 25.4 / 72.0)))];
    for (title, value) in [("Title", document.title()), ("Author", document.author())] {
        if let Some(value) = value.filter(|v| !v.trim().is_empty()) {
            details.push(row(title, clip(&value, 512)));
        }
    }
    (Some(Payload::Texture(texture)), details, String::new())
}

/// A decoder as the loader runs it (on its worker thread).
pub type Decoder = Arc<dyn Fn(&gio::File, &gio::Cancellable) -> Decoded + Send + Sync>;

fn overrides() -> &'static Mutex<HashMap<String, Decoder>> {
    static OVERRIDES: OnceLock<Mutex<HashMap<String, Decoder>>> = OnceLock::new();
    OVERRIDES.get_or_init(Default::default)
}

/// Replace (`Some`) or restore (`None`) the decoder for `kind` process-wide — the
/// Rust form of the Python tests' `patch.object(files_information, 'decode_preview', …)`.
pub fn set_decoder(kind: &str, decoder: Option<Decoder>) {
    let mut map = overrides().lock().unwrap();
    match decoder {
        Some(decoder) => { map.insert(kind.to_string(), decoder); }
        None => { map.remove(kind); }
    }
}

/// `decoder(kind)`, resolved at call time so tests may replace a decoder.
pub fn decoder(kind: &str) -> Decoder {
    if let Some(found) = overrides().lock().unwrap().get(kind) {
        return found.clone();
    }
    match kind {
        PDF => Arc::new(decode_pdf),
        TEXT => Arc::new(decode_text),
        _ => Arc::new(decode_preview),
    }
}

// ---------------------------------------------------------------- PreviewLoader

type Callback = Box<dyn FnOnce(Decoded)>;

#[derive(Default)]
struct LoaderState {
    pending: RefCell<Option<(gio::File, Callback, String)>>,
    running: Cell<bool>,
    cancellable: RefCell<Option<gio::Cancellable>>,
    closed: Cell<bool>,
}

/// One running decode and one replaceable request; never a selection backlog.
#[derive(Clone, Default)]
pub struct PreviewLoader(Rc<LoaderState>);

impl PreviewLoader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn running(&self) -> bool {
        self.0.running.get()
    }

    pub fn pending(&self) -> bool {
        self.0.pending.borrow().is_some()
    }

    pub fn closed(&self) -> bool {
        self.0.closed.get()
    }

    pub fn cancel(&self) {
        self.0.pending.replace(None);
        if let Some(token) = self.0.cancellable.borrow().as_ref() {
            token.cancel();
        }
    }

    /// Replace the pending request; `callback(payload, details, error)` runs on the
    /// main loop unless cancelled or superseded.
    pub fn request(&self, file: &gio::File, callback: impl FnOnce(Decoded) + 'static, kind: &str) {
        self.cancel();
        if !self.0.closed.get() {
            self.0.pending.replace(Some((file.clone(), Box::new(callback), kind.to_string())));
            self.start();
        }
    }

    fn start(&self) {
        if self.0.closed.get() || self.0.running.get() || self.0.pending.borrow().is_none() {
            return;
        }
        let (file, callback, kind) = self.0.pending.take().unwrap();
        self.0.running.set(true);
        let token = gio::Cancellable::new();
        self.0.cancellable.replace(Some(token.clone()));
        let decode = decoder(&kind);
        let worker_token = token.clone();
        let handle = gio::spawn_blocking(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode(&file, &worker_token))).unwrap_or_else(|_| unavailable())
        });
        let this = self.clone();
        glib::spawn_future_local(async move {
            let value = handle.await.unwrap_or_else(|_| unavailable());
            this.finish(&token, callback, value);
        });
    }

    fn finish(&self, token: &gio::Cancellable, callback: Callback, value: Decoded) {
        self.0.running.set(false);
        if !self.0.closed.get() && !token.is_cancelled() {
            callback(value);
        }
        self.start();
    }

    pub fn close(&self) {
        self.0.closed.set(true);
        self.cancel();
    }
}

/// `source_module()`: GtkSourceView 5 is linked, so always available.
pub fn source_module() -> bool {
    sourceview5::init();
    true
}

// ---------------------------------------------------------------- FilePreview

pub type OnLoaded = Rc<dyn Fn(&Details, &str)>;

pub struct PreviewWidgets {
    pub stack: gtk::Stack,
    pub picture: gtk::Picture,
    pub placeholder: gtk::Box,
    pub icon: gtk::Image,
    pub hint: gtk::Label,
    pub buffer: sourceview5::Buffer,
    pub text_view: sourceview5::View,
    pub video: gtk::Video,
}

pub struct PreviewInner {
    pub widgets: PreviewWidgets,
    pub large: bool,
    pub loader: PreviewLoader,
    on_loaded: RefCell<Option<OnLoaded>>,
    item: RefCell<Option<Item>>,
    kind: Cell<Option<&'static str>>,
    ready: Cell<bool>,
    media: RefCell<Option<gtk::MediaFile>>,
    generation: Cell<u64>,
}

/// A reusable preview surface (a `Gtk.Stack` with pages image/placeholder/text/media)
/// for the side pane and the quick preview window. Cheap to clone (shared handle).
#[derive(Clone)]
pub struct FilePreview(Rc<PreviewInner>);

impl std::ops::Deref for FilePreview {
    type Target = PreviewWidgets;
    fn deref(&self) -> &PreviewWidgets {
        &self.0.widgets
    }
}

impl FilePreview {
    pub const PRINTABLE: [&'static str; 3] = [IMAGE, PDF, TEXT];

    pub fn new(large: bool) -> Self {
        let stack = gtk::Stack::builder().hexpand(true).vexpand(large).build();
        stack.add_css_class("files-image-preview");
        let picture = gtk::Picture::builder().can_shrink(true).content_fit(gtk::ContentFit::Contain)
            .hexpand(true).vexpand(large).build();
        picture.set_alternative_text(Some("Selected file preview"));
        let placeholder = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12)
            .valign(gtk::Align::Center).halign(gtk::Align::Center).build();
        let icon = gtk::Image::builder().icon_name("image-x-generic-symbolic").pixel_size(if large { 96 } else { 64 }).build();
        let hint = gtk::Label::builder().label("Select a file to preview").wrap(true).justify(gtk::Justification::Center).build();
        hint.add_css_class("muted");
        placeholder.append(&icon);
        placeholder.append(&hint);
        source_module();
        let buffer = sourceview5::Buffer::new(None);
        buffer.set_highlight_syntax(true);
        let text_view = sourceview5::View::with_buffer(&buffer);
        text_view.set_show_line_numbers(large);
        text_view.set_editable(false);
        text_view.set_cursor_visible(false);
        text_view.set_monospace(true);
        text_view.set_wrap_mode(if large { gtk::WrapMode::None } else { gtk::WrapMode::WordChar });
        text_view.add_css_class("files-text-preview");
        let text_scroll = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).child(&text_view).build();
        let video = gtk::Video::builder().autoplay(false).hexpand(true).vexpand(true).build();
        stack.add_named(&picture, Some("image"));
        stack.add_named(&placeholder, Some("placeholder"));
        stack.add_named(&text_scroll, Some("text"));
        stack.add_named(&video, Some("media"));
        stack.set_visible_child_name("placeholder");
        Self(Rc::new(PreviewInner {
            widgets: PreviewWidgets { stack, picture, placeholder, icon, hint, buffer, text_view, video },
            large,
            loader: PreviewLoader::new(),
            on_loaded: RefCell::new(None),
            item: RefCell::new(None),
            kind: Cell::new(None),
            ready: Cell::new(false),
            media: RefCell::new(None),
            generation: Cell::new(0),
        }))
    }

    /// The `Gtk.Stack` to put in a layout.
    pub fn widget(&self) -> &gtk::Stack {
        &self.0.widgets.stack
    }

    pub fn loader(&self) -> &PreviewLoader {
        &self.0.loader
    }

    pub fn large(&self) -> bool {
        self.0.large
    }

    /// `on_loaded(details, error)` reports extra detail rows.
    pub fn set_on_loaded(&self, callback: Option<OnLoaded>) {
        self.0.on_loaded.replace(callback);
    }

    pub fn item(&self) -> Option<Item> {
        self.0.item.borrow().clone()
    }

    pub fn kind(&self) -> Option<&'static str> {
        self.0.kind.get()
    }

    pub fn ready(&self) -> bool {
        self.0.ready.get()
    }

    /// Always true: GtkSourceView 5 is linked (Python's `self.source`).
    pub fn source(&self) -> bool {
        true
    }

    /// The current `Gtk.MediaFile` (`self.media`).
    pub fn media(&self) -> Option<gtk::MediaFile> {
        self.0.media.borrow().clone()
    }

    pub fn printable(&self) -> bool {
        self.ready() && self.kind().is_some_and(|k| Self::PRINTABLE.contains(&k)) && self.0.item.borrow().is_some()
    }

    fn emit_loaded(&self, details: &Details, error: &str) {
        let callback = self.0.on_loaded.borrow().clone();
        if let Some(callback) = callback {
            callback(details, error);
        }
    }

    /// Preview `item` (or nothing); `hint` replaces "No preview available".
    pub fn show(&self, item: Option<Item>, hint: Option<&str>) {
        self.clear();
        let kind = preview_kind(item.as_ref());
        self.0.item.replace(item.clone());
        self.0.kind.set(kind);
        if let Some(item) = &item {
            self.icon.set_from_gicon(&item.icon());
            self.picture.set_alternative_text(Some(&item.display_name()));
        }
        let (Some(item), Some(kind)) = (item, kind) else {
            self.hint.set_text(hint.filter(|h| !h.is_empty()).unwrap_or("No preview available"));
            return;
        };
        if kind == MEDIA {
            self.show_media(&item);
            return;
        }
        self.hint.set_text("Loading preview…");
        let generation = self.0.generation.get();
        let weak = Rc::downgrade(&self.0);
        let shown = item.clone();
        self.0.loader.request(&item.file(), move |(payload, details, error)| {
            let Some(inner) = weak.upgrade() else { return };
            let this = FilePreview(inner);
            if this.0.generation.get() != generation {
                return;
            }
            let ready = payload.is_some();
            match payload {
                None => this.hint.set_text(if error.is_empty() { "Preview unavailable" } else { &error }),
                Some(Payload::Text(text)) => this.show_text(&shown, &text),
                Some(Payload::Texture(texture)) => {
                    this.picture.set_paintable(Some(&texture));
                    this.stack.set_visible_child_name("image");
                }
            }
            this.0.ready.set(ready);
            this.emit_loaded(&details, &error);
        }, kind);
    }

    fn show_text(&self, item: &Item, text: &str) {
        let languages = sourceview5::LanguageManager::default();
        let language = languages.guess_language(Some(item.name().as_str()), Some(item.content_type().as_str()));
        self.buffer.set_language(language.as_ref());
        let color = self.stack.color();
        let dark = (0.2126 * color.red() + 0.7152 * color.green() + 0.0722 * color.blue()) > 0.5;
        let schemes = sourceview5::StyleSchemeManager::default();
        self.buffer.set_style_scheme(schemes.scheme(if dark { "Adwaita-dark" } else { "Adwaita" }).as_ref());
        self.buffer.set_text(text);
        self.buffer.place_cursor(&self.buffer.start_iter());
        self.stack.set_visible_child_name("text");
    }

    fn show_media(&self, item: &Item) {
        let media = gtk::MediaFile::for_file(&item.file());
        self.0.media.replace(Some(media.clone()));
        self.video.set_media_stream(Some(&media));
        self.stack.set_visible_child_name("media");
        let weak = Rc::downgrade(&self.0);
        media.connect_prepared_notify(move |media| {
            let Some(inner) = weak.upgrade() else { return };
            let this = FilePreview(inner);
            if this.0.media.borrow().as_ref() != Some(media) || !media.is_prepared() {
                return;
            }
            this.0.ready.set(true);
            let mut details = Vec::new();
            if media.duration() > 0 {
                let seconds = (media.duration() as f64 / 1_000_000.0).round_ties_even() as i64;
                details.push(row("Duration", if seconds >= 3600 {
                    format!("{}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60)
                } else {
                    format!("{}:{:02}", seconds / 60, seconds % 60)
                }));
            }
            details.push(row("Contents", if media.has_video() { "Video" } else { "Audio" }));
            this.emit_loaded(&details, "");
        });
        let weak = Rc::downgrade(&self.0);
        media.connect_error_notify(move |media| {
            let Some(inner) = weak.upgrade() else { return };
            let this = FilePreview(inner);
            if this.0.media.borrow().as_ref() == Some(media) && media.error().is_some() {
                this.hint.set_text("Preview unavailable");
                this.stack.set_visible_child_name("placeholder");
            }
        });
    }

    pub fn clear(&self) {
        self.0.loader.cancel();
        self.0.generation.set(self.0.generation.get() + 1);
        self.0.item.replace(None);
        self.0.kind.set(None);
        self.0.ready.set(false);
        self.picture.set_paintable(None::<&gdk::Paintable>);
        self.buffer.set_text("");
        if let Some(media) = self.0.media.take() {
            self.video.set_media_stream(None::<&gtk::MediaStream>);
            media.clear();
        }
        self.stack.set_visible_child_name("placeholder");
    }

    pub fn close(&self) {
        self.clear();
        self.0.loader.close();
    }
}

// ---------------------------------------------------------------- InformationPane

type SelectionKey = (String, Vec<(String, u64, u64, u32, u32)>);

pub struct PaneWidgets {
    pub root: gtk::Box,
    pub print_button: gtk::Button,
    pub collapse: gtk::Button,
    pub preview: FilePreview,
    pub picture: gtk::Picture,
    pub icon: gtk::Image,
    pub hint: gtk::Label,
    pub title: gtk::Label,
    pub details: gtk::Grid,
    pub text: gtk::Label,
}

pub struct PaneInner {
    pub widgets: PaneWidgets,
    selection_key: RefCell<Option<SelectionKey>>,
    basic: RefCell<Details>,
    rows: RefCell<Details>,
}

/// The side preview pane (`.files-info`), a vertical `Gtk.Box` of width 300.
#[derive(Clone)]
pub struct InformationPane(Rc<PaneInner>);

impl std::ops::Deref for InformationPane {
    type Target = PaneWidgets;
    fn deref(&self) -> &PaneWidgets {
        &self.0.widgets
    }
}

impl InformationPane {
    /// `InformationPane(hide, print_file=None)`.
    pub fn new(hide: impl Fn() + 'static, print_file: Option<Rc<dyn Fn(Item)>>) -> Self {
        let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).width_request(300).build();
        root.add_css_class("files-info");
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.add_css_class("files-info-header");
        let title = gtk::Label::builder().label("Preview").xalign(0.0).hexpand(true).build();
        title.add_css_class("heading");
        header.append(&title);
        let print_button = gtk::Button::builder().child(&suite_gtk::utils::image("printer-symbolic", 16))
            .tooltip_text("Print…").visible(false).build();
        print_button.add_css_class("flat");
        header.append(&print_button);
        let collapse = gtk::Button::builder().child(&suite_gtk::utils::image("window-close-symbolic", 16))
            .tooltip_text("Hide preview pane — F11").build();
        collapse.add_css_class("flat");
        collapse.connect_clicked(move |_| hide());
        header.append(&collapse);
        root.append(&header);
        let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
        root.append(&scroll);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 16);
        body.add_css_class("files-info-content");
        scroll.set_child(Some(&body));
        let preview = FilePreview::new(false);
        preview.widget().set_size_request(-1, 300);
        body.append(preview.widget());
        let title = gtk::Label::builder().xalign(0.0).wrap(true).wrap_mode(pango::WrapMode::WordChar)
            .max_width_chars(24).selectable(true).build();
        title.add_css_class("title");
        body.append(&title);
        let details = gtk::Grid::builder().column_spacing(14).row_spacing(12).hexpand(true).build();
        body.append(&details);
        let text = gtk::Label::builder().xalign(0.0).wrap(true).wrap_mode(pango::WrapMode::WordChar)
            .selectable(true).max_width_chars(28).build();
        text.add_css_class("muted");
        body.append(&text);
        let widgets = PaneWidgets {
            picture: preview.picture.clone(), icon: preview.icon.clone(), hint: preview.hint.clone(),
            root, print_button, collapse, preview, title, details, text,
        };
        let pane = Self(Rc::new(PaneInner { widgets, selection_key: RefCell::new(None), basic: RefCell::default(), rows: RefCell::default() }));
        let weak = Rc::downgrade(&pane.0);
        pane.print_button.connect_clicked(move |_| {
            let Some(inner) = weak.upgrade() else { return };
            if let (Some(print), Some(item)) = (print_file.as_ref(), inner.widgets.preview.item()) {
                print(item);
            }
        });
        let weak = Rc::downgrade(&pane.0);
        pane.preview.set_on_loaded(Some(Rc::new(move |details: &Details, _error: &str| {
            if let Some(inner) = weak.upgrade() {
                InformationPane(inner).loaded(details);
            }
        })));
        pane
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.0.widgets.root
    }

    pub fn loader(&self) -> &PreviewLoader {
        self.preview.loader()
    }

    /// The detail rows currently shown (`self.rows`).
    pub fn rows(&self) -> Details {
        self.0.rows.borrow().clone()
    }

    pub fn row_value(&self, title: &str) -> Option<String> {
        self.0.rows.borrow().iter().rev().find(|(t, _)| t == title).map(|(_, v)| v.clone())
    }

    pub fn set_details(&self, rows: Details) {
        suite_gtk::utils::remove_children(&self.details);
        for (index, (title, value)) in rows.iter().enumerate() {
            let label = gtk::Label::builder().label(title).xalign(0.0).valign(gtk::Align::Start).build();
            label.add_css_class("muted");
            let field = gtk::Label::builder().label(value).xalign(0.0).valign(gtk::Align::Start).wrap(true)
                .wrap_mode(pango::WrapMode::WordChar).selectable(true).hexpand(true).max_width_chars(22).build();
            self.details.attach(&label, 0, index as i32, 1, 1);
            self.details.attach(&field, 1, index as i32, 1, 1);
        }
        self.0.rows.replace(rows);
    }

    fn loaded(&self, details: &Details) {
        if self.root.is_visible() {
            let mut rows = details.clone();
            rows.extend(self.0.basic.borrow().iter().cloned());
            self.set_details(rows);
            self.print_button.set_visible(self.preview.printable());
        }
    }

    /// Show the selection of `folder` (nothing, one item, or several).
    pub fn show_selection(&self, selected: &[Item], folder: &gio::File) {
        let key: SelectionKey = (folder.uri().to_string(), selected.iter().map(|item| {
            let info = item.info();
            (item.uri(), item.modified(), item.size(), info.attribute_uint32("time::modified-usec"),
             info.attribute_uint32("time::modified-nsec"))
        }).collect());
        if self.0.selection_key.borrow().as_ref() == Some(&key) {
            return;
        }
        self.clear();
        self.0.selection_key.replace(Some(key));
        if selected.len() != 1 {
            self.icon.set_icon_name(Some(if selected.is_empty() { "folder-symbolic" } else { "edit-select-all-symbolic" }));
            if selected.is_empty() {
                let name = folder.basename().map(|p| p.to_string_lossy().into_owned()).filter(|n| !n.is_empty())
                    .unwrap_or_else(|| folder.parse_name().to_string());
                self.title.set_text(&name);
            } else {
                self.title.set_text(&item_count(selected.len() as i64));
            }
            self.hint.set_text(if selected.is_empty() { "Select a file to preview" } else { "Select one file to preview" });
            self.text.set_text(&folder.parse_name());
            self.set_details(if selected.is_empty() { Vec::new() } else {
                vec![row("Total size", format_size(selected.iter().map(|i| i.size()).sum()))]
            });
            return;
        }
        let item = &selected[0];
        self.title.set_text(&item.display_name());
        self.text.set_text(&item.file().parse_name());
        let mut basic = vec![row("Type", item.type_description())];
        if !item.is_dir() {
            basic.push(row("File size", format_size(item.size())));
        }
        basic.push(row("Modified", format_modified(item.modified())));
        self.0.basic.replace(basic.clone());
        self.set_details(basic);
        self.preview.show(Some(item.clone()), Some(if item.is_dir() { "Select a file to preview" } else { "No preview for this file type" }));
    }

    pub fn clear(&self) {
        self.0.selection_key.replace(None);
        self.0.basic.replace(Vec::new());
        self.print_button.set_visible(false);
        self.preview.clear();
    }

    pub fn close(&self) {
        self.clear();
        self.preview.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_jpeg_gif_webp_modes() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01".to_vec();
        png.extend([8, 6, 0, 0, 0]);
        assert_eq!(pil_mode(&png), Some("RGBA"));
        png[25] = 2;
        assert_eq!(pil_mode(&png), Some("RGB"));
        png[25] = 0;
        png[24] = 1;
        assert_eq!(pil_mode(&png), Some("1"));
        let jpeg = [0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0, 0xff, 0xc0, 0, 17, 8, 0, 1, 0, 1, 3, 0, 0, 0];
        assert_eq!(pil_mode(&jpeg), Some("RGB"));
        assert_eq!(pil_mode(b"GIF89a....."), Some("P"));
        assert_eq!(pil_mode(b"not an image"), None);
    }

    #[test]
    fn kinds_follow_content_types() {
        assert_eq!(kind_for_type("image/png"), Some(IMAGE));
        assert_eq!(kind_for_type("application/pdf"), Some(PDF));
        assert_eq!(kind_for_type("video/mp4"), Some(MEDIA));
        assert_eq!(kind_for_type("audio/ogg"), Some(MEDIA));
        assert_eq!(kind_for_type("text/x-python"), Some(TEXT));
        assert_eq!(kind_for_type("application/octet-stream"), None);
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!("files-widgets-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn text_is_capped_on_a_line_boundary_and_binary_is_refused() {
        let root = scratch("text");
        let big = root.join("big.log");
        std::fs::write(&big, ("x".repeat(99) + "\n").repeat(20)).unwrap();
        let (text, truncated) = read_text(&gio::File::for_path(&big), None, 1050).unwrap();
        assert!(truncated);
        assert_eq!(text, ("x".repeat(99) + "\n").repeat(10));
        let blob = root.join("blob.txt");
        std::fs::write(&blob, b"text\x00binary").unwrap();
        let (payload, details, error) = decode_text(&gio::File::for_path(&blob), &gio::Cancellable::new());
        assert!(payload.is_none() && details.is_empty());
        assert_eq!(error, "This file contains binary data.");
        let script = root.join("script.py");
        std::fs::write(&script, "def hello():\n    return 42\n").unwrap();
        let (payload, details, _) = decode_text(&gio::File::for_path(&script), &gio::Cancellable::new());
        assert!(matches!(payload, Some(Payload::Text(ref t)) if t == "def hello():\n    return 42\n"));
        assert_eq!(details, vec![row("Lines", "2")]);
        let (_, _, error) = decode_text(&gio::File::for_path(root.join("missing")), &gio::Cancellable::new());
        assert_eq!(error, "Preview unavailable");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn image_decoding_scales_and_reports_dimensions() {
        let root = scratch("image");
        let path = root.join("wide.png");
        let pixbuf = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, 2400, 1600).unwrap();
        pixbuf.fill(0x438da3ff);
        pixbuf.savev(&path, "png", &[]).unwrap();
        let (payload, details, error) = decode_preview(&gio::File::for_path(&path), &gio::Cancellable::new());
        assert_eq!(error, "");
        let Some(Payload::Texture(texture)) = payload else { panic!("no texture") };
        assert_eq!((texture.width(), texture.height()), (1200, 800));
        assert_eq!(details[0], row("Dimensions", "2,400 × 1,600 pixels"));
        assert_eq!(details[1], row("Format", "PNG"));
        assert_eq!(details[2], row("Color mode", "RGB"));
        std::fs::write(root.join("broken.jpg"), b"broken jpeg image").unwrap();
        let (payload, _, error) = decode_preview(&gio::File::for_path(root.join("broken.jpg")), &gio::Cancellable::new());
        assert!(payload.is_none());
        assert_eq!(error, "Preview unavailable");
        let _ = std::fs::remove_dir_all(root);
    }
}
