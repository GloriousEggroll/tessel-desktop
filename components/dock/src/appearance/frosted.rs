//! Small static wallpaper textures for legacy Frosted backgrounds
//! (frosted_wallpaper.py). Tessel Dock itself keeps this disabled
//! (`set_frosted_enabled(False)`); the cache is ported for parity.
//!
//! `FrostedWallpaperCache::texture` decodes one bounded still (gdk-pixbuf),
//! fits it to the monitor like Tessel Shell (Fill/Fit/Stretch/Tile/Center),
//! applies Pillow's Gaussian blur, colour enhance (0.82) and cool tint blend
//! (exact ports of Pillow's integer pipelines), and stores a content-addressed
//! PNG (`sha256("tessel-dock-frosted-v3\0" + "(w, h)" + RGB bytes)`) in a bounded
//! cache (≤ 64 files / 16 MiB) that only prunes its own files.
//! Resampling uses gdk-pixbuf instead of Pillow's LANCZOS, so textures of
//! non-solid wallpapers are close to, not byte-identical with, Python's.

use super::fsx;
use gtk::gdk_pixbuf::{self, Pixbuf};
use std::collections::{HashSet, VecDeque};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use tessel_gtk::palette::Rgb;

pub const MAX_DIMENSION: u32 = 1024;
pub const MAX_PIXELS: u64 = 100_000_000;
pub const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_CACHE_FILES: usize = 64;
pub const MAX_CACHE_BYTES: u64 = 16 * 1024 * 1024;
const TEXTURE_VERSION: &[u8] = b"tessel-dock-frosted-v3\0";
pub const MODES: [&str; 5] = ["Fill", "Fit", "Stretch", "Tile", "Center"];
pub const DEFAULT_BACKING: Rgb = [112, 112, 112];

/// `$XDG_CACHE_HOME/tessel/dock/frosted` (only an absolute XDG_CACHE_HOME counts).
pub fn cache_directory() -> PathBuf {
    match std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from) {
        Some(value) if value.is_absolute() => value,
        _ => tessel_core::paths::home().join(".cache"),
    }.join(tessel_core::paths::SUITE).join("dock").join("frosted")
}

/// `(dev, ino, size, mtime_ns, ctime_ns, mode)`.
pub type SourceStamp = (u64, u64, u64, i64, i64, u32);

pub fn source_stamp(path: &Path) -> Result<SourceStamp, String> {
    let value = std::fs::metadata(path).map_err(|e| fsx::os_error_path(&e, path))?;
    if !value.is_file() {
        return Err("Wallpaper is not a regular file".into());
    }
    Ok((value.dev(), value.ino(), value.size(), value.mtime() * 1_000_000_000 + value.mtime_nsec(),
        value.ctime() * 1_000_000_000 + value.ctime_nsec(), value.mode()))
}

pub fn normalized_size(size: Option<(i64, i64)>) -> Option<(i64, i64)> {
    size.filter(|(w, h)| (1..=65536).contains(w) && (1..=65536).contains(h))
}

pub fn normalized_mode(mode: Option<&str>) -> &'static str {
    mode.and_then(|m| MODES.iter().find(|candidate| **candidate == m)).copied().unwrap_or("Fill")
}

pub fn normalized_color(color: Option<Rgb>) -> Rgb {
    color.unwrap_or(DEFAULT_BACKING)
}

/// An 8-bit RGBA image (Pillow "RGBA"; RGB images use alpha 255).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    /// Pillow `info["source_size"]`.
    pub source_size: Option<(i64, i64)>,
    /// Pillow `info["source_size_unknown"]` (video stills).
    pub source_size_unknown: bool,
}

impl Image {
    pub fn solid(width: u32, height: u32, color: [u8; 4]) -> Self {
        Self { width, height, data: color.repeat((width * height) as usize), source_size: None, source_size_unknown: false }
    }

    pub fn from_pixbuf(pixbuf: &Pixbuf) -> Self {
        let (width, height) = (pixbuf.width() as u32, pixbuf.height() as u32);
        let (channels, stride) = (pixbuf.n_channels() as usize, pixbuf.rowstride() as usize);
        let bytes = pixbuf.read_pixel_bytes();
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height as usize {
            for x in 0..width as usize {
                let offset = y * stride + x * channels;
                data.extend_from_slice(&bytes[offset..offset + 3]);
                data.push(if channels == 4 { bytes[offset + 3] } else { 255 });
            }
        }
        Self { width, height, data, source_size: None, source_size_unknown: false }
    }

    pub fn from_rgb(width: u32, height: u32, rgb: &[u8]) -> Self {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for pixel in rgb.chunks_exact(3) {
            data.extend_from_slice(pixel);
            data.push(255);
        }
        Self { width, height, data, source_size: None, source_size_unknown: false }
    }

