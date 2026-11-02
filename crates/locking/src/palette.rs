//! `tessel-lock/lock_palette.py`: sample the lock surface's own rendered
//! wallpaper (including its crop) for the palette accent colour.
use std::collections::HashMap;

/// Area-average downsample of packed RGB (`stride` bytes per row) to
/// `size`×`size` (stands in for PIL's `resize((64, 64))`).
pub fn downsample(pixels: &[u8], width: usize, height: usize, stride: usize, size: usize) -> Vec<[u8; 3]> {
    let mut out = Vec::with_capacity(size * size);
    if width == 0 || height == 0 {
        return out;
    }
    for oy in 0..size {
        let (y0, y1) = (oy * height / size, ((oy + 1) * height / size).max(oy * height / size + 1).min(height));
        for ox in 0..size {
            let (x0, x1) = (ox * width / size, ((ox + 1) * width / size).max(ox * width / size + 1).min(width));
            let mut sum = [0u64; 3];
            let mut count = 0u64;
            for y in y0..y1 {
                let row = &pixels[y * stride..];
                for x in x0..x1 {
                    for (c, total) in sum.iter_mut().enumerate() {
                        *total += row[x * 3 + c] as u64;
                    }
                    count += 1;
                }
            }
            let count = count.max(1);
            out.push([((sum[0] + count / 2) / count) as u8, ((sum[1] + count / 2) / count) as u8,
                      ((sum[2] + count / 2) / count) as u8]);
        }
    }
    out
}

/// `colorsys.rgb_to_hsv` → (h, s, v), all 0‥1.
pub fn rgb_to_hsv(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let v = max;
    if min == max {
        return (0.0, 0.0, v);
    }
    let s = (max - min) / max;
    let (rc, gc, bc) = ((max - r) / (max - min), (max - g) / (max - min), (max - b) / (max - min));
    let h = if r == max { bc - gc } else if g == max { 2.0 + rc - bc } else { 4.0 + gc - rc };
    ((h / 6.0).rem_euclid(1.0), s, v)
}

/// `wallpaper_color(image)` on 64×64 samples: the bucket (32 levels per
/// channel) that maximises count·(.25+saturation)·(.35+brightness).
pub fn color_of_samples(samples: &[[u8; 3]]) -> (u8, u8, u8) {
    let mut colors: Vec<([u8; 3], u64)> = Vec::new();
    let mut index: HashMap<[u8; 3], usize> = HashMap::new();
    for pixel in samples {
        match index.get(pixel) {
            Some(&i) => colors[i].1 += 1,
            None => {
                index.insert(*pixel, colors.len());
                colors.push((*pixel, 1));
            }
        }
    }
    let mut order: Vec<[u8; 3]> = Vec::new();
    let mut buckets: HashMap<[u8; 3], [u64; 4]> = HashMap::new();
    for ([red, green, blue], count) in colors {
        let key = [red / 32, green / 32, blue / 32];
        let bucket = buckets.entry(key).or_insert_with(|| {
            order.push(key);
            [0; 4]
        });
        for (i, value) in [1, red as u64, green as u64, blue as u64].into_iter().enumerate() {
            bucket[i] += count * value;
        }
    }
    let score = |bucket: &[u64; 4]| {
        let count = bucket[0] as f64;
        let (_h, s, v) = rgb_to_hsv(bucket[1] as f64 / count / 255.0, bucket[2] as f64 / count / 255.0, bucket[3] as f64 / count / 255.0);
        count * (0.25 + s) * (0.35 + v)
    };
    let mut winner: Option<[u64; 4]> = None;
    for key in order {
        let bucket = buckets[&key];
        if winner.is_none_or(|w| score(&bucket) > score(&w)) {
            winner = Some(bucket);
        }
    }
    let Some(w) = winner else { return (22, 24, 32) };
    let channel = |i: usize| (w[i] as f64 / w[0] as f64).round_ties_even() as u8;
    (channel(1), channel(2), channel(3))
}

/// `wallpaper_color(image)` for packed RGB pixels.
pub fn wallpaper_color(pixels: &[u8], width: usize, height: usize, stride: usize) -> (u8, u8, u8) {
    color_of_samples(&downsample(pixels, width, height, stride, 64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saturated_bright_bucket_beats_a_larger_grey_area() {
        // 70% mid grey, 30% saturated blue: blue wins on saturation.
        let (w, h) = (100usize, 10usize);
        let mut pixels = Vec::new();
        for _y in 0..h {
            for x in 0..w {
                pixels.extend_from_slice(if x < 70 { &[128, 128, 128] } else { &[20, 60, 230] });
            }
        }
        assert_eq!(wallpaper_color(&pixels, w, h, w * 3), (20, 60, 230));
        // A uniform image yields its own colour.
        let flat: Vec<u8> = [145u8, 181, 215].repeat(16 * 16);
        assert_eq!(wallpaper_color(&flat, 16, 16, 48), (145, 181, 215));
        assert_eq!(rgb_to_hsv(1.0, 0.0, 0.0), (0.0, 1.0, 1.0));
        let (h, s, v) = rgb_to_hsv(0.0, 0.0, 1.0);
        assert!((h - 2.0 / 3.0).abs() < 1e-9 && s == 1.0 && v == 1.0);
    }
}
