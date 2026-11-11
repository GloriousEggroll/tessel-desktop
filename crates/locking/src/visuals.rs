//! `tessel-lock/lock_visuals.py`: GTK4 adaptation of the Kasumi idle clock's
//! wallpaper fade and voice band, plus the shared lock/login "family" widgets
//! (avatar, clock/date CSS, password chevron, preview control).
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{cairo, gdk, glib, graphene, gsk};
use tessel_core::tr;

use crate::config::{desktop_wallpaper, LockConfig, IMAGE_EXTENSIONS};
use crate::palette::wallpaper_color;

/// One visual family for the lock screen and the login greeter.
pub const AVATAR_SIZE: i32 = 96;
pub const ACCOUNT_ICONS: &str = "/var/lib/AccountsService/icons";
/// Wallpaper cross-fade length (µs).
pub const FADE_US: i64 = 1_800_000;
/// LoginLayer slide length (µs).
pub const SLIDE_US: i64 = 350_000;

const FAMILY_CSS: &str = include_str!("family.css");

/// `family_css(scope)`: shared lock/login rules, appended after each window's palette rules.
pub fn family_css(scope: &str) -> String { FAMILY_CSS.replace("@SCOPE@", scope) }

/// `long_date(value)`: locale-aware long date; the translated pattern orders the parts.
pub fn long_date(value: &glib::DateTime) -> String {
    let weekday = value.format("%A").map(|s| s.to_string()).unwrap_or_default();
    let month = value.format("%B").map(|s| s.to_string()).unwrap_or_default();
    tr!("{weekday}, {month} {day}", weekday = weekday, month = month, day = value.day_of_month() as i64)
}

/// `initials(name)`.
pub fn initials(name: &str) -> String {
    let cleaned = name.replace(['.', '_'], " ");
    let words: Vec<&str> = cleaned.split_whitespace().filter(|w| w.chars().next().is_some_and(char::is_alphanumeric)).collect();
    let Some(first) = words.first() else { return String::new() };
    let mut text: String = first.chars().next().into_iter().collect();
    if words.len() > 1 {
        text.extend(words[words.len() - 1].chars().next());
    }
    text.to_uppercase()
}

// ───────────────────────────── Avatar ─────────────────────────────

struct AvatarInner {
    widget: gtk::Box,
    icons: PathBuf,
    child: RefCell<Option<gtk::Widget>>,
    text: RefCell<String>,
}

/// `Avatar`: a round account picture, or the account's initials in a filled circle.
#[derive(Clone)]
pub struct Avatar(Rc<AvatarInner>);

impl Default for Avatar {
    fn default() -> Self { Self::new() }
}

impl Avatar {
    pub fn new() -> Self { Self::with_icons_dir(Path::new(ACCOUNT_ICONS)) }

    /// Tests point this at a private folder instead of AccountsService's.
    pub fn with_icons_dir(icons: &Path) -> Self {
        let widget = gtk::Box::builder().halign(gtk::Align::Center).valign(gtk::Align::Center).css_classes(["family-avatar"])
            .width_request(AVATAR_SIZE).height_request(AVATAR_SIZE).can_target(false).build();
        widget.set_overflow(gtk::Overflow::Hidden);
        Self(Rc::new(AvatarInner { widget, icons: icons.to_path_buf(), child: RefCell::new(None), text: RefCell::new(String::new()) }))
    }

    pub fn widget(&self) -> &gtk::Box { &self.0.widget }
    pub fn pixel_size(&self) -> i32 { AVATAR_SIZE }
    pub fn text(&self) -> String { self.0.text.borrow().clone() }
    pub fn child(&self) -> Option<gtk::Widget> { self.0.child.borrow().clone() }

    /// `set_user(name, username=None)`. AccountsService pictures are local
    /// system files; never the user's home.
    pub fn set_user(&self, name: &str, username: Option<&str>) {
        let picture = username.filter(|u| !u.is_empty() && !u.contains('/')).map(|u| self.0.icons.join(u));
        if let Some(child) = self.0.child.borrow_mut().take() {
            self.0.widget.remove(&child);
        }
        let text = initials(name);
        self.0.text.replace(text.clone());
        let child: gtk::Widget = if let Some(picture) = picture.filter(|p| p.is_file()) {
            let child = gtk::Picture::for_filename(&picture);
            child.set_content_fit(gtk::ContentFit::Cover);
            child.set_hexpand(true);
            child.set_valign(gtk::Align::Fill);
            child.upcast()
        } else if !text.is_empty() {
            let child = gtk::Label::builder().label(&text).hexpand(true).halign(gtk::Align::Center).build();
            child.set_valign(gtk::Align::Center);
            child.upcast()
        } else {
            let child = gtk::Image::builder().icon_name("avatar-default-symbolic").pixel_size(AVATAR_SIZE / 2)
                .hexpand(true).halign(gtk::Align::Center).build();
            child.set_valign(gtk::Align::Center);
            child.upcast()
        };
        child.set_vexpand(true);
        self.0.widget.append(&child);
        self.0.child.replace(Some(child));
    }
}

/// `preview_close_button(callback)`: the preview-only exit control, identical
/// on lock and login (margins 16/16; callers may override them afterwards).
pub fn preview_close_button(callback: impl Fn() + 'static) -> gtk::Button {
    let button = gtk::Button::builder().label(tr!("Preview · Close")).halign(gtk::Align::End).valign(gtk::Align::Start)
        .css_classes(["family-preview"]).margin_top(16).margin_end(16).build();
    button.update_property(&[gtk::accessible::Property::Label(&tr!("Close preview"))]);
    button.connect_clicked(move |_| callback());
    button
}

/// `submit_icon()`: the same centred, theme-coloured chevron on lock and login.
pub fn submit_icon() -> gtk::DrawingArea {
    let icon = gtk::DrawingArea::builder().content_width(20).content_height(20).halign(gtk::Align::Center)
        .valign(gtk::Align::Center).can_target(false).build();
    icon.set_draw_func(|widget, context, width, height| {
        let color = widget.color();
        context.set_source_rgba(color.red() as f64, color.green() as f64, color.blue() as f64, color.alpha() as f64);
        context.translate(width as f64 / 2.0, height as f64 / 2.0);
        context.set_line_width(1.5);
        context.set_line_cap(cairo::LineCap::Round);
        context.set_line_join(cairo::LineJoin::Round);
        context.move_to(-3.5, -7.0);
        context.line_to(3.5, 0.0);
        context.line_to(-3.5, 7.0);
        let _ = context.stroke();
    });
    icon
}