    fn to_pixbuf(&self) -> Pixbuf {
        Pixbuf::from_bytes(&glib::Bytes::from(&self.data), gdk_pixbuf::Colorspace::Rgb, true, 8,
                           self.width as i32, self.height as i32, self.width as i32 * 4)
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * self.width + x) * 4) as usize;
        [self.data[offset], self.data[offset + 1], self.data[offset + 2], self.data[offset + 3]]
    }

    /// Pillow `tobytes()` of the RGB conversion.
    pub fn rgb_bytes(&self) -> Vec<u8> {
        self.data.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect()
    }

    /// Resize to exactly `width`×`height`; `crop` is an optional source box
    /// `(left, top, right, bottom)` in source pixels.
    pub fn resize(&self, width: u32, height: u32, crop: Option<(f64, f64, f64, f64)>) -> Image {
        let (left, top, right, bottom) = crop.unwrap_or((0.0, 0.0, self.width as f64, self.height as f64));
        if (width, height) == (self.width, self.height) && crop.is_none() {
            return self.clone();
        }
        let source = self.to_pixbuf();
        let Some(dest) = Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, true, 8, width as i32, height as i32) else { return self.clone() };
        let (scale_x, scale_y) = (width as f64 / (right - left), height as f64 / (bottom - top));
        source.scale(&dest, 0, 0, width as i32, height as i32, -left * scale_x, -top * scale_y, scale_x, scale_y,
                     gdk_pixbuf::InterpType::Hyper);
        let mut image = Image::from_pixbuf(&dest);
        image.source_size = self.source_size;
        image.source_size_unknown = self.source_size_unknown;
        image
    }

    /// Pillow `Image.thumbnail((size, size))`: aspect-preserving, never enlarges.
    pub fn thumbnail(&self, size: u32) -> Image {
        match thumbnail_size(self.width, self.height, size, size) {
            Some((w, h)) => self.resize(w, h, None),
            None => self.clone(),
        }
    }
}

/// Pillow's `thumbnail` target size (`preserve_aspect_ratio`), `None` if unchanged.
pub fn thumbnail_size(width: u32, height: u32, max_x: u32, max_y: u32) -> Option<(u32, u32)> {
    if max_x >= width && max_y >= height {
        return None;
    }
    let aspect = width as f64 / height as f64;
    let (x, y) = (max_x as f64, max_y as f64);
    let round_aspect = |number: f64, key: &dyn Fn(f64) -> f64| -> f64 {
        let (low, high) = (number.floor(), number.ceil());
        let best = if key(high) < key(low) { high } else { low };
        best.max(1.0)
    };
    if x / y >= aspect {
        let nx = round_aspect(y * aspect, &|n| (aspect - n / y).abs());
        Some((nx as u32, max_y))
    } else {
        let ny = round_aspect(x / aspect, &|n| if n == 0.0 { 0.0 } else { (aspect - x / n).abs() });
        Some((max_x, ny as u32))
    }
}

fn py_round(value: f64) -> i64 {
    fsx::py_round(value) as i64
}

/// Pillow `alpha_composite(layer, (x, y))` onto an opaque backing.
fn alpha_composite(base: &mut Image, layer: &Image, dx: i64, dy: i64) {
    for y in 0..layer.height as i64 {
        for x in 0..layer.width as i64 {
            let (bx, by) = (x + dx, y + dy);
            if bx < 0 || by < 0 || bx >= base.width as i64 || by >= base.height as i64 {
                continue;
            }
            let src = layer.pixel(x as u32, y as u32);
            let offset = ((by as u32 * base.width + bx as u32) * 4) as usize;
            if src[3] == 0 {
                continue;
            }
            let dst = [base.data[offset], base.data[offset + 1], base.data[offset + 2], base.data[offset + 3]];
            const PRECISION: u32 = 7;
            let shift = |a: u32| ((a >> 8) + a) >> 8;
            let (sa, da) = (src[3] as u32, dst[3] as u32);
            let blend = da * (255 - sa);
            let outa255 = sa * 255 + blend;
            let coef1 = sa * 255 * 255 * (1 << PRECISION) / outa255;
            let coef2 = 255 * (1 << PRECISION) - coef1;
            for c in 0..3 {
                let tmp = src[c] as u32 * coef1 + dst[c] as u32 * coef2;
                base.data[offset + c] = (shift(tmp + (0x80 << PRECISION)) >> PRECISION) as u8;
            }
            base.data[offset + 3] = shift(outa255 + 0x80) as u8;
        }
    }
}

