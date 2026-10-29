//! Port of picture_model.py: image documents, bounded disk-backed undo and atomic image saves.
//! Pillow is replaced by the `image` crate (codecs, Lanczos resampling), kamadak-exif
//! (EXIF read/write), LittleCMS through dlopen (embedded profile → sRGB), cairo
//! (annotation masks) and GdkPixbuf for the optional system decoders (SVG, AVIF, …).
//! Pixel operations reproduce Pillow's integer formulas.

use crate::compat::lcms;
use crate::exif_data::ExifData;
use gtk::cairo;
use image::{DynamicImage, ImageDecoder, ImageFormat, Rgba, RgbaImage};
use std::fs;
use std::io::{self, BufReader, Read};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use suite_core::tr;

/// `IMAGE_EXTENSIONS`: Pillow 12's registered extensions + the GdkPixbuf extras.
pub const IMAGE_EXTENSIONS: &[&str] = &[
    ".apng", ".avif", ".blp", ".bmp", ".bufr", ".bw", ".cur", ".dcx", ".dds", ".dib", ".emf", ".eps", ".fit", ".fits", ".flc",
    ".fli", ".fpx", ".ftc", ".ftu", ".gbr", ".gif", ".grib", ".h5", ".hdf", ".heic", ".heif", ".icb", ".icns", ".ico", ".iim",
    ".im", ".j2c", ".j2k", ".jfif", ".jp2", ".jpc", ".jpe", ".jpeg", ".jpf", ".jpg", ".jpx", ".jxl", ".mic", ".mpeg", ".mpg",
    ".mpo", ".msp", ".palm", ".pbm", ".pcd", ".pcx", ".pdf", ".pfm", ".pgm", ".png", ".pnm", ".ppm", ".ps", ".psd", ".pxr",
    ".qoi", ".ras", ".rgb", ".rgba", ".sgi", ".svg", ".svgz", ".tga", ".tif", ".tiff", ".vda", ".vst", ".webp", ".wmf", ".xbm", ".xpm",
];
pub const VIDEO_EXTENSIONS: [&str; 7] = [".mp4", ".m4v", ".webm", ".mkv", ".mov", ".avi", ".ogv"];
pub const SAVE_FORMATS: [(&str, &str); 8] = [(".png", "PNG"), (".jpg", "JPEG"), (".jpeg", "JPEG"), (".webp", "WEBP"),
                                            (".tif", "TIFF"), (".tiff", "TIFF"), (".bmp", "BMP"), (".gif", "GIF")];
pub const MAX_PIXELS: u64 = 80_000_000;
pub const MAX_HISTORY: usize = 20;
pub const MAX_HISTORY_BYTES: u64 = 512 * 1024 * 1024;

/// `Path(name).suffix.lower()`.
pub fn suffix(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let trimmed = name.trim_start_matches('.');
    match trimmed.rfind('.') {
        Some(index) if index + 1 < trimmed.len() => trimmed[index..].to_lowercase(),
        _ => String::new(),
    }
}

pub fn is_image(path: &Path) -> bool {
    IMAGE_EXTENSIONS.contains(&suffix(path).as_str())
}
pub fn is_video(path: &Path) -> bool { VIDEO_EXTENSIONS.contains(&suffix(path).as_str()) }
pub fn save_format(path: &Path) -> Option<&'static str> {
    let ext = suffix(path);
    SAVE_FORMATS.iter().find(|(e, _)| *e == ext).map(|(_, f)| *f)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyPart {
    Text(String),
    Number(Vec<u8>),
}

/// `natural_key`: `re.split(r'(\d+)')` → `(0, text.casefold())` / `(1, int(digits))` parts.
pub fn natural_key(name: &str) -> Vec<KeyPart> {
    let chars: Vec<char> = name.chars().collect();
    let mut parts = Vec::new();
    let mut i = 0;
    loop {
        let start = i;
        while i < chars.len() && !chars[i].is_ascii_digit() {
            i += 1;
        }
        parts.push(KeyPart::Text(suite_core::pyjson::casefold(&chars[start..i].iter().collect::<String>())));
        if i >= chars.len() {
            return parts;
        }
        let start = i;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        let digits: String = chars[start..i].iter().collect();
        let trimmed = digits.trim_start_matches('0');
        // Integers compare by length first, then digit by digit.
        let mut key = (trimmed.len() as u32).to_be_bytes().to_vec();
        key.extend(trimmed.bytes());
        parts.push(KeyPart::Number(key));
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    pub folder: bool,
    pub size: u64,
    pub modified: f64,
}

/// `scan_folder(folder, hidden=, order='name'|'date'|'size', reverse=)`: folders first.
pub fn scan_folder(folder: &Path, hidden: bool, order: &str, reverse: bool) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for item in fs::read_dir(folder)? {
        let Ok(item) = item else { continue };
        let name = item.file_name().to_string_lossy().into_owned();
        if !hidden && name.starts_with('.') {
            continue;
        }
        let path = item.path();
        let Ok(details) = fs::metadata(&path) else { continue };
        let folder_ = details.is_dir();
        if !folder_ && !(is_image(&path) || is_video(&path)) {
            continue;
        }
        let modified = details.mtime() as f64 + details.mtime_nsec() as f64 / 1e9;
        entries.push(Entry { path, folder: folder_, size: details.len(), modified });
    }
    let name_key = |e: &Entry| natural_key(&e.path.file_name().unwrap_or_default().to_string_lossy());
    let compare = |a: &Entry, b: &Entry| match order {
        "date" => a.modified.partial_cmp(&b.modified).unwrap_or(std::cmp::Ordering::Equal),
        "size" => a.size.cmp(&b.size),
        _ => name_key(a).cmp(&name_key(b)),
    };
    if reverse {
        entries.sort_by(|a, b| compare(b, a));
    } else {
        entries.sort_by(compare);
    }
    entries.sort_by_key(|e| !e.folder);
    Ok(entries)
}

pub type Signature = (u64, u64, u64, i128);