// ───────────────────────── wallpaper preparation ─────────────────────────

/// Packed 8-bit RGB pixels (`width * 3` bytes per row).
#[derive(Clone, Debug, PartialEq)]
pub struct Rgb {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl Rgb {
    pub fn filled(width: usize, height: usize, color: (u8, u8, u8)) -> Self {
        Self { width, height, data: [color.0, color.1, color.2].repeat(width * height) }
    }

    fn from_pixbuf(pixbuf: &gtk::gdk_pixbuf::Pixbuf) -> Self {
        let (width, height) = (pixbuf.width() as usize, pixbuf.height() as usize);
        let channels = pixbuf.n_channels() as usize;
        let stride = pixbuf.rowstride() as usize;
        let bytes = pixbuf.read_pixel_bytes();
        let mut data = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            let row = &bytes[y * stride..];
            for x in 0..width {
                data.extend_from_slice(&row[x * channels..x * channels + 3]);
            }
        }
        Self { width, height, data }
    }

    /// `Image.paste(picture, (x, y))` (clipped).
    pub fn paste(&mut self, picture: &Rgb, x: i64, y: i64) {
        for row in 0..picture.height as i64 {
            let ty = y + row;
            if ty < 0 || ty >= self.height as i64 {
                continue;
            }
            for col in 0..picture.width as i64 {
                let tx = x + col;
                if tx < 0 || tx >= self.width as i64 {
                    continue;
                }
                let s = ((row as usize) * picture.width + col as usize) * 3;
                let d = ((ty as usize) * self.width + tx as usize) * 3;
                self.data[d..d + 3].copy_from_slice(&picture.data[s..s + 3]);
            }
        }
    }
}

/// PIL `_gaussian_blur_radius(radius, passes)`: the extended box radius.
pub fn box_radius(radius: f64, passes: u32) -> f64 {
    let sigma2 = radius * radius / passes as f64;
    let l_full = (12.0 * sigma2 + 1.0).sqrt();
    let l = ((l_full - 1.0) / 2.0).floor();
    let mut a = (2.0 * l + 1.0) * (l * (l + 1.0) - 3.0 * sigma2);
    a /= 6.0 * (sigma2 - (l + 1.0) * (l + 1.0));
    l + a
}

fn box_line(input: &[f32], output: &mut [f32], radius: f64) {
    // Extended box filter (fractional edge weights), edges clamped like PIL.
    let n = input.len() as i64;
    let whole = radius.floor() as i64;
    let edge = (radius - whole as f64) as f32;
    let scale = 1.0 / (2.0 * radius as f32 + 1.0);
    let at = |i: i64| input[i.clamp(0, n - 1) as usize];
    let mut sum: f32 = (-whole..=whole).map(at).sum();
    for i in 0..n {
        output[i as usize] = (sum + edge * (at(i - whole - 1) + at(i + whole + 1))) * scale;
        sum += at(i + whole + 1) - at(i - whole);
    }
}

/// `ImageFilter.GaussianBlur(radius)`: three extended box passes per axis.
pub fn gaussian_blur(image: &Rgb, radius: f64) -> Rgb {
    if radius <= 0.0 || image.width == 0 || image.height == 0 {
        return image.clone();
    }
    let boxed = box_radius(radius, 3);
    let (w, h) = (image.width, image.height);
    let mut planes: Vec<Vec<f32>> = (0..3).map(|c| image.data.iter().skip(c).step_by(3).map(|v| *v as f32).collect()).collect();
    for plane in &mut planes {
        for _ in 0..3 {
            let mut line = vec![0f32; w.max(h)];
            for y in 0..h {
                let row = &plane[y * w..(y + 1) * w];
                box_line(row, &mut line[..w], boxed);
                plane[y * w..(y + 1) * w].copy_from_slice(&line[..w]);
            }
            let mut column = vec![0f32; h];
            for x in 0..w {
                for y in 0..h {
                    column[y] = plane[y * w + x];
                }
                box_line(&column, &mut line[..h], boxed);
                for y in 0..h {
                    plane[y * w + x] = line[y];
                }
            }
        }
    }
    let mut data = vec![0u8; w * h * 3];
    for (c, plane) in planes.iter().enumerate() {
        for (i, value) in plane.iter().enumerate() {
            data[i * 3 + c] = value.round().clamp(0.0, 255.0) as u8;
        }
    }
    Rgb { width: w, height: h, data }
}

/// PIL `Image.thumbnail(size)` target size (`None` = unchanged).
pub fn thumbnail_size(width: usize, height: usize, x: usize, y: usize) -> Option<(usize, usize)> {
    if x >= width && y >= height {
        return None;
    }
    let aspect = width as f64 / height as f64;
    let (xf, yf) = (x as f64, y as f64);
    let round_aspect = |number: f64, key: &dyn Fn(f64) -> f64| -> usize {
        let (lo, hi) = (number.floor(), number.ceil());
        let pick = if key(hi) < key(lo) { hi } else { lo };
        (pick as usize).max(1)
    };
    if xf / yf >= aspect {
        Some((round_aspect(yf * aspect, &|n| (aspect - n / yf).abs()), y))
    } else {
        Some((x, round_aspect(xf / aspect, &|n| if n == 0.0 { 0.0 } else { (aspect - xf / n).abs() })))
    }
}

/// `ImageOps.fit` crop box (centering .5/.5) for an output size.
pub fn fit_crop(width: usize, height: usize, out_w: usize, out_h: usize) -> (f64, f64, f64, f64) {
    let (lw, lh) = (width as f64, height as f64);
    let live = lw / lh;
    let output = out_w as f64 / out_h as f64;
    let (cw, ch) = if live == output { (lw, lh) } else if live >= output { (output * lh, lh) } else { (lw, lw / output) };
    ((lw - cw) * 0.5, (lh - ch) * 0.5, cw, ch)
}