/// `wallpaper_canvas`: opaque miniature of the whole monitor (Tessel Shell fitting).
pub fn wallpaper_canvas(image: &Image, size: Option<(i64, i64)>, mode: &str, color: Rgb) -> Image {
    let size = normalized_size(size);
    let (canvas, layer) = match size {
        None => ((image.width, image.height), image.clone()),
        Some((sw, sh)) => {
            let scale = (MAX_DIMENSION as f64 / sw.max(sh) as f64).min(1.0);
            let canvas = (py_round(sw as f64 * scale).max(1) as u32, py_round(sh as f64 * scale).max(1) as u32);
            let mode = normalized_mode(Some(mode));
            let layer = if mode == "Fill" {
                // ImageOps.fit: centred crop to the canvas aspect.
                let (w, h) = (image.width as f64, image.height as f64);
                let output_ratio = canvas.0 as f64 / canvas.1 as f64;
                let live_ratio = w / h;
                let (crop_w, crop_h) = if live_ratio == output_ratio { (w, h) }
                    else if live_ratio > output_ratio { (output_ratio * h, h) } else { (w, w / output_ratio) };
                let (left, top) = ((w - crop_w) * 0.5, (h - crop_h) * 0.5);
                image.resize(canvas.0, canvas.1, Some((left, top, left + crop_w, top + crop_h)))
            } else if mode == "Stretch" || mode == "Tile" {
                // Tessel Shell currently treats Tile as Stretch for its Gtk.Picture.
                image.resize(canvas.0, canvas.1, None)
            } else if mode == "Center" && !image.source_size_unknown {
                let original = normalized_size(image.source_size).unwrap_or((image.width as i64, image.height as i64));
                let downscale = 1f64.min(sw as f64 / original.0 as f64).min(sh as f64 / original.1 as f64);
                let target = (py_round(original.0 as f64 * downscale * scale).max(1) as u32,
                              py_round(original.1 as f64 * downscale * scale).max(1) as u32);
                image.resize(target.0, target.1, None)
            } else {
                // ImageOps.contain.
                let (iw, ih) = (image.width as f64, image.height as f64);
                let (im_ratio, dest_ratio) = (iw / ih, canvas.0 as f64 / canvas.1 as f64);
                let mut target = canvas;
                if im_ratio != dest_ratio {
                    if im_ratio > dest_ratio {
                        let height = py_round(ih / iw * canvas.0 as f64) as u32;
                        if height != canvas.1 {
                            target = (canvas.0, height);
                        }
                    } else {
                        let width = py_round(iw / ih * canvas.1 as f64) as u32;
                        if width != canvas.0 {
                            target = (width, canvas.1);
                        }
                    }
                }
                image.resize(target.0, target.1, None)
            };
            (canvas, layer)
        }
    };
    let mut backing = Image::solid(canvas.0, canvas.1, [color[0], color[1], color[2], 255]);
    alpha_composite(&mut backing, &layer, (canvas.0 as i64 - layer.width as i64).div_euclid(2),
                    (canvas.1 as i64 - layer.height as i64).div_euclid(2));
    for pixel in backing.data.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    backing
}

/// Pillow `_gaussian_blur_radius` (float arithmetic).
fn gaussian_blur_radius(radius: f32, passes: i32) -> f32 {
    let sigma2 = radius * radius / passes as f32;
    let length = (12.0f64 * sigma2 as f64 + 1.0).sqrt() as f32;
    let l = ((length as f64 - 1.0) / 2.0).floor() as f32;
    let mut a = (2.0 * l + 1.0) * (l * (l + 1.0) - 3.0 * sigma2);
    a /= 6.0 * (sigma2 - (l + 1.0) * (l + 1.0));
    l + a
}

fn line_box_blur(out: &mut [[u8; 4]], line: &[[u8; 4]], radius: i32, edge_a: i32, edge_b: i32, ww: u32, fw: u32) {
    let lastx = line.len() as i32 - 1;
    let mut acc = [0u32; 4];
    for c in 0..4 {
        acc[c] = (line[0][c] as u32).wrapping_mul((radius + 1) as u32);
    }
    for x in 0..(edge_a - 1).max(0) {
        for c in 0..4 {
            acc[c] = acc[c].wrapping_add(line[x as usize][c] as u32);
        }
    }
    for c in 0..4 {
        acc[c] = acc[c].wrapping_add((line[lastx as usize][c] as u32).wrapping_mul((radius - edge_a + 1) as u32));
    }
    let mut step = |x: i32, subtract: i32, add: i32, left: i32, right: i32, acc: &mut [u32; 4]| {
        for c in 0..4 {
            acc[c] = acc[c].wrapping_add(line[add as usize][c] as u32).wrapping_sub(line[subtract as usize][c] as u32);
            let bulk = acc[c].wrapping_mul(ww).wrapping_add((line[left as usize][c] as u32 + line[right as usize][c] as u32).wrapping_mul(fw));
            out[x as usize][c] = (bulk.wrapping_add(1 << 23) >> 24) as u8;
        }
    };
    if edge_a <= edge_b {
        for x in 0..edge_a {
            step(x, 0, x + radius, 0, x + radius + 1, &mut acc);
        }
        for x in edge_a..edge_b {
            step(x, x - radius - 1, x + radius, x - radius - 1, x + radius + 1, &mut acc);
        }
        for x in edge_b..=lastx {
            step(x, x - radius - 1, lastx, x - radius - 1, lastx, &mut acc);
        }
    } else {
        for x in 0..edge_b {
            step(x, 0, x + radius, 0, x + radius + 1, &mut acc);
        }
        for x in edge_b..edge_a {
            step(x, 0, lastx, 0, lastx, &mut acc);
        }
        for x in edge_a..=lastx {
            step(x, x - radius - 1, lastx, x - radius - 1, lastx, &mut acc);
        }
    }
}

