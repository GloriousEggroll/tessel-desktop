//! The PIL parts of capture: sRGB ICC profile, PNG/JPEG encoding with the
//! profile embedded, decoding, and PIL-identical Lanczos resampling.

use image::{ImageEncoder, RgbImage};

/// `ImageCms.ImageCmsProfile(ImageCms.createProfile('sRGB')).tobytes()` from
/// lcms2 (588 bytes). Only the header creation date (bytes 24..36) varies.
const SRGB_TEMPLATE: &[u8] = include_bytes!("../assets/srgb-lcms2.icc");

/// The lcms2 sRGB profile with the current UTC creation date, as PIL writes it.
pub fn srgb_profile() -> Vec<u8> {
    let mut profile = SRGB_TEMPLATE.to_vec();
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::gmtime_r(&now, &mut tm) };
    let fields = [tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec];
    for (index, value) in fields.iter().enumerate() {
        profile[24 + index * 2..26 + index * 2].copy_from_slice(&(*value as u16).to_be_bytes());
    }
    profile
}

fn image_error(error: image::ImageError) -> String {
    error.to_string()
}

/// `Image.save(stream, 'PNG', icc_profile=srgb_profile())` for an RGB image.
pub fn png_rgb_with_icc(image: &RgbImage) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new(&mut output);
    encoder.set_icc_profile(srgb_profile()).map_err(|e| e.to_string())?;
    encoder.write_image(image.as_raw(), image.width(), image.height(), image::ExtendedColorType::Rgb8).map_err(image_error)?;
    Ok(output)
}

/// `im.convert('RGB').save(stream, 'JPEG', quality=95, subsampling=0, icc_profile=srgb_profile())`.
pub fn jpeg_with_icc(image: &RgbImage) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    // image's baseline encoder samples every component 1×1 (4:4:4 = PIL subsampling=0).
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 95);
    encoder.set_icc_profile(srgb_profile()).map_err(|e| e.to_string())?;
    encoder.write_image(image.as_raw(), image.width(), image.height(), image::ExtendedColorType::Rgb8).map_err(image_error)?;
    Ok(output)
}

/// `Image.open(BytesIO(data)).convert('RGB')`.
pub fn decode_rgb(data: &[u8]) -> Result<RgbImage, String> {
    Ok(image::load_from_memory(data).map_err(image_error)?.to_rgb8())
}

/// Re-encode any supported file as PNG in its own colour type (`picture.save(stream, 'PNG')`).
pub fn to_png(data: &[u8]) -> Result<Vec<u8>, String> {
    let picture = image::load_from_memory(data).map_err(image_error)?;
    let mut output = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut output, image::ImageFormat::Png).map_err(image_error)?;
    Ok(output.into_inner())
}

/// Embedded ICC profile of a PNG or JPEG, if any (tests / parity checks).
pub fn icc_profile(data: &[u8]) -> Option<Vec<u8>> {
    use image::ImageDecoder;
    let cursor = std::io::Cursor::new(data);
    match image::guess_format(data).ok()? {
        image::ImageFormat::Png => image::codecs::png::PngDecoder::new(cursor).ok()?.icc_profile().ok()?,
        image::ImageFormat::Jpeg => image::codecs::jpeg::JpegDecoder::new(cursor).ok()?.icc_profile().ok()?,
        _ => None,
    }
}

// ---- PIL Lanczos (libImaging/Resample.c), 8-bit RGB ----

const PRECISION_BITS: u32 = 32 - 8 - 2;

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        return 1.0;
    }
    let x = x * std::f64::consts::PI;
    x.sin() / x
}

fn lanczos(x: f64) -> f64 {
    if (-3.0..3.0).contains(&x) { sinc(x) * sinc(x / 3.0) } else { 0.0 }
}

/// `precompute_coeffs` + `normalize_coeffs_8bpc`: (ksize, bounds[xmin, xmax], fixed-point weights).
fn coefficients(in_size: usize, out_size: usize) -> (usize, Vec<(usize, usize)>, Vec<i32>) {
    let scale = in_size as f64 / out_size as f64;
    let filterscale = scale.max(1.0);
    let support = 3.0 * filterscale;
    let ksize = support.ceil() as usize * 2 + 1;
    let mut bounds = Vec::with_capacity(out_size);
    let mut weights = vec![0i32; out_size * ksize];
    let mut k = vec![0f64; ksize];
    for xx in 0..out_size {
        let center = (xx as f64 + 0.5) * scale;
        let ss = 1.0 / filterscale;
        let xmin = ((center - support + 0.5) as i64).max(0) as usize;
        let xmax = ((center + support + 0.5) as i64).min(in_size as i64) as usize - xmin;
        let mut ww = 0.0;
        for (x, slot) in k.iter_mut().enumerate().take(xmax) {
            let w = lanczos((x as f64 + xmin as f64 - center + 0.5) * ss);
            *slot = w;
            ww += w;
        }
        for (x, slot) in k.iter_mut().enumerate() {
            let value = if x < xmax { if ww != 0.0 { *slot / ww } else { *slot } } else { 0.0 };
            let scaled = value * (1u32 << PRECISION_BITS) as f64;
            weights[xx * ksize + x] = if value < 0.0 { (-0.5 + scaled) as i32 } else { (0.5 + scaled) as i32 };
        }
        bounds.push((xmin, xmax));
    }
    (ksize, bounds, weights)
}