fn scaled(pixbuf: &gtk::gdk_pixbuf::Pixbuf, crop: (f64, f64, f64, f64), out_w: usize, out_h: usize,
          interp: gtk::gdk_pixbuf::InterpType) -> Option<gtk::gdk_pixbuf::Pixbuf> {
    let dest = gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, pixbuf.has_alpha(), 8, out_w as i32, out_h as i32)?;
    let (sx, sy) = (out_w as f64 / crop.2, out_h as f64 / crop.3);
    pixbuf.scale(&dest, 0, 0, out_w as i32, out_h as i32, -crop.0 * sx, -crop.1 * sy, sx, sy, interp);
    Some(dest)
}

/// Result of [`prepare_wallpaper`]: sharp pixels, blurred pixels (`None` =
/// same as sharp), the sampled palette colour and the size.
pub struct Prepared {
    pub sharp: Rgb,
    pub blurred: Option<Rgb>,
    pub color: (u8, u8, u8),
}

pub fn parse_color(color: &str) -> (u8, u8, u8) {
    let channel = |i: usize| color.get(i..i + 2).and_then(|s| u8::from_str_radix(s, 16).ok()).unwrap_or(0);
    (channel(1), channel(3), channel(5))
}

/// `prepare_wallpaper(path, width, height, placement, radius, color)`:
/// decode, orient, place, blur and sample — all off the GTK thread.
pub fn prepare_wallpaper(path: &Path, width: i32, height: i32, placement: &str, radius: f64, color: &str) -> Result<Prepared, String> {
    use gtk::gdk_pixbuf::InterpType;
    let factor = 1f64.min(2048.0 / width.max(height) as f64);
    let size = (((width as f64 * factor).round() as usize).max(1), ((height as f64 * factor).round() as usize).max(1));
    let source = gtk::gdk_pixbuf::Pixbuf::from_file(path).map_err(|e| e.to_string())?;
    let source = source.apply_embedded_orientation().unwrap_or(source);
    let (sw, sh) = (source.width() as usize, source.height() as usize);
    let full = (0.0, 0.0, sw as f64, sh as f64);
    let picture = match placement {
        "fill" => Rgb::from_pixbuf(&scaled(&source, fit_crop(sw, sh, size.0, size.1), size.0, size.1, InterpType::Hyper).ok_or("scale")?),
        "stretch" => Rgb::from_pixbuf(&scaled(&source, full, size.0, size.1, InterpType::Hyper).ok_or("scale")?),
        _ => {
            let inner = if placement == "fit" {
                match thumbnail_size(sw, sh, size.0, size.1) {
                    Some((w, h)) => Rgb::from_pixbuf(&scaled(&source, full, w, h, InterpType::Hyper).ok_or("scale")?),
                    None => Rgb::from_pixbuf(&source),
                }
            } else {
                let (w, h) = (((sw as f64 * factor).round() as usize).max(1), ((sh as f64 * factor).round() as usize).max(1));
                Rgb::from_pixbuf(&scaled(&source, full, w, h, InterpType::Bilinear).ok_or("scale")?)
            };
            let mut background = Rgb::filled(size.0, size.1, parse_color(color));
            let x = (size.0 as i64 - inner.width as i64).div_euclid(2);
            let y = (size.1 as i64 - inner.height as i64).div_euclid(2);
            background.paste(&inner, x, y);
            background
        }
    };
    let blurred = (radius > 0.0).then(|| gaussian_blur(&picture, radius * factor));
    let color = wallpaper_color(&picture.data, picture.width, picture.height, picture.width * 3);
    Ok(Prepared { sharp: picture, blurred, color })
}

fn texture(image: &Rgb) -> gdk::Texture {
    gdk::MemoryTexture::new(image.width as i32, image.height as i32, gdk::MemoryFormat::R8g8b8,
                            &glib::Bytes::from(&image.data[..]), image.width * 3).upcast()
}

fn rgba(red: f64, green: f64, blue: f64, alpha: f64) -> gdk::RGBA { gdk::RGBA::new(red as f32, green as f32, blue as f32, alpha as f32) }

fn shuffle<T>(items: &mut [T]) {
    let mut seed = [0u8; 8];
    if let Ok(mut random) = std::fs::File::open("/dev/urandom") {
        let _ = std::io::Read::read_exact(&mut random, &mut seed);
    }
    let mut state = u64::from_le_bytes(seed) | 1;
    for i in (1..items.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        items.swap(i, (state % (i as u64 + 1)) as usize);
    }
}

fn suffix(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // pathlib suffix: the last ".x" of the name, not a leading dot.
    match name.rfind('.') {
        Some(index) if index > 0 && index + 1 < name.len() => name[index..].to_lowercase(),
        _ => String::new(),
    }
}

fn is_image(path: &Path) -> bool { IMAGE_EXTENSIONS.contains(&suffix(path).as_str()) }

// ───────────────────────────── Wallpaper ─────────────────────────────

type Signature = (Option<PathBuf>, Option<(i128, u64)>, (i32, i32), String, i64, String);

mod wallpaper_imp {
    use super::*;

    pub struct Wallpaper {
        pub monitor: RefCell<String>,
        pub config: RefCell<Option<LockConfig>>,
        pub blur: Cell<i64>,
        pub surface: RefCell<Option<gdk::Texture>>,
        pub previous: RefCell<Option<gdk::Texture>>,
        pub blurred: RefCell<Option<gdk::Texture>>,
        pub previous_blurred: RefCell<Option<gdk::Texture>>,
        pub overlay: RefCell<Option<glib::WeakRef<gtk::Widget>>>,
        pub tint: Cell<(f64, f64, f64, f64)>,
        pub signature: RefCell<Option<Signature>>,
        pub current_path: RefCell<Option<PathBuf>>,
        pub deck: RefCell<Vec<PathBuf>>,
        pub fade_started: Cell<i64>,
        pub next_rotation: Cell<f64>,
        pub generation: Cell<u64>,
        pub animator: RefCell<Option<gtk::TickCallbackId>>,
        pub closed: Cell<bool>,
        pub changed: RefCell<Rc<dyn Fn()>>,
        pub palette_color: Cell<(u8, u8, u8)>,
        pub prepared: Cell<u32>,
    }