fn horizontal_box_blur(pixels: &mut [[u8; 4]], width: usize, height: usize, float_radius: f32) {
    let radius = float_radius as i32;
    let ww = ((1u32 << 24) as f32 / (float_radius * 2.0 + 1.0)) as u32;
    let fw = ((1u32 << 24).wrapping_sub(((radius * 2 + 1) as u32).wrapping_mul(ww))) / 2;
    let edge_a = (radius + 1).min(width as i32);
    let edge_b = (width as i32 - radius - 1).max(0);
    let mut line = vec![[0u8; 4]; width];
    for y in 0..height {
        let row = &mut pixels[y * width..(y + 1) * width];
        line.copy_from_slice(row);
        line_box_blur(row, &line, radius, edge_a, edge_b, ww, fw);
    }
}

fn transpose(pixels: &[[u8; 4]], width: usize, height: usize) -> Vec<[u8; 4]> {
    let mut out = vec![[0u8; 4]; pixels.len()];
    for y in 0..height {
        for x in 0..width {
            out[x * height + y] = pixels[y * width + x];
        }
    }
    out
}

/// Pillow `ImageFilter.GaussianBlur(radius)` on an RGB(X) image.
pub fn gaussian_blur(image: &Image, radius: f32) -> Image {
    let (width, height) = (image.width as usize, image.height as usize);
    let box_radius = gaussian_blur_radius(radius, 3);
    let mut pixels: Vec<[u8; 4]> = image.data.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect();
    if box_radius != 0.0 {
        for _ in 0..3 {
            horizontal_box_blur(&mut pixels, width, height, box_radius);
        }
        let mut transposed = transpose(&pixels, width, height);
        for _ in 0..3 {
            horizontal_box_blur(&mut transposed, height, width, box_radius);
        }
        pixels = transpose(&transposed, height, width);
    }
    Image { data: pixels.concat(), ..image.clone() }
}

/// Pillow `Image.blend(first, second, alpha)` (float, truncating, clipped).
pub fn blend(first: &Image, second: &Image, alpha: f32) -> Image {
    let data = first.data.iter().zip(&second.data).map(|(a, b)| {
        let temp = *a as i32 as f32 + alpha * (*b as i32 - *a as i32) as f32;
        if temp <= 0.0 { 0 } else if temp >= 255.0 { 255 } else { temp as u8 }
    }).collect();
    Image { data, ..first.clone() }
}

/// Pillow `ImageEnhance.Color(image).enhance(factor)`.
pub fn enhance_color(image: &Image, factor: f32) -> Image {
    let mut grey = image.clone();
    for pixel in grey.data.chunks_exact_mut(4) {
        let l = ((pixel[0] as u32 * 19595 + pixel[1] as u32 * 38470 + pixel[2] as u32 * 7471 + 0x8000) >> 16) as u8;
        pixel[0] = l;
        pixel[1] = l;
        pixel[2] = l;
    }
    blend(&grey, image, factor)
}

/// Blur, desaturate slightly and tint: the idle-screen glass treatment.
pub fn frost(canvas: &Image) -> Image {
    let rendered = gaussian_blur(canvas, (10.0 * canvas.width.max(canvas.height) as f64 / MAX_DIMENSION as f64) as f32);
    let rendered = enhance_color(&rendered, 0.82);
    let tint = Image::solid(rendered.width, rendered.height, [193, 206, 213, 255]);
    let mut out = blend(&rendered, &tint, 0.09);
    for pixel in out.data.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    out
}

/// `load_image`: bounded decode of the first frame, EXIF-oriented.
pub fn load_image(path: &Path, size: u32) -> Result<Image, String> {
    if source_stamp(path)?.2 > MAX_IMAGE_BYTES {
        return Err("Wallpaper image is too large".into());
    }
    let (_format, width, height) = Pixbuf::file_info(path).ok_or_else(|| format!("cannot identify image file '{}'", path.display()))?;
    if width <= 0 || height <= 0 || width as u64 * height as u64 > MAX_PIXELS {
        return Err("Wallpaper image dimensions are too large".into());
    }
    let pixbuf = Pixbuf::from_file(path).map_err(|e| tessel_core::pyerr::python_error_text(&e))?;
    let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
    let mut image = Image::from_pixbuf(&pixbuf).thumbnail(size);
    image.source_size = Some((width as i64, height as i64));
    Ok(image)
}