pub fn signature(path: &Path) -> io::Result<Signature> {
    let details = fs::metadata(path)?;
    Ok((details.dev(), details.ino(), details.size(), details.mtime() as i128 * 1_000_000_000 + details.mtime_nsec() as i128))
}

/// Errors carry Python's exception class (for callers that branch on it) and `str(exc)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PictureError {
    Value(String),
    FileExists(String),
    Runtime(String),
    Os(String),
}

impl std::fmt::Display for PictureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PictureError::Value(m) | PictureError::FileExists(m) | PictureError::Runtime(m) | PictureError::Os(m) => f.write_str(m),
        }
    }
}

pub type Result<T> = std::result::Result<T, PictureError>;

fn os(error: &io::Error, path: &Path) -> PictureError { PictureError::Os(suite_core::pyerr::os_error_path(error, path)) }

pub fn checked_size(width: i64, height: i64) -> Result<(u32, u32)> {
    if width < 1 || height < 1 || (width as i128) * (height as i128) > MAX_PIXELS as i128 {
        return Err(PictureError::Value(tr!("Image size must be positive and at most 80 megapixels.")));
    }
    Ok((width as u32, height as u32))
}

pub type LinkFn = fn(&Path, &Path) -> io::Result<()>;

fn hard_link(from: &Path, to: &Path) -> io::Result<()> { fs::hard_link(from, to) }

/// `publish_new`: publish without overwriting, also on filesystems without hard links.
pub fn publish_new(temporary: &Path, destination: &Path, link: LinkFn) -> Result<()> {
    match link(temporary, destination) {
        Ok(()) => {
            let _ = fs::remove_file(temporary);
            Ok(())
        }
        Err(error) if matches!(error.raw_os_error(), Some(libc::EPERM | libc::EOPNOTSUPP | libc::ENOSYS | libc::EXDEV)) => {
            use std::os::unix::ffi::OsStrExt;
            let from = std::ffi::CString::new(temporary.as_os_str().as_bytes()).unwrap();
            let to = std::ffi::CString::new(destination.as_os_str().as_bytes()).unwrap();
            let result = unsafe { libc::renameat2(libc::AT_FDCWD, from.as_ptr(), libc::AT_FDCWD, to.as_ptr(), libc::RENAME_NOREPLACE) };
            if result != 0 {
                return Err(os(&io::Error::last_os_error(), destination));
            }
            Ok(())
        }
        Err(error) => Err(PictureError::Os(match error.raw_os_error() {
            Some(_) => format!("{}: '{}' -> '{}'", suite_core::pyerr::os_error_text(&error), temporary.display(), destination.display()),
            None => error.to_string(),
        })),
    }
}

/// `info` of an opened image (`format`, per-frame `duration` in ms).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metadata {
    pub format: Option<String>,
    pub duration: Option<u32>,
}

pub struct Loaded {
    pub image: RgbaImage,
    pub metadata: Metadata,
    pub exif: ExifData,
    pub frames: usize,
}

fn pillow_format(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "PNG",
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Gif => "GIF",
        ImageFormat::WebP => "WEBP",
        ImageFormat::Tiff => "TIFF",
        ImageFormat::Bmp => "BMP",
        ImageFormat::Ico => "ICO",
        ImageFormat::Pnm => "PPM",
        ImageFormat::Tga => "TGA",
        ImageFormat::Qoi => "QOI",
        _ => "IMAGE",
    }
}

fn image_error(error: image::ImageError) -> PictureError { PictureError::Os(error.to_string()) }

/// Count frames/pages like Pillow's `n_frames` (GIF image blocks, APNG `acTL`, WebP `ANMF`, TIFF IFDs).
pub fn frame_count(path: &Path, format: ImageFormat) -> usize {
    let Ok(mut file) = fs::File::open(path) else { return 1 };
    let mut data = Vec::new();
    if file.read_to_end(&mut data).is_err() {
        return 1;
    }
    match format {
        ImageFormat::Gif => gif_frames(&data).max(1),
        ImageFormat::Png => png_frames(&data).max(1),
        ImageFormat::WebP => webp_frames(&data).max(1),
        ImageFormat::Tiff => tiff_pages(&data).max(1),
        _ => 1,
    }
}

fn gif_frames(data: &[u8]) -> usize {
    if data.len() < 13 {
        return 1;
    }
    let mut pos = 13;
    if data[10] & 0x80 != 0 {
        pos += 3 << ((data[10] & 7) + 1);
    }
    let skip_blocks = |mut pos: usize| -> Option<usize> {
        loop {
            let size = *data.get(pos)? as usize;
            pos += 1 + size;
            if size == 0 {
                return Some(pos);
            }
        }
    };
    let mut frames = 0;
    while let Some(&kind) = data.get(pos) {
        match kind {
            0x21 => match skip_blocks(pos + 2) { Some(next) => pos = next, None => break },
            0x2c => {
                frames += 1;
                let Some(&flags) = data.get(pos + 9) else { break };
                pos += 10;
                if flags & 0x80 != 0 {
                    pos += 3 << ((flags & 7) + 1);
                }
                match skip_blocks(pos + 1) { Some(next) => pos = next, None => break }
            }
            _ => break,
        }
    }
    frames
}

fn png_frames(data: &[u8]) -> usize {
    let mut pos = 8;
    while pos + 8 <= data.len() {
        let length = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let kind = &data[pos + 4..pos + 8];
        if kind == b"acTL" && pos + 12 <= data.len() {
            return u32::from_be_bytes(data[pos + 8..pos + 12].try_into().unwrap()) as usize;
        }
        if kind == b"IDAT" {
            break;
        }
        pos += 12 + length;
    }
    1
}

fn webp_frames(data: &[u8]) -> usize {
    let mut pos = 12;
    let mut frames = 0;
    while pos + 8 <= data.len() {
        let size = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if &data[pos..pos + 4] == b"ANMF" {
            frames += 1;
        }
        pos += 8 + size + (size & 1);
    }
    frames.max(1)
}