    impl Default for Wallpaper {
        fn default() -> Self {
            Self {
                monitor: RefCell::default(), config: RefCell::default(), blur: Cell::new(0), surface: RefCell::default(),
                previous: RefCell::default(), blurred: RefCell::default(), previous_blurred: RefCell::default(),
                overlay: RefCell::default(), tint: Cell::new((0.025, 0.04, 0.065, 0.12)), signature: RefCell::default(),
                current_path: RefCell::default(), deck: RefCell::default(), fade_started: Cell::new(0), next_rotation: Cell::new(0.0),
                generation: Cell::new(0), animator: RefCell::default(), closed: Cell::new(false),
                changed: RefCell::new(Rc::new(|| {})), palette_color: Cell::new((22, 24, 32)), prepared: Cell::new(0),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Wallpaper {
        const NAME: &'static str = "SuiteLockWallpaper";
        type Type = super::Wallpaper;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Wallpaper {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_hexpand(true);
            obj.set_vexpand(true);
            obj.connect_unrealize(|widget| widget.close());
        }

        fn dispose(&self) { self.obj().close(); }
    }

    impl WidgetImpl for Wallpaper {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) { (0, 0, -1, -1) }

        fn size_allocate(&self, _width: i32, _height: i32, _baseline: i32) { self.obj().refresh(); }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            obj.snapshot_background(snapshot, obj.width(), obj.height(), false);
        }
    }
}

glib::wrapper! {
    /// `Wallpaper(monitor, executor, changed)`: the opaque, cross-fading
    /// wallpaper (desktop / picture / slideshow / colour).
    pub struct Wallpaper(ObjectSubclass<wallpaper_imp::Wallpaper>) @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Wallpaper {
    /// `monitor` is the output connector (for `desktop_wallpaper`). Decoding
    /// runs on GIO's blocking pool (Python: the window's ThreadPoolExecutor).
    pub fn new(monitor: &str, changed: impl Fn() + 'static) -> Self {
        let widget: Self = glib::Object::new();
        widget.imp().monitor.replace(monitor.to_owned());
        widget.imp().changed.replace(Rc::new(changed));
        widget
    }

    pub fn set_changed(&self, changed: impl Fn() + 'static) { self.imp().changed.replace(Rc::new(changed)); }
    pub fn config(&self) -> Option<LockConfig> { self.imp().config.borrow().clone() }
    pub fn palette_color(&self) -> (u8, u8, u8) { self.imp().palette_color.get() }
    pub fn tint(&self) -> (f64, f64, f64, f64) { self.imp().tint.get() }
    /// Set `self.background.tint`; call [`Self::redraw`] afterwards (as Python does).
    pub fn set_tint(&self, tint: (f64, f64, f64, f64)) { self.imp().tint.set(tint) }
    pub fn current_path(&self) -> Option<PathBuf> { self.imp().current_path.borrow().clone() }
    pub fn surface(&self) -> Option<gdk::Texture> { self.imp().surface.borrow().clone() }
    pub fn blurred(&self) -> Option<gdk::Texture> { self.imp().blurred.borrow().clone() }
    pub fn previous(&self) -> Option<gdk::Texture> { self.imp().previous.borrow().clone() }
    pub fn is_closed(&self) -> bool { self.imp().closed.get() }
    pub fn is_animating(&self) -> bool { self.imp().animator.borrow().is_some() }
    /// Number of decode jobs started (tests assert that reveals reuse textures).
    pub fn prepare_count(&self) -> u32 { self.imp().prepared.get() }
    /// `wallpaper.next_rotation = 0` (`--next-wallpaper`); follow with [`Self::refresh`].
    pub fn set_next_rotation(&self, value: f64) { self.imp().next_rotation.set(value) }

    /// `configure(config, blur)`.
    pub fn configure(&self, config: &LockConfig, blur: i64) {
        let imp = self.imp();
        if imp.config.borrow().as_ref() != Some(config) {
            imp.deck.borrow_mut().clear();
            imp.next_rotation.set(0.0);
        }
        imp.config.replace(Some(config.clone()));
        imp.blur.set(blur);
        self.refresh();
    }

    pub fn refresh(&self) {
        let imp = self.imp();
        let Some(config) = imp.config.borrow().clone() else { return };
        if imp.closed.get() || self.width() == 0 || self.height() == 0 {
            return;
        }
        let path: Option<PathBuf> = match config.wallpaper_mode.as_str() {
            "desktop" => desktop_wallpaper(&imp.monitor.borrow()),
            // One of the Slate renders (the login screen's default) becomes
            // the render made for this screen's shape, as on the desktop.
            "picture" => (!config.picture.is_empty()).then(|| tessel_core::wallpaper::render_for_screen(
                &crate::expanduser(Path::new(&config.picture)), Some((self.width(), self.height())))),
            "slideshow" => {
                if crate::voice_state::monotonic() >= imp.next_rotation.get() {
                    let mut deck = imp.deck.borrow_mut();
                    if deck.is_empty() {
                        *deck = config.slideshow.iter().map(PathBuf::from)
                            .filter(|item| is_image(item) && crate::expanduser(item).is_file())
                            .map(|item| crate::expanduser(&item)).collect();
                        shuffle(&mut deck);
                        let current = imp.current_path.borrow().clone();
                        if deck.len() > 1 && deck.last() == current.as_ref() {
                            let last = deck.len() - 1;
                            deck.swap(0, last);
                        }
                    }
                    imp.current_path.replace(deck.pop());
                    imp.next_rotation.set(crate::voice_state::monotonic() + config.interval as f64);
                }
                imp.current_path.borrow().clone()
            }
            _ => None,
        };
        let size = (self.width(), self.height());
        let stamp = path.as_ref().and_then(|p| std::fs::metadata(p).ok()).map(|info| {
            use std::os::unix::fs::MetadataExt;
            (info.mtime() as i128 * 1_000_000_000 + info.mtime_nsec() as i128, info.size())
        });
        let signature: Signature = (path.clone(), stamp, size, config.placement.clone(), imp.blur.get(), config.background_color.clone());
        if imp.signature.borrow().as_ref() == Some(&signature) {
            return;
        }
        imp.signature.replace(Some(signature));
        imp.generation.set(imp.generation.get() + 1);
        let serial = imp.generation.get();
        let Some(path) = path.filter(|p| is_image(p)) else {
            imp.previous.replace(None);
            imp.surface.replace(None);
            imp.previous_blurred.replace(None);
            imp.blurred.replace(None);
            imp.palette_color.set(parse_color(&config.background_color));
            self.notify_changed();
            self.redraw();
            return;
        };
        imp.prepared.set(imp.prepared.get() + 1);
        let (placement, color, radius) = (config.placement.clone(), config.background_color.clone(), imp.blur.get() as f64 * 0.4);
        let handle = gio::spawn_blocking(move || prepare_wallpaper(&path, size.0, size.1, &placement, radius, &color));
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let result = handle.await.unwrap_or_else(|_| Err("worker failed".into()));
            if let Some(widget) = weak.upgrade() {
                widget.loaded(result, serial);
            }
        });
    }

    fn notify_changed(&self) {
        let changed = self.imp().changed.borrow().clone();
        changed();
    }

    fn loaded(&self, result: Result<Prepared, String>, serial: u64) {
        let imp = self.imp();
        if imp.closed.get() || serial != imp.generation.get() {
            return;
        }
        let (surface, softened, color) = match result {
            Ok(prepared) => {
                let surface = texture(&prepared.sharp);
                let softened = prepared.blurred.as_ref().map(texture).unwrap_or_else(|| surface.clone());
                (Some(surface), Some(softened), prepared.color)
            }
            Err(_) => (None, None, parse_color(&imp.config.borrow().as_ref().map(|c| c.background_color.clone()).unwrap_or_default())),
        };
        imp.palette_color.set(color);
        let old = imp.surface.replace(surface);
        imp.previous.replace(old);
        let old = imp.blurred.replace(softened);
        imp.previous_blurred.replace(old);
        imp.fade_started.set(self.frame_clock().map(|c| c.frame_time()).unwrap_or_else(glib::monotonic_time));
        self.notify_changed();
        self.redraw();
        if imp.previous.borrow().is_some() && imp.animator.borrow().is_none() {
            let id = self.add_tick_callback(|widget, clock| widget.frame(clock.frame_time()));
            imp.animator.replace(Some(id));
        }
    }

    /// Redraw the wallpaper and the login backdrop that mirrors it.
    pub fn redraw(&self) {
        self.queue_draw();
        if let Some(overlay) = self.imp().overlay.borrow().as_ref().and_then(|w| w.upgrade()) {
            overlay.queue_draw();
        }
    }

    fn frame(&self, now: i64) -> glib::ControlFlow {
        let imp = self.imp();
        self.redraw();
        if imp.closed.get() || now - imp.fade_started.get() >= FADE_US {
            imp.previous.replace(None);
            imp.previous_blurred.replace(None);
            imp.animator.replace(None);
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    }

    /// Cross-fade fraction at `now` (µs), smoothstepped.
    pub fn fade_mix(&self, now: i64) -> f64 {
        let mix = ((now - self.imp().fade_started.get()) as f64 / FADE_US as f64).clamp(0.0, 1.0);
        mix * mix * (3.0 - 2.0 * mix)
    }

    /// `snapshot_background(snapshot, width, height, blurred=False)`: opaque
    /// colour underlay (mandatory, including while images load or fail), the
    /// fading textures and, for the login backdrop, tint + vertical gradient.
    pub fn snapshot_background(&self, snapshot: &gtk::Snapshot, width: i32, height: i32, blurred: bool) {
        let imp = self.imp();
        let bounds = graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
        let color = imp.config.borrow().as_ref().map(|c| c.background_color.clone()).unwrap_or_else(|| "#161820".into());
        let (r, g, b) = parse_color(&color);
        snapshot.append_color(&rgba(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0, 1.0), &bounds);
        let now = self.frame_clock().map(|c| c.frame_time()).unwrap_or(imp.fade_started.get());
        let mix = self.fade_mix(now);
        let (previous, current) = if blurred {
            (imp.previous_blurred.borrow().clone(), imp.blurred.borrow().clone())
        } else {
            (imp.previous.borrow().clone(), imp.surface.borrow().clone())
        };
        if let Some(previous) = &previous {
            snapshot.append_texture(previous, &bounds);
        }
        if let Some(current) = &current {
            if previous.is_some() {
                snapshot.push_opacity(mix);
            }
            snapshot.append_texture(current, &bounds);
            if previous.is_some() {
                snapshot.pop();
            }
        }
        if blurred {
            let (tr_, tg, tb, ta) = imp.tint.get();
            snapshot.append_color(&rgba(tr_, tg, tb, ta), &bounds);
            let stops = [(0.0, 0.04), (0.48, 0.12), (1.0, 0.28)].map(|(offset, alpha)| gsk::ColorStop::new(offset, rgba(0.0, 0.0, 0.0, alpha)));
            snapshot.append_linear_gradient(&bounds, &graphene::Point::new(0.0, 0.0), &graphene::Point::new(0.0, height as f32), &stops);
        }
    }

    pub fn close(&self) {
        let imp = self.imp();
        imp.closed.set(true);
        imp.generation.set(imp.generation.get() + 1);
        if let Some(id) = imp.animator.borrow_mut().take() {
            id.remove();
        }
        imp.surface.replace(None);
        imp.previous.replace(None);
        imp.blurred.replace(None);
        imp.previous_blurred.replace(None);
        imp.overlay.replace(None);
    }

    fn set_overlay(&self, overlay: &gtk::Widget) { self.imp().overlay.replace(Some(overlay.downgrade())); }
}

// ─────────────────────────── LoginLayer ───────────────────────────

mod backdrop_imp {
    use super::*;

    #[derive(Default)]
    pub struct LoginBackdrop {
        pub wallpaper: RefCell<Option<glib::WeakRef<super::Wallpaper>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LoginBackdrop {
        const NAME: &'static str = "SuiteLockLoginBackdrop";
        type Type = super::LoginBackdrop;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for LoginBackdrop {}

    impl WidgetImpl for LoginBackdrop {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            if let Some(wallpaper) = self.wallpaper.borrow().as_ref().and_then(|w| w.upgrade()) {
                wallpaper.snapshot_background(snapshot, obj.width(), obj.height(), true);
            }
        }
    }
}

glib::wrapper! {
    /// The stationary blurred copy of the wallpaper behind the login form.
    pub struct LoginBackdrop(ObjectSubclass<backdrop_imp::LoginBackdrop>) @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// The LoginLayer slide state (pure; the widget drives it from its frame clock).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slide {
    pub progress: f64,
    pub start: f64,
    pub target: f64,
    pub started: Option<i64>,
}

impl Default for Slide {
    fn default() -> Self { Self { progress: 1.0, start: 1.0, target: 1.0, started: None } }
}

impl Slide {
    /// Begin moving towards `target` from the current progress (reversal never jumps).
    pub fn retarget(&mut self, target: f64) {
        self.start = self.progress;
        self.target = target;
        self.started = None;
    }

    /// One frame at `now` (µs): ease-out cubic over 350 ms. Returns true when finished.
    pub fn step(&mut self, now: i64) -> bool {
        let started = *self.started.get_or_insert(now);
        let fraction = ((now - started) as f64 / SLIDE_US as f64).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - fraction).powi(3);
        self.progress = self.start + (self.target - self.start) * eased;
        fraction >= 1.0
    }
}

mod layer_imp {
    use super::*;

    pub struct LoginLayer {
        pub content: RefCell<Option<gtk::Widget>>,
        pub backdrop: RefCell<Option<super::LoginBackdrop>>,
        pub settled: RefCell<Rc<dyn Fn(bool)>>,
        pub slide: Cell<Slide>,
        pub animation: RefCell<Option<gtk::TickCallbackId>>,
    }

    impl Default for LoginLayer {
        fn default() -> Self {
            Self { content: RefCell::default(), backdrop: RefCell::default(), settled: RefCell::new(Rc::new(|_| {})),
                   slide: Cell::new(Slide::default()), animation: RefCell::default() }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LoginLayer {
        const NAME: &'static str = "SuiteLockLoginLayer";
        type Type = super::LoginLayer;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for LoginLayer {
        fn dispose(&self) { self.obj().close(); }
    }

    impl WidgetImpl for LoginLayer {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) { (0, 0, -1, -1) }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(backdrop) = self.backdrop.borrow().as_ref() {
                let _ = backdrop.measure(gtk::Orientation::Vertical, width);
                backdrop.allocate(width, height, baseline, None);
            }
            if let Some(content) = self.content.borrow().as_ref() {
                let progress = self.slide.get().progress;
                let _ = content.measure(gtk::Orientation::Vertical, width);
                let offset = graphene::Point::new(0.0, (-(height as f64) * (1.0 - progress)) as f32);
                content.allocate(width, height, baseline, Some(gsk::Transform::new().translate(&offset)));
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let progress = self.slide.get().progress;
            if progress <= 0.0 {
                return;
            }
            snapshot.push_clip(&graphene::Rect::new(0.0, 0.0, obj.width() as f32, (obj.height() as f64 * progress) as f32));
            if let Some(backdrop) = self.backdrop.borrow().as_ref() {
                obj.snapshot_child(backdrop, snapshot);
            }
            if let Some(content) = self.content.borrow().as_ref() {
                obj.snapshot_child(content, snapshot);
            }
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    /// `LoginLayer(wallpaper, content, settled=...)`: slide the form while
    /// revealing a stationary, aligned blur of the wallpaper. Only the content
    /// allocation moves; the texture-backed backdrop is clipped in screen
    /// coordinates. Starts fully shown (progress 1).
    pub struct LoginLayer(ObjectSubclass<layer_imp::LoginLayer>) @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl LoginLayer {
    pub fn new(wallpaper: &Wallpaper, content: &impl IsA<gtk::Widget>, settled: impl Fn(bool) + 'static) -> Self {
        let layer: Self = glib::Object::new();
        layer.set_hexpand(true);
        layer.set_vexpand(true);
        let backdrop: LoginBackdrop = glib::Object::builder().property("can-target", false).build();
        backdrop.imp().wallpaper.replace(Some(wallpaper.downgrade()));
        wallpaper.set_overlay(backdrop.upcast_ref());
        backdrop.set_parent(&layer);
        content.set_parent(&layer);
        let imp = layer.imp();
        imp.backdrop.replace(Some(backdrop));
        imp.content.replace(Some(content.clone().upcast()));
        imp.settled.replace(Rc::new(settled));
        layer.set_overflow(gtk::Overflow::Hidden);
        layer
    }

    pub fn content(&self) -> Option<gtk::Widget> { self.imp().content.borrow().clone() }
    pub fn backdrop(&self) -> Option<LoginBackdrop> { self.imp().backdrop.borrow().clone() }
    pub fn progress(&self) -> f64 { self.imp().slide.get().progress }
    pub fn target(&self) -> f64 { self.imp().slide.get().target }
    pub fn is_animating(&self) -> bool { self.imp().animation.borrow().is_some() }

    /// Force a progress (tests: `layer.progress = .5`), re-allocating and redrawing.
    pub fn set_progress(&self, progress: f64) {
        let mut slide = self.imp().slide.get();
        slide.progress = progress;
        self.imp().slide.set(slide);
        self.queue_allocate();
        self.queue_draw();
    }

    /// `reveal(visible, animate=True)`.
    pub fn reveal(&self, visible: bool, animate: bool) {
        let imp = self.imp();
        let target = if visible { 1.0 } else { 0.0 };
        if let Some(content) = imp.content.borrow().as_ref() {
            content.set_sensitive(visible);
        }
        self.set_can_target(visible);
        let mut slide = imp.slide.get();
        if slide.target == target && animate {
            return;
        }
        slide.retarget(target);
        if !animate {
            self.cancel_animation();
            slide.progress = target;
            imp.slide.set(slide);
            self.queue_allocate();
            self.queue_draw();
            let settled = imp.settled.borrow().clone();
            settled(target != 0.0);
        } else {
            imp.slide.set(slide);
            if imp.animation.borrow().is_none() {
                let id = self.add_tick_callback(|layer, clock| layer.frame_at(clock.frame_time()));
                imp.animation.replace(Some(id));
            }
        }
    }

    /// Remove the frame-clock tick (tests then drive [`Self::frame_at`] with a fake clock).
    pub fn cancel_animation(&self) {
        if let Some(id) = self.imp().animation.borrow_mut().take() {
            id.remove();
        }
    }

    /// `frame(widget, clock)` at frame time `now` (µs).
    pub fn frame_at(&self, now: i64) -> glib::ControlFlow {
        let imp = self.imp();
        let mut slide = imp.slide.get();
        let finished = slide.step(now);
        imp.slide.set(slide);
        self.queue_allocate();
        self.queue_draw();
        if finished {
            imp.animation.replace(None);
            let settled = imp.settled.borrow().clone();
            settled(slide.target != 0.0);
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    }

    pub fn close(&self) {
        self.cancel_animation();
        let imp = self.imp();
        if let Some(content) = imp.content.borrow_mut().take() {
            if content.parent().as_ref() == Some(self.upcast_ref()) {
                content.unparent();
            }
        }
        if let Some(backdrop) = imp.backdrop.borrow_mut().take() {
            if backdrop.parent().as_ref() == Some(self.upcast_ref()) {
                backdrop.unparent();
            }
        }
    }
}

// ─────────────────────────── VoiceBand ───────────────────────────

struct VoiceInner {
    area: gtk::DrawingArea,
    state: Cell<&'static str>,
    display_state: Cell<&'static str>,
    alpha: Cell<f64>,
    animator: RefCell<Option<glib::SourceId>>,
    last_frame: Cell<f64>,
}

/// `VoiceBand`: the original 37-bar phase representation — not a microphone
/// level meter (it only reflects the state parsed from Kasumi's log).
#[derive(Clone)]
pub struct VoiceBand(Rc<VoiceInner>);

impl Default for VoiceBand {
    fn default() -> Self { Self::new() }
}

fn voice_color(state: &str) -> (f64, f64, f64) {
    match state {
        "thinking" => (0.35, 0.65, 1.0),
        "speaking" => (0.67, 0.55, 1.0),
        _ => (0.12, 0.93, 1.0),
    }
}

impl VoiceBand {
    pub fn new() -> Self {
        let area = gtk::DrawingArea::builder().height_request(120).hexpand(true).valign(gtk::Align::End).can_target(false).build();
        let band = Self(Rc::new(VoiceInner {
            area, state: Cell::new("idle"), display_state: Cell::new("listening"), alpha: Cell::new(0.0),
            animator: RefCell::new(None), last_frame: Cell::new(crate::voice_state::monotonic()),
        }));
        let weak = Rc::downgrade(&band.0);
        band.0.area.set_draw_func(move |_area, cr, width, height| {
            if let Some(inner) = weak.upgrade() {
                VoiceBand(inner).draw(cr, width as f64, height as f64);
            }
        });
        let weak = Rc::downgrade(&band.0);
        band.0.area.connect_unrealize(move |_| {
            if let Some(inner) = weak.upgrade() {
                VoiceBand(inner).close();
            }
        });
        band
    }

    pub fn widget(&self) -> &gtk::DrawingArea { &self.0.area }
    pub fn state(&self) -> &'static str { self.0.state.get() }
    pub fn display_state(&self) -> &'static str { self.0.display_state.get() }
    pub fn alpha(&self) -> f64 { self.0.alpha.get() }
    pub fn is_animating(&self) -> bool { self.0.animator.borrow().is_some() }

    /// `update(state)`: unknown states are treated as idle.
    pub fn update(&self, state: &str) {
        let state: &'static str = match state {
            "listening" => "listening",
            "thinking" => "thinking",
            "speaking" => "speaking",
            _ => "idle",
        };
        self.0.state.set(state);
        if state != "idle" {
            self.0.display_state.set(state);
        }
        if self.0.animator.borrow().is_none() && (state != "idle" || self.0.alpha.get() != 0.0) {
            self.0.last_frame.set(crate::voice_state::monotonic());
            let weak = Rc::downgrade(&self.0);
            let id = glib::timeout_add_local(std::time::Duration::from_millis(33), move || match weak.upgrade() {
                Some(inner) => VoiceBand(inner).frame(),
                None => glib::ControlFlow::Break,
            });
            self.0.animator.replace(Some(id));
        }
    }

    fn frame(&self) -> glib::ControlFlow {
        let inner = &self.0;
        let now = crate::voice_state::monotonic();
        let step = 1f64.min((now - inner.last_frame.get()) / 0.35);
        inner.last_frame.set(now);
        let alpha = inner.alpha.get();
        inner.alpha.set(if inner.state.get() == "idle" { (alpha - step).max(0.0) } else { (alpha + step).min(1.0) });
        inner.area.queue_draw();
        if inner.alpha.get() == 0.0 && inner.state.get() == "idle" {
            inner.animator.replace(None);
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    }

    fn draw(&self, cr: &cairo::Context, width: f64, height: f64) {
        let alpha = self.0.alpha.get();
        if alpha == 0.0 {
            return;
        }
        let display = self.0.display_state.get();
        let unit = 1f64.min(width / 900.0);
        let (r, g, b) = voice_color(display);
        let glow = cairo::LinearGradient::new(0.0, 0.0, 0.0, height);
        glow.add_color_stop_rgba(0.0, r, g, b, 0.0);
        glow.add_color_stop_rgba(1.0, r, g, b, 0.18 * alpha);
        let _ = cr.set_source(&glow);
        let _ = cr.paint();
        let now = crate::voice_state::monotonic();
        let thinking = display == "thinking";
        for index in 0..37 {
            let i = index as f64;
            let envelope = (-((i - 18.0) / 18.0).powi(2) * 2.0).exp();
            let wave = ((i * 0.55 + now * if thinking { 3.0 } else { 7.0 }).sin() + 1.0) / 2.0;
            let bar = (5.0 + envelope * (9.0 + wave * if thinking { 20.0 } else { 40.0 })) * unit;
            let (x, center) = (width / 2.0 + (i - 18.0) * 12.0 * unit, height * 0.6);
            cr.set_source_rgba(r, g, b, (0.42 + envelope * 0.58) * alpha);
            cr.set_line_width(5.0 * unit);
            cr.set_line_cap(cairo::LineCap::Round);
            cr.move_to(x, center - bar / 2.0);
            cr.line_to(x, center + bar / 2.0);
            let _ = cr.stroke();
        }
        cr.set_source_rgba(r, g, b, alpha);
        cr.rectangle(0.0, height - 3.0, width, 3.0);
        let _ = cr.fill();
    }

    pub fn close(&self) {
        if let Some(id) = self.0.animator.borrow_mut().take() {
            id.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_css_is_scoped_verbatim() {
        let css = family_css("tessel-lock");
        assert!(css.starts_with(".tessel-lock .family-clock { font-size: 108pt; font-weight: 600; color: white; text-shadow: 0 2px 8px #000; }"));
        assert!(css.contains(".tessel-lock.family-compact .family-clock { font-size: 78pt; }"));
        assert!(css.contains(".tessel-lock .family-avatar { min-width: 96px; min-height: 96px; border-radius: 9999px;"));
        assert!(css.ends_with(".tessel-lock popover row, .tessel-lock popover modelbutton { border-radius: 8px; }"));
        assert!(!css.contains("@SCOPE@"));
        assert_eq!(css.matches('{').count(), css.matches('}').count());
    }

    #[test]
    fn initials_match_python() {
        assert_eq!(initials("Thomas Crider"), "TC");
        assert_eq!(initials("john.q.public"), "JP");
        assert_eq!(initials("_admin_"), "A");
        assert_eq!(initials("  "), "");
        assert_eq!(initials("-x élan"), "É");
        assert_eq!(initials(""), "");
    }

    #[test]
    fn frame_clock_and_reversing_animation() {
        // test_lock_animation.test_frame_clock_and_reversing_animation (model part).
        let mut slide = Slide { progress: 0.0, start: 0.0, target: 0.0, started: None };
        slide.retarget(1.0);
        let mut now = 10_000_000;
        assert!(!slide.step(now));
        assert_eq!(slide.progress, 0.0);
        now += 175_000;
        slide.step(now);
        assert!((slide.progress - 0.875).abs() < 1e-9);
        let previous = slide.progress;
        slide.retarget(0.0);
        slide.step(now);
        assert_eq!(slide.progress, previous, "Reversing should not jump");
        now += 175_000;
        slide.step(now);
        assert!(slide.progress > 0.0 && slide.progress < previous);
        now += 175_000;
        assert!(slide.step(now));
        assert_eq!(slide.progress, 0.0);
    }

    #[test]
    fn placement_geometry_matches_pil() {
        assert_eq!(thumbnail_size(4000, 3000, 1920, 1080), Some((1440, 1080)));
        assert_eq!(thumbnail_size(100, 50, 1920, 1080), None);
        assert_eq!(thumbnail_size(3000, 1000, 1280, 720), Some((1280, 427)));
        assert_eq!(fit_crop(4000, 3000, 1920, 1080), (0.0, 375.0, 4000.0, 2250.0));
        let (left, top, w, h) = fit_crop(1000, 2000, 1920, 1080);
        assert_eq!((left, w), (0.0, 1000.0));
        assert!((h - 562.5).abs() < 1e-9 && (top - 718.75).abs() < 1e-9);
        assert_eq!(parse_color("#91b5d7"), (145, 181, 215));
        assert!((box_radius(20.0, 3) - 19.4875).abs() < 1e-6, "{}", box_radius(20.0, 3));
        assert_eq!(suffix(Path::new("/a/b.JPG")), ".jpg");
        assert_eq!(suffix(Path::new("/a/.png")), "");
    }

    #[test]
    fn blur_preserves_flat_colour_and_spreads_an_edge() {
        let flat = Rgb::filled(40, 30, (145, 181, 215));
        assert_eq!(gaussian_blur(&flat, 8.0), flat);
        let mut edge = Rgb::filled(64, 4, (0, 0, 0));
        edge.paste(&Rgb::filled(32, 4, (255, 255, 255)), 32, 0);
        let soft = gaussian_blur(&edge, 6.0);
        let at = |x: usize| soft.data[x * 3];
        assert!(at(0) < 5 && at(63) > 250);
        assert!(at(31) > 60 && at(31) < 128 && at(32) > 127 && at(32) < 196, "{} {}", at(31), at(32));
        let mut canvas = Rgb::filled(4, 4, (1, 2, 3));
        canvas.paste(&Rgb::filled(8, 8, (9, 9, 9)), -2, -2);
        assert_eq!(canvas, Rgb::filled(4, 4, (9, 9, 9)));
    }

    #[test]
    fn prepare_wallpaper_places_blurs_and_samples_off_thread() {
        let dir = std::env::temp_dir().join(format!("tessel-locking-wp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tall.png");
        let pixbuf = gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 100, 200).unwrap();
        pixbuf.fill(0x2050e0ff);
        pixbuf.savev(&path, "png", &[]).unwrap();
        let fill = std::thread::spawn({
            let path = path.clone();
            move || prepare_wallpaper(&path, 320, 180, "fill", 0.0, "#000000").map(|p| (p.sharp.width, p.sharp.height, p.blurred.is_none(), p.color))
        }).join().unwrap().unwrap();
        assert_eq!(fill, (320, 180, true, (0x20, 0x50, 0xe0)));
        let fit = prepare_wallpaper(&path, 320, 180, "fit", 10.0, "#ff0000").unwrap();
        assert_eq!((fit.sharp.width, fit.sharp.height), (320, 180));
        // Fit pastes a 90×180 picture centred on the background colour.
        assert_eq!(&fit.sharp.data[0..3], &[255, 0, 0]);
        assert_eq!(&fit.sharp.data[(160 * 3)..(160 * 3 + 3)], &[0x20, 0x50, 0xe0]);
        assert!(fit.blurred.is_some());
        let big = prepare_wallpaper(&path, 3840, 2160, "stretch", 0.0, "#000000").unwrap();
        assert_eq!((big.sharp.width, big.sharp.height), (2048, 1152));
        assert!(prepare_wallpaper(&dir.join("missing.png"), 10, 10, "fill", 0.0, "#000000").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