fn clip8(value: i64) -> u8 {
    if value >= (1i64 << PRECISION_BITS << 8) {
        255
    } else if value <= 0 {
        0
    } else {
        (value >> PRECISION_BITS) as u8
    }
}

/// `im.resize((w, h), Image.Resampling.LANCZOS)` for RGB, pixel-identical to Pillow.
pub fn resize_lanczos(image: &RgbImage, width: u32, height: u32) -> RgbImage {
    let (in_w, in_h) = (image.width() as usize, image.height() as usize);
    let (out_w, out_h) = (width as usize, height as usize);
    if (in_w, in_h) == (out_w, out_h) {
        return image.clone();
    }
    let need_horizontal = out_w != in_w;
    let need_vertical = out_h != in_h;
    let (kh, bounds_h, weights_h) = coefficients(in_w, out_w);
    let (kv, mut bounds_v, weights_v) = coefficients(in_h, out_h);
    let first = bounds_v[0].0;
    let last = bounds_v[out_h - 1].0 + bounds_v[out_h - 1].1;
    let src = image.as_raw();
    let half = 1i64 << (PRECISION_BITS - 1);
    let (mut current, mut cur_w, mut cur_h) = (src.clone(), in_w, in_h);
    if need_horizontal {
        for bound in bounds_v.iter_mut() {
            bound.0 -= first;
        }
        let rows = last - first;
        let mut temp = vec![0u8; out_w * rows * 3];
        for y in 0..rows {
            let row = &src[(y + first) * in_w * 3..(y + first + 1) * in_w * 3];
            for (xx, (xmin, xmax)) in bounds_h.iter().enumerate() {
                let k = &weights_h[xx * kh..xx * kh + kh];
                let mut ss = [half; 3];
                for x in 0..*xmax {
                    let p = (x + xmin) * 3;
                    for c in 0..3 {
                        ss[c] += row[p + c] as i64 * k[x] as i64;
                    }
                }
                let o = (y * out_w + xx) * 3;
                for c in 0..3 {
                    temp[o + c] = clip8(ss[c]);
                }
            }
        }
        current = temp;
        cur_w = out_w;
        cur_h = rows;
    }
    if need_vertical {
        let mut output = vec![0u8; out_w * out_h * 3];
        for (yy, (ymin, ymax)) in bounds_v.iter().enumerate() {
            let k = &weights_v[yy * kv..yy * kv + kv];
            for xx in 0..cur_w {
                let mut ss = [half; 3];
                for y in 0..*ymax {
                    let p = ((y + ymin) * cur_w + xx) * 3;
                    for c in 0..3 {
                        ss[c] += current[p + c] as i64 * k[y] as i64;
                    }
                }
                let o = (yy * out_w + xx) * 3;
                for c in 0..3 {
                    output[o + c] = clip8(ss[c]);
                }
            }
        }
        current = output;
        cur_h = out_h;
    }
    debug_assert_eq!(cur_h, out_h);
    RgbImage::from_raw(out_w as u32, out_h as u32, current).expect("resampled buffer")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_matches_lcms2_except_the_creation_date() {
        let profile = srgb_profile();
        assert_eq!(profile.len(), 588);
        assert_eq!(&profile[..24], &SRGB_TEMPLATE[..24]);
        assert_eq!(&profile[36..], &SRGB_TEMPLATE[36..]);
        assert_eq!(&profile[36..40], b"acsp");
    }

    #[test]
    fn png_and_jpeg_embed_the_profile() {
        let image = RgbImage::from_pixel(5, 3, image::Rgb([10, 200, 30]));
        let png = png_rgb_with_icc(&image).unwrap();
        assert_eq!(icc_profile(&png).unwrap()[36..], SRGB_TEMPLATE[36..]);
        assert_eq!(decode_rgb(&png).unwrap(), image);
        let jpeg = jpeg_with_icc(&image).unwrap();
        assert_eq!(icc_profile(&jpeg).unwrap()[36..], SRGB_TEMPLATE[36..]);
        let back = decode_rgb(&jpeg).unwrap();
        assert!(back.pixels().all(|p| (p[1] as i32 - 200).abs() < 4));
    }

    /// Expected bytes from Pillow's `Image.resize(size, LANCZOS)` of the same pattern.
    #[test]
    fn lanczos_matches_pillow_byte_for_byte() {
        let (w, h) = (13u32, 9u32);
        let source = RgbImage::from_fn(w, h, |x, y| image::Rgb([((x * 37 + y * 11) % 256) as u8, ((x * x * 5 + y * 29) % 256) as u8,
                                                                ((x * y * 13 + 7) % 256) as u8]));
        for line in include_str!("../tests/fixtures/pil_lanczos.txt").lines() {
            let numbers: Vec<u32> = line.split(' ').map(|n| n.parse().unwrap()).collect();
            let resized = resize_lanczos(&source, numbers[0], numbers[1]);
            let expected: Vec<u8> = numbers[2..].iter().map(|n| *n as u8).collect();
            assert_eq!(resized.as_raw(), &expected, "{}x{}", numbers[0], numbers[1]);
        }
    }

    #[test]
    fn lanczos_keeps_flat_colour_and_size() {
        let image = RgbImage::from_pixel(32, 24, image::Rgb([90, 91, 92]));
        let small = resize_lanczos(&image, 16, 12);
        assert_eq!(small.dimensions(), (16, 12));
        assert!(small.pixels().all(|p| p.0 == [90, 91, 92]));
        let large = resize_lanczos(&image, 50, 7);
        assert!(large.pixels().all(|p| p.0 == [90, 91, 92]));
    }
}