fn tiff_pages(data: &[u8]) -> usize {
    if data.len() < 8 {
        return 1;
    }
    let little = &data[..2] == b"II";
    let u16_at = |p: usize| data.get(p..p + 2).map(|b| if little { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) });
    let u32_at = |p: usize| data.get(p..p + 4).map(|b| {
        let b: [u8; 4] = b.try_into().unwrap();
        if little { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) }
    });
    if u16_at(2) != Some(42) {
        return 1;
    }
    let mut offset = u32_at(4).unwrap_or(0) as usize;
    let mut pages = 0;
    while offset != 0 && pages < 10_000 {
        let Some(count) = u16_at(offset) else { break };
        pages += 1;
        offset = u32_at(offset + 2 + count as usize * 12).unwrap_or(0) as usize;
    }
    pages.max(1)
}

fn strip_exif_header(mut data: Vec<u8>) -> Vec<u8> {
    if data.starts_with(b"Exif\0\0") {
        data.drain(..6);
    }
    data
}

/// Decode one frame of an animation through the codec's composited frame iterator.
fn animation_frame(path: &Path, format: ImageFormat, frame: usize) -> Result<(RgbaImage, u32)> {
    use image::AnimationDecoder;
    let open = || fs::File::open(path).map(BufReader::new).map_err(|e| os(&e, path));
    let mut frames: Box<dyn Iterator<Item = image::ImageResult<image::Frame>>> = match format {
        ImageFormat::Gif => Box::new(image::codecs::gif::GifDecoder::new(open()?).map_err(image_error)?.into_frames()),
        ImageFormat::Png => Box::new(image::codecs::png::PngDecoder::new(open()?).map_err(image_error)?.apng().map_err(image_error)?.into_frames()),
        ImageFormat::WebP => Box::new(image::codecs::webp::WebPDecoder::new(open()?).map_err(image_error)?.into_frames()),
        _ => return Err(PictureError::Value("EOF".into())),
    };
    let found = frames.nth(frame).ok_or_else(|| PictureError::Value("no more images in file".into()))?.map_err(image_error)?;
    let (numerator, denominator) = found.delay().numer_denom_ms();
    let delay = if denominator == 0 { 0 } else { numerator / denominator };
    Ok((found.into_buffer(), delay))
}

/// `load_pixels(path, thumbnail=None, frame=0)`: EXIF orientation and embedded colour profiles applied once.
pub fn load_pixels(path: &Path, thumbnail: Option<u32>, frame: usize) -> Result<Loaded> {
    let reader = image::ImageReader::open(path).map_err(|e| os(&e, path))?.with_guessed_format().map_err(|e| os(&e, path))?;
    let Some(format) = reader.format() else {
        return load_with_pixbuf(path, thumbnail);
    };
    let mut decoder = reader.into_decoder().map_err(image_error)?;
    let (width, height) = decoder.dimensions();
    checked_size(width as i64, height as i64)?;
    let frames = frame_count(path, format);
    let mut metadata = Metadata { format: Some(pillow_format(format).into()), duration: None };
    let exif_bytes = match format {
        ImageFormat::Tiff => None,
        _ => decoder.exif_metadata().ok().flatten().map(strip_exif_header),
    };
    let mut exif = match exif_bytes {
        Some(bytes) => ExifData::parse(bytes),
        None if format == ImageFormat::Tiff => ExifData::from_container(path),
        None => ExifData::default(),
    };
    let icc = decoder.icc_profile().ok().flatten();
    let mut image = if frames > 1 && matches!(format, ImageFormat::Gif | ImageFormat::Png | ImageFormat::WebP) {
        let (buffer, delay) = animation_frame(path, format, frame)?;
        metadata.duration = Some(delay);
        DynamicImage::ImageRgba8(buffer)
    } else if frame > 0 {
        return Err(PictureError::Value(tr!("Save a still-frame copy before editing an animation or multi-page image.")));
    } else {
        DynamicImage::from_decoder(decoder).map_err(image_error)?
    };
    if let Some(orientation) = exif.orientation().and_then(image::metadata::Orientation::from_exif) {
        image.apply_orientation(orientation);
    }
    exif.set_orientation(1);
    let mut rgba = image.into_rgba8();
    if let Some(profile) = icc.filter(|p| !p.is_empty()) {
        // Pillow falls back to a plain conversion when the profile is unusable.
        let mut converted = rgba.clone();
        if lcms::to_srgb(&profile, &mut converted).is_ok() {
            rgba = converted;
        }
    }
    if let Some(size) = thumbnail {
        rgba = thumbnail_of(&rgba, size);
    }
    Ok(Loaded { image: rgba, metadata, exif, frames })
}

fn thumbnail_of(image: &RgbaImage, size: u32) -> RgbaImage {
    let (w, h) = image.dimensions();
    if w <= size && h <= size {
        return image.clone();
    }
    let scale = (size as f64 / w as f64).min(size as f64 / h as f64);
    let (nw, nh) = (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1));
    image::imageops::resize(image, nw, nh, image::imageops::FilterType::Lanczos3)
}

fn load_with_pixbuf(path: &Path, thumbnail: Option<u32>) -> Result<Loaded> {
    use gtk::gdk_pixbuf::Pixbuf;
    let (width, height) = Pixbuf::file_info(path).map(|(_, w, h)| (w, h)).unwrap_or((0, 0));
    checked_size(width as i64, height as i64)?;
    let pixbuf = match thumbnail {
        Some(size) => Pixbuf::from_file_at_scale(path, size as i32, size as i32, true),
        None => Pixbuf::from_file(path),
    }.map_err(|e| PictureError::Os(suite_core::pyerr::python_error_text(&e)))?;
    checked_size(pixbuf.width() as i64, pixbuf.height() as i64)?;
    let (w, h, stride, channels) = (pixbuf.width() as usize, pixbuf.height() as usize, pixbuf.rowstride() as usize, pixbuf.n_channels() as usize);
    let bytes = pixbuf.read_pixel_bytes();
    let mut image = RgbaImage::new(w as u32, h as u32);
    for y in 0..h {
        for x in 0..w {
            let p = &bytes[y * stride + x * channels..];
            let alpha = if channels == 4 { p[3] } else { 255 };
            image.put_pixel(x as u32, y as u32, Rgba([p[0], p[1], p[2], alpha]));
        }
    }
    Ok(Loaded { image, metadata: Metadata::default(), exif: ExifData::default(), frames: 1 })
}