/// `(path, stamp, size, mode, color)`.
type Key = (PathBuf, SourceStamp, Option<(i64, i64)>, &'static str, Rgb);
pub type Loader = Box<dyn Fn(&Path, u32) -> Result<Image, String> + Send>;

/// The texture cache seen by the wallpaper reader (tests use fakes).
pub trait FrostCache: Send {
    fn texture(&mut self, path: &Path, size: Option<(i64, i64)>, mode: &str, color: Rgb) -> Result<String, String>;
    fn protect(&mut self, paths: &HashSet<String>);
    fn directory(&self) -> Option<PathBuf> {
        None
    }
}

pub struct FrostedWallpaperCache {
    pub directory: PathBuf,
    pub loader: Loader,
    pub max_files: usize,
    pub max_bytes: u64,
    entries: VecDeque<(Key, PathBuf)>,
    protected: HashSet<PathBuf>,
    /// Counts `texture` calls (tests mirror Python's `mock.wraps`).
    pub calls: usize,
}

impl FrostedWallpaperCache {
    pub fn new(directory: Option<PathBuf>, loader: Option<Loader>, max_files: usize, max_bytes: u64) -> Self {
        let directory = directory.unwrap_or_else(cache_directory);
        let directory = if directory.is_absolute() { directory } else {
            std::env::current_dir().map(|c| c.join(&directory)).unwrap_or(directory)
        };
        Self { directory, loader: loader.unwrap_or_else(|| Box::new(load_image)), max_files: max_files.max(1),
               max_bytes: max_bytes.max(1), entries: VecDeque::new(), protected: HashSet::new(), calls: 0 }
    }

    pub fn with_directory(directory: PathBuf) -> Self {
        Self::new(Some(directory), None, MAX_CACHE_FILES, MAX_CACHE_BYTES)
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    fn render(&self, path: &Path, size: Option<(i64, i64)>, mode: &str, color: Rgb) -> Result<(String, Vec<u8>), String> {
        let image = (self.loader)(path, MAX_DIMENSION)?;
        if image.width == 0 || image.height == 0 || image.width as u64 * image.height as u64 > MAX_PIXELS {
            return Err("Wallpaper image dimensions are too large".into());
        }
        let image = { let mut thumb = image.thumbnail(MAX_DIMENSION); thumb.source_size = image.source_size;
                      thumb.source_size_unknown = image.source_size_unknown; thumb };
        let rendered = frost(&wallpaper_canvas(&image, size, mode, color));
        let rgb = rendered.rgb_bytes();
        let mut hashed = TEXTURE_VERSION.to_vec();
        hashed.extend_from_slice(format!("({}, {})", rendered.width, rendered.height).as_bytes());
        hashed.extend_from_slice(&rgb);
        let digest = glib::compute_checksum_for_data(glib::ChecksumType::Sha256, &hashed).map(|d| d.to_string()).unwrap_or_default();
        let pixbuf = Pixbuf::from_bytes(&glib::Bytes::from(&rgb), gdk_pixbuf::Colorspace::Rgb, false, 8,
                                        rendered.width as i32, rendered.height as i32, rendered.width as i32 * 3);
        let data = pixbuf.save_to_bufferv("png", &[("compression", "6")]).map_err(|e| tessel_core::pyerr::python_error_text(&e))?;
        Ok((digest, data))
    }

    /// Only remove our own content-addressed PNGs, oldest first.
    fn prune(&self, keep: &Path) -> bool {
        let mut files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some(stem) = name.strip_suffix(".png") else { continue };
                if stem.len() != 64 || !stem.chars().all(|c| "0123456789abcdef".contains(c)) {
                    continue;
                }
                if let Ok(value) = path.symlink_metadata() {
                    if value.is_file() {
                        files.push((value.mtime() * 1_000_000_000 + value.mtime_nsec(), value.size(), path));
                    }
                }
            }
        }
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        let mut remaining = files.len();
        files.sort();
        for (_mtime, size, path) in files {
            if remaining <= self.max_files && total <= self.max_bytes {
                break;
            }
            if path == keep || self.protected.contains(&path) {
                continue;
            }
            if std::fs::remove_file(&path).is_err() {
                continue;
            }
            remaining -= 1;
            total -= size;
        }
        remaining <= self.max_files && total <= self.max_bytes
    }
}

impl FrostCache for FrostedWallpaperCache {
    fn protect(&mut self, paths: &HashSet<String>) {
        self.protected = paths.iter().map(PathBuf::from).collect();
    }

    fn directory(&self) -> Option<PathBuf> {
        Some(self.directory.clone())
    }

