// Colour work on album covers: a small palette for the wave, and a grid of colours for the dot cover.

use image::imageops::FilterType;
use image::RgbaImage;

pub type Rgba = [f32; 4];

fn hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let mut h = 0.0;
    if d > 0.0 {
        h = if max == r {
            ((g - b) / d) % 6.0
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        h /= 6.0;
        if h < 0.0 {
            h += 1.0;
        }
    }
    (h, if max == 0.0 { 0.0 } else { d / max }, max)
}

fn rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let h6 = (h.rem_euclid(1.0)) * 6.0;
    let i = h6.floor();
    let f = h6 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    match (i as i32).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

/// The cover's most prominent vivid colours, brightened enough to show on a desktop,
/// arranged symmetrically with the main colour in the middle.
pub fn palette(image: &RgbaImage) -> Vec<Rgba> {
    let small = image::imageops::resize(image, 32, 32, FilterType::Triangle);
    const BUCKETS: usize = 12;
    let mut weight = [0.0f32; BUCKETS];
    let mut sum_r = [0.0f32; BUCKETS];
    let mut sum_g = [0.0f32; BUCKETS];
    let mut sum_b = [0.0f32; BUCKETS];
    let mut total = 0.0f32;

    for p in small.pixels() {
        let (r, g, b) = (p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0);
        let (h, s, v) = hsv(r, g, b);
        if s < 0.18 || v < 0.18 {
            continue;
        }
        let w = s * s * v;
        let bucket = ((h * BUCKETS as f32) as usize).min(BUCKETS - 1);
        weight[bucket] += w;
        sum_r[bucket] += r * w;
        sum_g[bucket] += g * w;
        sum_b[bucket] += b * w;
        total += w;
    }

    // Black-and-white or washed-out cover: a clean white wave.
    if total < 12.0 {
        let white = [0.97, 0.97, 0.97, 1.0];
        let grey = [0.72, 0.72, 0.72, 1.0];
        return vec![grey, white, grey];
    }

    let mut order: Vec<usize> = (0..BUCKETS).collect();
    order.sort_by(|a, b| weight[*b].partial_cmp(&weight[*a]).unwrap_or(std::cmp::Ordering::Equal));
    let top = weight[order[0]];
    let mut picked: Vec<Rgba> = Vec::new();
    for &bucket in order.iter().take(3) {
        if weight[bucket] < top * 0.15 {
            continue;
        }
        let w = weight[bucket];
        let (h, s, v) = hsv(sum_r[bucket] / w, sum_g[bucket] / w, sum_b[bucket] / w);
        let (r, g, b) = rgb(h, s.max(0.55), v.max(0.90));
        picked.push([r, g, b, 1.0]);
    }

    match picked.len() {
        3 => vec![picked[2], picked[1], picked[0], picked[1], picked[2]],
        2 => vec![picked[1], picked[0], picked[1]],
        _ => {
            let main = picked[0];
            let soft = [main[0], main[1], main[2], 0.55];
            vec![soft, main, soft]
        }
    }
}

/// The cover shrunk to n x n colours, top row first: one colour per dot.
pub fn grid(image: &RgbaImage, n: u32) -> Vec<[f32; 3]> {
    let n = n.max(1);
    let small = image::imageops::resize(image, n, n, FilterType::Triangle);
    small
        .pixels()
        .map(|p| [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0])
        .collect()
}

/// Scale and crop an image to exactly fill w x h (like the "Fill" wallpaper setting).
#[allow(dead_code)]
pub fn fill(image: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    let (iw, ih) = (image.width().max(1), image.height().max(1));
    let scale = (w as f32 / iw as f32).max(h as f32 / ih as f32);
    let crop_w = ((w as f32 / scale).round() as u32).clamp(1, iw);
    let crop_h = ((h as f32 / scale).round() as u32).clamp(1, ih);
    let x = (iw - crop_w) / 2;
    let y = (ih - crop_h) / 2;
    let cropped = image::imageops::crop_imm(image, x, y, crop_w, crop_h).to_image();
    image::imageops::resize(&cropped, w.max(1), h.max(1), FilterType::Triangle)
}