/// `thumbnail_png(path, size)` without the PNG round trip: RGBA pixels for a texture.
pub fn thumbnail(path: &Path, size: u32) -> Result<RgbaImage> { Ok(load_pixels(path, Some(size), 0)?.image) }

/// Pillow's `L24` luma.
pub fn luma(p: &Rgba<u8>) -> u8 {
    ((p[0] as u32 * 19595 + p[1] as u32 * 38470 + p[2] as u32 * 7471 + 0x8000) >> 16) as u8
}

/// `Image.blend(degenerate, image, factor)` per channel (C float arithmetic).
fn blend(degenerate: u8, value: u8, alpha: f32) -> u8 {
    let result = degenerate as i32 as f32 + alpha * (value as i32 - degenerate as i32) as f32;
    if (0.0..=1.0).contains(&alpha) {
        result as u8
    } else if result <= 0.0 {
        0
    } else if result >= 255.0 {
        255
    } else {
        result as u8
    }
}

/// `adjusted(image, brightness, contrast, saturation, gamma)` (ImageEnhance + gamma LUT, alpha kept).
pub fn adjusted(image: &RgbaImage, brightness: f64, contrast: f64, saturation: f64, gamma: f64) -> RgbaImage {
    let mut result = image.clone();
    let channels = |p: &mut Rgba<u8>, f: &dyn Fn(u8, usize) -> u8| for c in 0..3 { p[c] = f(p[c], c) };
    for p in result.pixels_mut() {
        channels(p, &|v, _| blend(0, v, brightness as f32));
    }
    let count = (result.width() as u64 * result.height() as u64).max(1);
    let sum: u64 = result.pixels().map(|p| luma(p) as u64).sum();
    let mean = (sum as f64 / count as f64 + 0.5) as u8;
    for p in result.pixels_mut() {
        channels(p, &|v, _| blend(mean, v, contrast as f32));
    }
    for p in result.pixels_mut() {
        let gray = luma(p);
        channels(p, &|v, _| blend(gray, v, saturation as f32));
    }
    if gamma != 1.0 {
        let gamma = gamma.clamp(0.1, 5.0);
        let lut: Vec<u8> = (0..256).map(|x| (255.0 * (x as f64 / 255.0).powf(1.0 / gamma)).round_ties_even() as u8).collect();
        for p in result.pixels_mut() {
            channels(p, &|v, _| lut[v as usize]);
        }
    }
    result
}

/// `ImageOps.autocontrast(image.convert('RGB'))` with alpha restored.
pub fn autocontrast(image: &RgbaImage) -> RgbaImage {
    let mut luts = Vec::new();
    for channel in 0..3 {
        let mut histogram = [0u64; 256];
        for p in image.pixels() {
            histogram[p[channel] as usize] += 1;
        }
        let lo = histogram.iter().position(|&n| n > 0);
        let hi = histogram.iter().rposition(|&n| n > 0);
        let lut: Vec<u8> = match (lo, hi) {
            (Some(lo), Some(hi)) if hi > lo => {
                let scale = 255.0 / (hi - lo) as f64;
                let offset = -(lo as f64) * scale;
                (0..256).map(|ix| ((ix as f64 * scale + offset) as i64).clamp(0, 255) as u8).collect()
            }
            _ => (0..256).map(|v| v as u8).collect(),
        };
        luts.push(lut);
    }
    let mut result = image.clone();
    for p in result.pixels_mut() {
        for c in 0..3 {
            p[c] = luts[c][p[c] as usize];
        }
    }
    result
}

fn rectangle(rect: [f64; 4], width: u32, height: u32) -> Result<(u32, u32, u32, u32)> {
    let [x, y, w, h] = rect.map(|v| v.round_ties_even() as i64);
    if x < 0 || y < 0 || w < 1 || h < 1 || x + w > width as i64 || y + h > height as i64 {
        return Err(PictureError::Value(tr!("Select a rectangle inside the image.")));
    }
    Ok((x as u32, y as u32, w as u32, h as u32))
}

#[derive(Clone, Debug, PartialEq)]
pub enum AnnotationKind {
    Rectangle,
    Ellipse,
    Line,
    Arrow,
    Pen,
    Text,
    Stamp,
}

impl AnnotationKind {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "rectangle" => Self::Rectangle,
            "ellipse" => Self::Ellipse,
            "line" => Self::Line,
            "arrow" => Self::Arrow,
            "pen" => Self::Pen,
            "text" => Self::Text,
            "stamp" => Self::Stamp,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub kind: Option<AnnotationKind>,
    pub points: Vec<(f64, f64)>,
    pub color: [u8; 4],
    pub width: i64,
    pub text: String,
    pub stamp: Option<PathBuf>,
}