    fn texture(&mut self, path: &Path, size: Option<(i64, i64)>, mode: &str, color: Rgb) -> Result<String, String> {
        self.calls += 1;
        let stamp = source_stamp(path)?;
        let key: Key = (path.to_owned(), stamp, normalized_size(size), normalized_mode(Some(mode)), color);
        if let Some(index) = self.entries.iter().position(|(k, _)| *k == key) {
            if self.entries[index].1.is_file() {
                let entry = self.entries.remove(index).unwrap();
                let result = entry.1.to_string_lossy().into_owned();
                self.entries.push_back(entry);
                return Ok(result);
            }
        }
        let (digest, data) = self.render(path, key.2, key.3, color)?;
        if source_stamp(path)? != stamp {
            return Err("Wallpaper changed while preparing Frosted background".into());
        }
        if data.len() as u64 > self.max_bytes {
            return Err("Frosted wallpaper exceeds the texture cache limit".into());
        }
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.directory)
            .map_err(|e| fsx::os_error_path(&e, &self.directory))?;
        let destination = self.directory.join(format!("{digest}.png"));
        if !destination.is_file() {
            let (mut file, temporary) = fsx::mkstemp(&self.directory, ".frosted-", ".tmp").map_err(|e| fsx::error_text(&e))?;
            let result = file.write_all(&data).map_err(|e| fsx::os_error_path(&e, &temporary))
                .and_then(|()| fsx::replace(&temporary, &destination).map_err(|e| fsx::error_text(&e)));
            let _ = std::fs::remove_file(&temporary);
            result?;
        } else {
            let file = std::fs::OpenOptions::new().append(true).open(&destination).map_err(|e| fsx::os_error_path(&e, &destination))?;
            let now = std::time::SystemTime::now();
            file.set_times(std::fs::FileTimes::new().set_accessed(now).set_modified(now)).map_err(|e| fsx::os_error_path(&e, &destination))?;
        }
        self.entries.retain(|(k, _)| *k != key);
        self.entries.push_back((key.clone(), destination.clone()));
        while self.entries.len() > self.max_files {
            self.entries.pop_front();
        }
        if !self.prune(&destination) {
            // Do not evict another active monitor's background to make room.
            if !self.protected.contains(&destination) {
                let _ = std::fs::remove_file(&destination);
            }
            self.entries.retain(|(k, _)| *k != key);
            return Err("Frosted wallpaper cache is full".into());
        }
        Ok(destination.to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("d-frost-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn save(path: &Path, image: &Image) {
        image.to_pixbuf().savev(path, "png", &[]).unwrap();
    }

    fn wallpaper(root: &Path, name: &str, color: [u8; 4]) -> PathBuf {
        let path = root.join(name);
        save(&path, &Image::solid(1200, 800, color));
        path
    }

    fn open(path: &str) -> Image {
        Image::from_pixbuf(&Pixbuf::from_file(path).unwrap())
    }

    fn rgb(p: [u8; 4]) -> [u8; 3] {
        [p[0], p[1], p[2]]
    }

    fn counting(root: &Path) -> (FrostedWallpaperCache, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = counter.clone();
        let loader: Loader = Box::new(move |path, size| {
            assert_eq!(size, MAX_DIMENSION);
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            load_image(path, size)
        });
        (FrostedWallpaperCache::new(Some(root.join("cache")), Some(loader), MAX_CACHE_FILES, MAX_CACHE_BYTES), counter)
    }

    fn pngs(directory: &Path) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(directory).map(|e| e.flatten().map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "png")).collect()).unwrap_or_default();
        files.sort();
        files
    }

    #[test]
    fn repeated_source_uses_same_cached_file_without_decoding() {
        let root = root("repeat");
        let path = wallpaper(&root, "wall.png", [120, 20, 200, 255]);
        let (mut cache, loads) = counting(&root);
        let first = cache.texture(&path, None, "Fill", DEFAULT_BACKING).unwrap();
        assert_eq!(cache.texture(&path, None, "Fill", DEFAULT_BACKING).unwrap(), first);
        assert_eq!(loads.load(std::sync::atomic::Ordering::SeqCst), 1);
        let first = PathBuf::from(first);
        assert!(first.is_absolute());
        assert_eq!(first.file_stem().unwrap().len(), 64);
        assert_eq!(pngs(&root.join("cache")), [first]);
    }

    #[test]
    fn texture_preserves_wallpaper_colors_with_soft_edges_and_cool_glass_tint() {
        let root = root("panels");
        let path = root.join("panels.png");
        let mut image = Image::solid(2048, 1024, [255, 0, 0, 255]);
        for y in 0..1024 {
            for x in 1024..2048 {
                let o = ((y * 2048 + x) * 4) as usize;
                image.data[o..o + 4].copy_from_slice(&[0, 0, 255, 255]);
            }
        }
        save(&path, &image);
        let result = open(&FrostedWallpaperCache::with_directory(root.join("cache")).texture(&path, None, "Fill", DEFAULT_BACKING).unwrap());
        assert_eq!((result.width, result.height), (1024, 512));
        let (left, right) = (rgb(result.pixel(256, 256)), rgb(result.pixel(768, 256)));
        assert_eq!(left, [219, 30, 31]);
        assert_eq!(right, [21, 23, 213]);
        assert_eq!(rgb(result.pixel(448, 256)), left);
        assert_eq!(rgb(result.pixel(576, 256)), right);
        let transition: Vec<[u8; 3]> = (500..524).map(|x| rgb(result.pixel(x, 256))).collect();
        assert!(transition.iter().all(|p| left[0] > p[0] && p[0] > right[0]), "{transition:?}");
        assert!(transition.windows(2).map(|w| (w[0][0] as i32 - w[1][0] as i32).abs()).max().unwrap() < 10);
        let transparent = wallpaper(&root, "transparent.png", [255, 0, 0, 0]);
        let result = open(&FrostedWallpaperCache::with_directory(root.join("cache")).texture(&transparent, None, "Fill", DEFAULT_BACKING).unwrap());
        assert_eq!(rgb(result.pixel(0, 0)), [119, 120, 121]);
    }

    #[test]
    fn blur_scales_with_small_wallpapers_and_never_enlarges_source() {
        let root = root("small");
        let mut samples = Vec::new();
        for width in [1024u32, 512] {
            let path = root.join(format!("edge-{width}.png"));
            let mut image = Image::solid(width, width / 2, [255, 0, 0, 255]);
            for y in 0..width / 2 {
                for x in width / 2..width {
                    let o = ((y * width + x) * 4) as usize;
                    image.data[o..o + 4].copy_from_slice(&[0, 0, 255, 255]);
                }
            }
            save(&path, &image);
            let result = open(&FrostedWallpaperCache::with_directory(root.join("cache")).texture(&path, None, "Fill", DEFAULT_BACKING).unwrap());
            assert_eq!((result.width, result.height), (width, width / 2));
            samples.push([0.48, 0.49, 0.5, 0.51, 0.52].map(|f: f64| rgb(result.pixel(fsx::py_round(f * width as f64) as u32, result.height / 2))));
        }
        for (large, small) in samples[0].iter().zip(&samples[1]) {
            assert!((0..3).map(|i| (large[i] as i32 - small[i] as i32).abs()).max().unwrap() <= 8, "{large:?} {small:?}");
        }
    }

    #[test]
    fn texture_size_is_bounded_for_landscape_portrait_and_square_wallpapers() {
        let root = root("bounded");
        for (w, h) in [(3072u32, 768u32), (768, 3072), (2048, 2048)] {
            let path = root.join(format!("{w}x{h}.png"));
            save(&path, &Image::solid(w, h, [80, 100, 120, 255]));
            let image = open(&FrostedWallpaperCache::with_directory(root.join("cache")).texture(&path, None, "Fill", DEFAULT_BACKING).unwrap());
            assert_eq!(image.width.max(image.height), MAX_DIMENSION);
            assert_eq!(image.width as f64 / image.height as f64, w as f64 / h as f64);
        }
    }

    #[test]
    fn replaced_wallpaper_regenerates_and_identical_content_shares_cache() {
        let root = root("replaced");
        let path = wallpaper(&root, "wall.png", [120, 20, 200, 255]);
        let (mut cache, loads) = counting(&root);
        let original = cache.texture(&path, None, "Fill", DEFAULT_BACKING).unwrap();
        std::fs::rename(wallpaper(&root, "replacement.png", [20, 150, 60, 255]), &path).unwrap();
        let changed = cache.texture(&path, None, "Fill", DEFAULT_BACKING).unwrap();
        assert_ne!(original, changed);
        let duplicate = wallpaper(&root, "duplicate.png", [20, 150, 60, 255]);
        assert_eq!(cache.texture(&duplicate, None, "Fill", DEFAULT_BACKING).unwrap(), changed);
        assert_eq!(loads.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert_eq!(pngs(&root.join("cache")).len(), 2);
    }

    #[test]
    fn disk_and_memory_cache_are_bounded_and_leave_other_files_alone() {
        let root = root("limits");
        let directory = root.join("cache");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("leave-me.png"), "unrelated").unwrap();
        let mut cache = FrostedWallpaperCache::new(Some(directory.clone()), None, 2, 5000);
        for number in 0..6u8 {
            let _ = cache.texture(&wallpaper(&root, &format!("wall-{number}.png"), [number * 40, 30, 50, 255]), None, "Fill", DEFAULT_BACKING);
        }
        let artifacts: Vec<PathBuf> = pngs(&directory).into_iter().filter(|p| p.file_stem().unwrap().len() == 64).collect();
        assert!(artifacts.len() <= 2);
        assert!(artifacts.iter().map(|p| std::fs::metadata(p).unwrap().len()).sum::<u64>() <= 5000);
        assert!(cache.entry_count() <= 2);
        assert_eq!(std::fs::read_to_string(directory.join("leave-me.png")).unwrap(), "unrelated");
        assert!(!std::fs::read_dir(&directory).unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".tmp")));
    }

    #[test]
    fn missing_invalid_or_replaced_during_load_creates_no_texture() {
        let root = root("invalid");
        let mut cache = FrostedWallpaperCache::with_directory(root.join("cache"));
        assert!(cache.texture(&root.join("missing.png"), None, "Fill", DEFAULT_BACKING).unwrap_err().contains("No such file"));
        std::fs::write(root.join("invalid.png"), "not a picture").unwrap();
        assert!(cache.texture(&root.join("invalid.png"), None, "Fill", DEFAULT_BACKING).is_err());
        let path = wallpaper(&root, "wall.png", [120, 20, 200, 255]);
        let replacement_root = root.clone();
        let loader: Loader = Box::new(move |source, size| {
            let image = load_image(source, size);
            std::fs::rename(wallpaper(&replacement_root, "replacement.png", [0, 255, 0, 255]), source).unwrap();
            image
        });
        let mut cache = FrostedWallpaperCache::new(Some(root.join("cache")), Some(loader), MAX_CACHE_FILES, MAX_CACHE_BYTES);
        assert!(cache.texture(&path, None, "Fill", DEFAULT_BACKING).unwrap_err().contains("changed while"));
        assert!(!root.join("cache").exists());
    }

    #[test]
    fn pruning_retains_current_monitor_and_keeps_storage_bounded() {
        let root = root("prune");
        let mut cache = FrostedWallpaperCache::new(Some(root.join("cache")), None, 2, MAX_CACHE_BYTES);
        let selected = cache.texture(&wallpaper(&root, "selected.png", [120, 20, 200, 255]), None, "Fill", DEFAULT_BACKING).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let stale = cache.texture(&wallpaper(&root, "stale.png", [20, 150, 60, 255]), None, "Fill", DEFAULT_BACKING).unwrap();
        cache.protect(&[selected.clone()].into_iter().collect());
        std::thread::sleep(std::time::Duration::from_millis(10));
        let new = cache.texture(&wallpaper(&root, "new.png", [200, 100, 60, 255]), None, "Fill", DEFAULT_BACKING).unwrap();
        assert!(Path::new(&selected).is_file());
        assert!(!Path::new(&stale).exists());
        assert!(Path::new(&new).is_file());
        cache.protect(&[selected.clone(), new.clone()].into_iter().collect());
        let error = cache.texture(&wallpaper(&root, "too-many.png", [20, 100, 190, 255]), None, "Fill", DEFAULT_BACKING).unwrap_err();
        assert!(error.contains("cache is full"));
        assert_eq!(pngs(&root.join("cache")).len(), 2);
        assert!(Path::new(&selected).is_file() && Path::new(&new).is_file());
    }

    #[test]
    fn monitor_canvas_obeys_fit_fill_stretch_and_center() {
        let mut image = Image::solid(300, 100, [255, 0, 0, 255]);
        image.source_size = Some((300, 100));
        for mode in ["Fill", "Stretch", "Tile"] {
            let canvas = wallpaper_canvas(&image, Some((1000, 1000)), mode, [0, 0, 255]);
            assert_eq!((canvas.width, canvas.height), (1000, 1000));
            assert_eq!(rgb(canvas.pixel(0, 0)), [255, 0, 0], "{mode}");
        }
        let fit = wallpaper_canvas(&image, Some((1000, 1000)), "Fit", [0, 0, 255]);
        assert_eq!(rgb(fit.pixel(0, 0)), [0, 0, 255]);
        assert_eq!(rgb(fit.pixel(0, 499)), [255, 0, 0]);
        let center = wallpaper_canvas(&image, Some((1000, 1000)), "Center", [0, 0, 255]);
        assert_eq!(rgb(center.pixel(0, 499)), [0, 0, 255]);
        assert_eq!(rgb(center.pixel(499, 499)), [255, 0, 0]);
        assert_eq!(rgb(center.pixel(349, 499)), [0, 0, 255]);
        assert_eq!(rgb(center.pixel(350, 499)), [255, 0, 0]);
    }

    #[test]
    fn monitor_aspect_and_modes_are_part_of_cached_texture_identity() {
        let root = root("aspect");
        let path = wallpaper(&root, "wall.png", [120, 20, 200, 255]);
        let (mut cache, loads) = counting(&root);
        let landscape = cache.texture(&path, Some((1920, 1080)), "Fill", DEFAULT_BACKING).unwrap();
        let portrait = cache.texture(&path, Some((1080, 1920)), "Fill", DEFAULT_BACKING).unwrap();
        let fit = cache.texture(&path, Some((1920, 1080)), "Fit", [0, 0, 0]).unwrap();
        assert_ne!(landscape, portrait);
        assert_ne!(landscape, fit);
        let size = |p: &str| { let i = open(p); (i.width, i.height) };
        assert_eq!(size(&landscape), (1024, 576));
        assert_eq!(size(&portrait), (576, 1024));
        assert_eq!(cache.texture(&path, Some((1920, 1080)), "Fill", DEFAULT_BACKING).unwrap(), landscape);
        assert_eq!(loads.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn thumbnail_size_matches_pillow() {
        assert_eq!(thumbnail_size(1200, 800, 1024, 1024), Some((1024, 683)));
        assert_eq!(thumbnail_size(800, 1200, 1024, 1024), Some((683, 1024)));
        assert_eq!(thumbnail_size(300, 100, 1024, 1024), None);
        assert_eq!(thumbnail_size(128, 64, 64, 64), Some((64, 32)));
    }
}