impl Default for Annotation {
    fn default() -> Self {
        Self { kind: None, points: Vec::new(), color: [0xe2, 0x3c, 0x4a, 255], width: 4, text: String::new(), stamp: None }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Operation {
    Left,
    Right,
    Mirror,
    Flip,
    Crop([f64; 4]),
    Resize(f64, f64),
    Adjust { brightness: f64, contrast: f64, saturation: f64, gamma: f64 },
    Grayscale,
    Autocontrast,
    RedEye([f64; 4]),
    Annotate(Annotation),
    Comment(String),
}

impl Operation {
    /// `operation.replace('-', ' ').capitalize()`.
    pub fn label(&self) -> &'static str {
        match self {
            Operation::Left => "Left",
            Operation::Right => "Right",
            Operation::Mirror => "Mirror",
            Operation::Flip => "Flip",
            Operation::Crop(_) => "Crop",
            Operation::Resize(..) => "Resize",
            Operation::Adjust { .. } => "Adjust",
            Operation::Grayscale => "Grayscale",
            Operation::Autocontrast => "Autocontrast",
            Operation::RedEye(_) => "Red eye",
            Operation::Annotate(_) => "Annotate",
            Operation::Comment(_) => "Comment",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Step {
    pub id: u64,
    pub path: PathBuf,
    pub label: String,
    pub comment: String,
}

pub type Encoder = fn(&RgbaImage, &str, &Path, u8, Option<Vec<u8>>, Option<Vec<u8>>) -> std::result::Result<(), String>;

pub struct PictureDocument {
    pub path: PathBuf,
    pub original_signature: Signature,
    pub image: RgbaImage,
    pub metadata: Metadata,
    pub exif: ExifData,
    pub frames: usize,
    pub temporary: PathBuf,
    pub history: Vec<Step>,
    pub index: usize,
    pub next_id: u64,
    pub comment: String,
    pub saved_id: u64,
    pub max_history: usize,
    pub max_history_bytes: u64,
    /// `Image.save` (tests inject failures).
    pub encoder: Encoder,
    /// `os.link` (tests simulate filesystems without hard links).
    pub link: LinkFn,
    closed: bool,
}

fn expand_user(path: &Path) -> PathBuf {
    match path.to_str() {
        Some("~") => suite_core::paths::home(),
        Some(text) if text.starts_with("~/") => suite_core::paths::home().join(&text[2..]),
        _ => path.to_path_buf(),
    }
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) }
}

/// `tempfile.mkdtemp(prefix)`: 0700 directory under `$TMPDIR` (std::env::temp_dir()).
fn make_temporary_directory(prefix: &str) -> io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    for _ in 0..100 {
        let path = std::env::temp_dir().join(format!("{prefix}{}", random_name()));
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::from_raw_os_error(libc::EEXIST))
}

fn random_name() -> String {
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
    let mut random = [0u8; 8];
    if fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut random)).is_err() {
        random = (suite_core::timer::now().to_bits() ^ std::process::id() as u64).to_le_bytes();
    }
    random.iter().map(|b| LETTERS[*b as usize % LETTERS.len()] as char).collect()
}

/// `tempfile.mkstemp(prefix, suffix, dir)`: 0600, O_EXCL.
fn make_temporary_file(directory: &Path, prefix: &str, suffix: &str) -> Result<PathBuf> {
    use std::os::unix::fs::OpenOptionsExt;
    for _ in 0..100 {
        let path = directory.join(format!("{prefix}{}{suffix}", random_name()));
        match fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(os(&e, &path)),
        }
    }
    Err(PictureError::Os("No usable temporary file name found".into()))
}

fn save_png(image: &RgbaImage, path: &Path, fast: bool) -> std::result::Result<(), String> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::ImageEncoder;
    let file = fs::File::create(path).map_err(|e| suite_core::pyerr::os_error_path(&e, path))?;
    let writer = io::BufWriter::new(file);
    let encoder = PngEncoder::new_with_quality(writer, if fast { CompressionType::Level(1) } else { CompressionType::Level(6) },
                                               FilterType::Adaptive);
    encoder.write_image(image.as_raw(), image.width(), image.height(), image::ExtendedColorType::Rgba8).map_err(|e| e.to_string())
}

fn read_png(path: &Path) -> Result<RgbaImage> {
    Ok(image::ImageReader::open(path).map_err(|e| os(&e, path))?.with_guessed_format().map_err(|e| os(&e, path))?
        .decode().map_err(image_error)?.into_rgba8())
}

/// Pillow `Image.new('RGB', size, 'white').paste(pixels, mask=alpha)`.
pub fn over_white(image: &RgbaImage) -> image::RgbImage {
    image::RgbImage::from_fn(image.width(), image.height(), |x, y| {
        let p = image.get_pixel(x, y);
        let a = p[3] as u32;
        let mix = |v: u8| {
            let t = v as u32 * a + 255 * (255 - a) + 128;
            ((t + (t >> 8)) >> 8) as u8
        };
        image::Rgb([mix(p[0]), mix(p[1]), mix(p[2])])
    })
}

/// `image.save(path, format, quality=…, exif=…, icc_profile=…)`.
pub fn encode(image: &RgbaImage, format: &str, path: &Path, quality: u8, exif: Option<Vec<u8>>, icc: Option<Vec<u8>>)
    -> std::result::Result<(), String> {
    use image::ImageEncoder;
    let file = fs::OpenOptions::new().write(true).truncate(true).open(path).map_err(|e| suite_core::pyerr::os_error_path(&e, path))?;
    let mut writer = io::BufWriter::new(file);
    let error = |e: image::ImageError| e.to_string();
    match format {
        "PNG" => {
            use image::codecs::png::{CompressionType, FilterType, PngEncoder};
            let mut encoder = PngEncoder::new_with_quality(&mut writer, CompressionType::Level(6), FilterType::Adaptive);
            if let Some(icc) = icc { let _ = encoder.set_icc_profile(icc); }
            if let Some(exif) = exif { let _ = encoder.set_exif_metadata(exif); }
            encoder.write_image(image.as_raw(), image.width(), image.height(), image::ExtendedColorType::Rgba8).map_err(error)?;
        }
        "JPEG" => {
            let rgb = over_white(image);
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, quality);
            if let Some(icc) = icc { let _ = encoder.set_icc_profile(icc); }
            if let Some(exif) = exif { let _ = encoder.set_exif_metadata(exif); }
            encoder.write_image(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8).map_err(error)?;
        }
        "WEBP" => {
            let mut encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut writer);
            if let Some(icc) = icc { let _ = encoder.set_icc_profile(icc); }
            if let Some(exif) = exif { let _ = encoder.set_exif_metadata(exif); }
            encoder.write_image(image.as_raw(), image.width(), image.height(), image::ExtendedColorType::Rgba8).map_err(error)?;
        }
        "TIFF" => {
            drop(writer);
            let mut file = fs::OpenOptions::new().write(true).truncate(true).open(path).map_err(|e| e.to_string())?;
            let mut encoder = image::codecs::tiff::TiffEncoder::new(&mut file);
            if let Some(icc) = icc { let _ = encoder.set_icc_profile(icc); }
            encoder.write_image(image.as_raw(), image.width(), image.height(), image::ExtendedColorType::Rgba8).map_err(error)?;
            return Ok(());
        }
        "BMP" => {
            let rgb = over_white(image);
            image::codecs::bmp::BmpEncoder::new(&mut writer)
                .write_image(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8).map_err(error)?;
        }
        _ => {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut writer);
            encoder.encode_frame(image::Frame::new(image.clone())).map_err(error)?;
        }
    }
    io::Write::flush(&mut writer).map_err(|e| e.to_string())
}

impl PictureDocument {
    pub fn open(path: &Path) -> Result<Self> {
        let expanded = expand_user(path);
        let path = fs::canonicalize(&expanded).map_err(|e| os(&e, &expanded))?;
        let original_signature = signature(&path).map_err(|e| os(&e, &path))?;
        let loaded = load_pixels(&path, None, 0)?;
        let temporary = make_temporary_directory("waypictures-undo-").map_err(|e| os(&e, &std::env::temp_dir()))?;
        let comment = loaded.exif.text(270).unwrap_or_default();
        let mut document = Self {
            path, original_signature, image: loaded.image, metadata: loaded.metadata, exif: loaded.exif, frames: loaded.frames,
            temporary, history: Vec::new(), index: 0, next_id: 0, comment, saved_id: 0, max_history: MAX_HISTORY,
            max_history_bytes: MAX_HISTORY_BYTES, encoder: encode, link: hard_link, closed: false,
        };
        match document.checkpoint("Original") {
            Ok(id) => document.saved_id = id,
            Err(error) => {
                document.close();
                return Err(error);
            }
        }
        Ok(document)
    }

    pub fn dirty(&self) -> bool { self.history[self.index].id != self.saved_id }
    pub fn can_undo(&self) -> bool { self.index > 0 }
    pub fn can_redo(&self) -> bool { self.index + 1 < self.history.len() }

    fn checkpoint(&mut self, label: &str) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        let path = self.temporary.join(format!("{id}.png"));
        save_png(&self.image, &path, true).map_err(PictureError::Os)?;
        if !self.history.is_empty() {
            for old in self.history.drain(self.index + 1..) {
                let _ = fs::remove_file(&old.path);
            }
        }
        self.history.push(Step { id, path, label: label.into(), comment: self.comment.clone() });
        self.index = self.history.len() - 1;
        let size = |steps: &[Step]| steps.iter().map(|s| fs::metadata(&s.path).map(|m| m.len()).unwrap_or(0)).sum::<u64>();
        while self.history.len() > self.max_history || (self.history.len() > 2 && size(&self.history) > self.max_history_bytes) {
            let _ = fs::remove_file(&self.history.remove(0).path);
            self.index -= 1;
        }
        Ok(id)
    }

    pub fn apply(&mut self, operation: Operation) -> Result<()> {
        if self.frames > 1 {
            return Err(PictureError::Value(tr!("Save a still-frame copy before editing an animation or multi-page image.")));
        }
        let source = &self.image;
        let (width, height) = source.dimensions();
        let result = match &operation {
            Operation::Left => image::imageops::rotate270(source),
            Operation::Right => image::imageops::rotate90(source),
            Operation::Mirror => image::imageops::flip_horizontal(source),
            Operation::Flip => image::imageops::flip_vertical(source),
            Operation::Crop(rect) => {
                let (x, y, w, h) = rectangle(*rect, width, height)?;
                image::imageops::crop_imm(source, x, y, w, h).to_image()
            }
            Operation::Resize(w, h) => {
                let (w, h) = checked_size(w.trunc() as i64, h.trunc() as i64)?;
                image::imageops::resize(source, w, h, image::imageops::FilterType::Lanczos3)
            }
            Operation::Adjust { brightness, contrast, saturation, gamma } => adjusted(source, *brightness, *contrast, *saturation, *gamma),
            Operation::Grayscale => {
                let mut result = source.clone();
                for p in result.pixels_mut() {
                    let l = luma(p);
                    *p = Rgba([l, l, l, p[3]]);
                }
                result
            }
            Operation::Autocontrast => autocontrast(source),
            Operation::RedEye(rect) => {
                let (x0, y0, w, h) = rectangle(*rect, width, height)?;
                let mut result = source.clone();
                for y in y0..y0 + h {
                    for x in x0..x0 + w {
                        let p = result.get_pixel_mut(x, y);
                        let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
                        let threshold = ((g.max(b) as f64 * 1.4) as i32).min(255);
                        if r - threshold > 0 && r >= 61 {
                            p[0] = ((g + b) as f64 / 2.0) as u8;
                        }
                    }
                }
                result
            }
            Operation::Annotate(annotation) => self.annotate(annotation)?,
            Operation::Comment(_) => source.clone(),
        };
        let old_comment = self.comment.clone();
        if let Operation::Comment(text) = &operation {
            self.comment = text.clone();
        }
        let previous = std::mem::replace(&mut self.image, result);
        if let Err(error) = self.checkpoint(operation.label()) {
            self.image = previous;
            self.comment = old_comment;
            return Err(error);
        }
        Ok(())
    }

    /// `annotate(kind, points, color, width, text, stamp)`.
    pub fn annotate(&self, annotation: &Annotation) -> Result<RgbaImage> {
        let mut result = self.image.clone();
        let (width_px, height_px) = result.dimensions();
        let width = annotation.width.clamp(1, 100) as f64;
        let points = &annotation.points;
        let (start, end) = (*points.first().unwrap_or(&(0.0, 0.0)), *points.last().unwrap_or(&(0.0, 0.0)));
        let bbox = (start.0.min(end.0), start.1.min(end.1), start.0.max(end.0), start.1.max(end.1));
        let kind = annotation.kind.clone().ok_or_else(|| PictureError::Value(tr!("Choose an annotation tool.")))?;
        if kind == AnnotationKind::Stamp {
            let Some(stamp) = &annotation.stamp else { return Err(PictureError::Value(tr!("Choose an annotation tool."))) };
            let overlay = load_pixels(stamp, None, 0)?.image;
            let target = ((bbox.2 - bbox.0).round().max(1.0) as u32, (bbox.3 - bbox.1).round().max(1.0) as u32);
            let (ow, oh) = overlay.dimensions();
            let overlay = if ow > target.0 || oh > target.1 {
                let scale = (target.0 as f64 / ow as f64).min(target.1 as f64 / oh as f64);
                image::imageops::resize(&overlay, ((ow as f64 * scale).round() as u32).max(1), ((oh as f64 * scale).round() as u32).max(1),
                                        image::imageops::FilterType::CatmullRom)
            } else {
                overlay
            };
            alpha_composite(&mut result, &overlay, bbox.0.round() as i64, bbox.1.round() as i64);
            return Ok(result);
        }
        // Shapes are drawn into an aliased mask and replace pixels (ImageDraw on RGBA);
        // text is antialiased and blended through its mask (draw_bitmap).
        let mut mask = cairo::ImageSurface::create(cairo::Format::A8, width_px as i32, height_px as i32)
            .map_err(|e| PictureError::Os(e.to_string()))?;
        {
            let cr = cairo::Context::new(&mask).map_err(|e| PictureError::Os(e.to_string()))?;
            cr.set_source_rgba(0.0, 0.0, 0.0, 1.0);
            let text = kind == AnnotationKind::Text;
            cr.set_antialias(if text { cairo::Antialias::Gray } else { cairo::Antialias::None });
            let (x0, y0, x1, y1) = bbox;
            match kind {
                AnnotationKind::Rectangle => {
                    cr.set_line_width(width);
                    let half = width / 2.0;
                    cr.rectangle(x0 + half, y0 + half, (x1 + 1.0 - x0 - width).max(0.0), (y1 + 1.0 - y0 - width).max(0.0));
                    let _ = cr.stroke();
                }
                AnnotationKind::Ellipse => {
                    cr.set_line_width(width);
                    let half = width / 2.0;
                    let (rx, ry) = (((x1 + 1.0 - x0) / 2.0 - half).max(0.5), ((y1 + 1.0 - y0) / 2.0 - half).max(0.5));
                    cr.save().ok();
                    cr.translate((x0 + x1 + 1.0) / 2.0, (y0 + y1 + 1.0) / 2.0);
                    cr.scale(rx, ry);
                    cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU);
                    cr.restore().ok();
                    let _ = cr.stroke();
                }
                AnnotationKind::Line | AnnotationKind::Arrow | AnnotationKind::Pen => {
                    cr.set_line_width(width);
                    let line: Vec<(f64, f64)> = if kind == AnnotationKind::Pen { points.clone() } else { vec![start, end] };
                    for (i, (x, y)) in line.iter().enumerate() {
                        if i == 0 { cr.move_to(x + 0.5, y + 0.5) } else { cr.line_to(x + 0.5, y + 0.5) }
                    }
                    let _ = cr.stroke();
                    if kind == AnnotationKind::Arrow {
                        let angle = (end.1 - start.1).atan2(end.0 - start.0);
                        let length = (width * 4.0).max(12.0);
                        cr.move_to(end.0 + 0.5, end.1 + 0.5);
                        for offset in [-0.5f64, 0.5] {
                            cr.line_to(end.0 - length * (angle + offset).cos() + 0.5, end.1 - length * (angle + offset).sin() + 0.5);
                        }
                        cr.close_path();
                        let _ = cr.fill();
                    }
                }
                AnnotationKind::Text => {
                    let size = (width * 5.0).max(12.0);
                    cr.select_font_face("DejaVu Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
                    cr.set_font_size(size);
                    let ascent = cr.font_extents().map(|e| e.ascent()).unwrap_or(size);
                    cr.move_to(start.0, start.1 + ascent);
                    let _ = cr.show_text(&annotation.text);
                }
                AnnotationKind::Stamp => {}
            }
        }
        mask.flush();
        let stride = mask.stride() as usize;
        let data = mask.data().map_err(|e| PictureError::Os(e.to_string()))?;
        let ink = annotation.color;
        for y in 0..height_px as usize {
            for x in 0..width_px as usize {
                let m = data[y * stride + x] as u32;
                if m == 0 {
                    continue;
                }
                let p = result.get_pixel_mut(x as u32, y as u32);
                if kind == AnnotationKind::Text && m < 255 {
                    for c in 0..4 {
                        let t = ink[c] as u32 * m + p[c] as u32 * (255 - m) + 128;
                        p[c] = ((t + (t >> 8)) >> 8) as u8;
                    }
                } else {
                    *p = Rgba(ink);
                }
            }
        }
        Ok(result)
    }

    pub fn undo(&mut self, redo: bool) -> Result<bool> {
        let target = self.index as i64 + if redo { 1 } else { -1 };
        if target < 0 || target >= self.history.len() as i64 {
            return Ok(false);
        }
        let target = target as usize;
        self.image = read_png(&self.history[target].path)?;
        self.index = target;
        self.comment = self.history[target].comment.clone();
        Ok(true)
    }

    fn exif_bytes(&self, size: (u32, u32)) -> Option<Vec<u8>> {
        let mut exif = self.exif.clone();
        exif.set_orientation(1);
        exif.set_text(270, &self.comment);
        exif.set_long(40962, size.0);
        exif.set_long(40963, size.1);
        exif.to_bytes()
    }

    /// `save(path=None, overwrite=False, quality=95, frame=None)` → the destination.
    pub fn save(&mut self, path: Option<&Path>, overwrite: bool, quality: i64, frame: Option<usize>) -> Result<PathBuf> {
        let destination = absolute(&expand_user(path.unwrap_or(&self.path)));
        let same = destination == self.path;
        if fs::symlink_metadata(&destination).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(PictureError::Value(tr!("Choose a regular file rather than replacing a symbolic link.")));
        }
        if same && self.frames > 1 {
            return Err(PictureError::Value(tr!("Use Save As to export a still frame without replacing the animation.")));
        }
        let expected = if same { Some(self.original_signature) }
                       else if destination.exists() { Some(signature(&destination).map_err(|e| os(&e, &destination))?) } else { None };
        if expected.is_some() && !(same || overwrite) {
            return Err(PictureError::FileExists(tr!("A file with that name already exists.")));
        }
        if same && signature(&destination).map_err(|e| os(&e, &destination))? != expected.unwrap() {
            return Err(PictureError::Runtime(tr!("This file changed outside Pictures. Use Save As to keep both versions.")));
        }
        let format = save_format(&destination).ok_or_else(|| PictureError::Value(tr!("Save as PNG, JPEG, WebP, TIFF, BMP or GIF.")))?;
        let parent = destination.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/"));
        let extension = destination.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        let temporary = make_temporary_file(&parent, ".waypictures-", &extension)?;
        let mut frame_history: Option<PathBuf> = None;
        let outcome = (|| -> Result<()> {
            let exported = match frame {
                Some(frame) if self.frames > 1 => {
                    let pixels = load_pixels(&self.path, None, frame)?.image;
                    let history = self.temporary.join(format!("{}.png", self.next_id));
                    save_png(&pixels, &history, true).map_err(PictureError::Os)?;
                    frame_history = Some(history);
                    Some(pixels)
                }
                _ => None,
            };
            let pixels = exported.as_ref().unwrap_or(&self.image);
            let (exif, icc) = if matches!(format, "JPEG" | "PNG" | "WEBP" | "TIFF") {
                (self.exif_bytes(pixels.dimensions()), lcms::srgb_profile())
            } else {
                (None, None)
            };
            (self.encoder)(pixels, format, &temporary, quality.clamp(1, 100) as u8, exif, icc).map_err(PictureError::Os)?;
            fs::File::open(&temporary).and_then(|f| f.sync_all()).map_err(|e| PictureError::Os(suite_core::pyerr::os_error_text(&e)))?;
            match expected {
                Some(expected) => {
                    if signature(&destination).map_err(|e| os(&e, &destination))? != expected {
                        return Err(PictureError::Runtime(tr!("The destination changed while saving. Choose a different name.")));
                    }
                    let mode = fs::metadata(&destination).map_err(|e| os(&e, &destination))?.permissions().mode() & 0o7777;
                    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode)).map_err(|e| os(&e, &temporary))?;
                    fs::rename(&temporary, &destination).map_err(|e| PictureError::Os(format!("{}: '{}' -> '{}'",
                        suite_core::pyerr::os_error_text(&e), temporary.display(), destination.display())))?;
                }
                // An exclusive link avoids overwriting a file created since the save dialog was accepted.
                None => publish_new(&temporary, &destination, self.link)?,
            }
            self.path = destination.clone();
            self.original_signature = signature(&destination).map_err(|e| os(&e, &destination))?;
            if let Some(pixels) = exported {
                self.image = pixels;
                // Editing the exported still must never restore an unrelated earlier frame through Undo.
                for step in self.history.drain(..) {
                    let _ = fs::remove_file(&step.path);
                }
                self.history.push(Step { id: self.next_id, path: frame_history.take().unwrap(), label: "Exported frame".into(),
                                         comment: self.comment.clone() });
                self.next_id += 1;
                self.index = 0;
            }
            self.saved_id = self.history[self.index].id;
            self.frames = 1;
            self.metadata.format = Some(format.into());
            Ok(())
        })();
        let _ = fs::remove_file(&temporary);
        if let Some(history) = frame_history {
            let _ = fs::remove_file(history);
        }
        outcome.map(|_| destination)
    }

    pub fn close(&mut self) {
        if !self.closed {
            self.closed = true;
            let _ = fs::remove_dir_all(&self.temporary);
        }
    }
}

impl Drop for PictureDocument {
    fn drop(&mut self) { self.close(); }
}

/// `Image.alpha_composite(overlay, dest)` (straight alpha, Pillow's integer formula).
pub fn alpha_composite(base: &mut RgbaImage, overlay: &RgbaImage, left: i64, top: i64) {
    for (x, y, src) in overlay.enumerate_pixels() {
        let (tx, ty) = (left + x as i64, top + y as i64);
        if tx < 0 || ty < 0 || tx >= base.width() as i64 || ty >= base.height() as i64 {
            continue;
        }
        let dst = base.get_pixel_mut(tx as u32, ty as u32);
        let (sa, da) = (src[3] as u32, dst[3] as u32);
        if sa == 0 {
            continue;
        }
        // outA = srcA + dstA·(1 − srcA)
        let blend = da * (255 - sa);
        let out_a = sa * 255 + blend;
        if out_a == 0 {
            *dst = Rgba([0, 0, 0, 0]);
            continue;
        }
        let coef1 = (sa * 255 * 255 * 128 / out_a) as u64;
        let coef2 = 255 * 128 - coef1;
        let mut out = [0u8; 4];
        for c in 0..3 {
            let t = src[c] as u64 * coef1 + dst[c] as u64 * coef2 + (0x80 << 7);
            out[c] = (((t >> 7) + (t >> 15)) >> 8).min(255) as u8;
        }
        let ta = out_a + 0x80;
        out[3] = ((ta + (ta >> 8)) >> 8).min(255) as u8;
        *dst = Rgba(out);
    }
}

