// Draws the current style into a pixel buffer (premultiplied RGBA).
// Coordinates are worked out in logical points and multiplied by `scale` for pixels.

use crate::analyzer::BANDS;
use crate::config::Style;
use crate::engine::{cover_side, shape_radius, Engine, ScreenInfo, StageRect, BASELINE_GAP, RESTING, WAVE_PAD};
use crate::palette::{self, Rgba};
use crate::shapes;
use std::f32::consts::{FRAC_PI_2, PI};
use std::sync::Arc;
use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Paint, PathBuilder, Pixmap, Point, Rect, Shader,
    SpreadMode, Stroke, Transform,
};

/// Reuse the pixmap if it's already the right size.
pub fn ensure_pixmap(slot: &mut Option<Pixmap>, w: u32, h: u32) -> Option<&mut Pixmap> {
    let fits = matches!(slot, Some(p) if p.width() == w && p.height() == h);
    if !fits {
        *slot = Pixmap::new(w.max(1), h.max(1));
    }
    slot.as_mut()
}

impl Engine {
    pub fn render(&mut self, pixmap: &mut Pixmap, stage: StageRect, screen: &ScreenInfo, scale: f32) {
        pixmap.fill(Color::TRANSPARENT);
        match self.config.style {
            Style::Wave | Style::Bars => self.draw_wave(pixmap, stage, screen, scale),
            Style::Shape => self.draw_shape(pixmap, stage, screen, scale),
            Style::Cover => self.draw_cover(pixmap, stage, screen, scale),
        }
    }

    /// The five gradient colours right now: resting white fading into the album colours.
    fn colors(&self) -> [Rgba; 5] {
        let mut out = [[0.0; 4]; 5];
        for (i, stop) in out.iter_mut().enumerate() {
            for c in 0..4 {
                stop[c] = RESTING[c] + (self.palette[i][c] - RESTING[c]) * self.mix;
            }
        }
        out
    }

    fn draw_wave(&self, pixmap: &mut Pixmap, stage: StageRect, screen: &ScreenInfo, s: f32) {
        let n = BANDS;
        let soft = &self.soft;
        let mirrored = self.config.mirrored;
        let left = WAVE_PAD;
        let width = (stage.w - 2.0 * WAVE_PAD).max(1.0);
        let baseline = stage.h - BASELINE_GAP;
        let max_h = screen.height * 0.40 * self.vol_scale;
        // Symmetrical: bass in the middle, treble out to both ends. Left to right: bass on the left.
        let total = if mirrored { 2 * n - 1 } else { n };
        let band_of = |j: usize| if mirrored { (j as isize - (n as isize - 1)).unsigned_abs() } else { j };
        let colors = self.colors();
        let (gx0, gx1) = (left * s, (left + width) * s);

        if self.config.style == Style::Bars {
            // Flat-topped bars standing on the Dock.
            let slot = width / total as f32;
            let bar_w = (slot * 0.68).max(1.0);
            let mut pb = PathBuilder::new();
            for j in 0..total {
                let h = (soft[band_of(j)] * max_h).max(2.0);
                let x = left + j as f32 * slot + (slot - bar_w) / 2.0;
                if let Some(rect) = Rect::from_xywh(x * s, (baseline - h) * s, bar_w * s, h * s) {
                    pb.push_rect(rect);
                }
            }
            if let (Some(path), Some(shader)) = (pb.finish(), gradient(&colors, 1.0, gx0, gx1)) {
                pixmap.fill_path(&path, &paint(shader), FillRule::Winding, Transform::identity(), None);
            }
            return;
        }

        // One smooth wave, easing down to nothing at the ends.
        let taper_span = if mirrored { 0.12 } else { 0.05 };
        let points: Vec<(f32, f32)> = (0..total)
            .map(|j| {
                let t = j as f32 / (total - 1) as f32;
                let edge = t.min(1.0 - t) / taper_span;
                let taper = if edge >= 1.0 { 1.0 } else { (edge * FRAC_PI_2).sin() };
                (left + width * t, baseline - soft[band_of(j)] * max_h * taper)
            })
            .collect();

        let mut line = PathBuilder::new();
        let mut fill = PathBuilder::new();
        line.move_to(points[0].0 * s, points[0].1 * s);
        fill.move_to(points[0].0 * s, points[0].1 * s);
        // Smooth curve through the points (Catmull-Rom as Bezier segments), never dipping below the Dock.
        for i in 0..points.len() - 1 {
            let p0 = points[i.saturating_sub(1)];
            let p1 = points[i];
            let p2 = points[i + 1];
            let p3 = points[(i + 2).min(points.len() - 1)];
            let c1 = (p1.0 + (p2.0 - p0.0) / 6.0, (p1.1 + (p2.1 - p0.1) / 6.0).min(baseline));
            let c2 = (p2.0 - (p3.0 - p1.0) / 6.0, (p2.1 - (p3.1 - p1.1) / 6.0).min(baseline));
            for pb in [&mut line, &mut fill] {
                pb.cubic_to(c1.0 * s, c1.1 * s, c2.0 * s, c2.1 * s, p2.0 * s, p2.1 * s);
            }
        }
        fill.line_to((left + width) * s, baseline * s);
        fill.line_to(left * s, baseline * s);
        fill.close();

        if self.mix > 0.01 {
            if let (Some(path), Some(shader)) = (fill.finish(), gradient(&colors, 0.40 * self.mix, gx0, gx1)) {
                pixmap.fill_path(&path, &paint(shader), FillRule::Winding, Transform::identity(), None);
            }
        }
        if let (Some(path), Some(shader)) = (line.finish(), gradient(&colors, 1.0, gx0, gx1)) {
            let stroke = Stroke {
                width: (1.5 + 1.5 * self.mix) * s,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            pixmap.stroke_path(&path, &paint(shader), &stroke, Transform::identity(), None);
        }
    }

    /// A spinning 3D shape made of dots, with optional smaller copies orbiting it.
    fn draw_shape(&self, pixmap: &mut Pixmap, stage: StageRect, screen: &ScreenInfo, s: f32) {
        let n = BANDS;
        let soft = &self.soft;
        let vol = self.vol_scale;
        let mirrored = self.config.mirrored;
        let cx = stage.w / 2.0;
        let cy = stage.h / 2.0;
        let radius = shape_radius(&self.config, screen) * (1.0 + self.bass * 0.10 * vol);
        let (cos_a, sin_a) = (self.orb_angle.cos(), self.orb_angle.sin());
        let tilt = 0.38f32; // lean towards the viewer so the top is visible
        let (cos_t, sin_t) = (tilt.cos(), tilt.sin());
        let colors = self.colors();
        let orbiters = self.config.orbiters as usize;
        let span = radius * if orbiters > 0 { 2.6 } else { 1.7 };
        let color_at = |x: f32| sample(&colors, ((x - (cx - span)) / (2.0 * span)).clamp(0.0, 1.0));
        let depth_alpha = [0.28f32, 0.60, 1.0];
        let mut dots: [Vec<(f32, f32, f32, Rgba)>; 3] = [Vec::new(), Vec::new(), Vec::new()];

        let all = shapes::points(self.config.shape);
        for p in all {
            // Symmetrical: bass around the middle, treble at both poles. Otherwise bass at the bottom.
            let lat = if mirrored { p.v.abs() } else { (p.v + 1.0) / 2.0 };
            let band = ((lat * (n - 1) as f32) as usize).min(n - 1);
            let level = soft[band];
            let swell = 1.0 + level * 0.55 * vol;

            // Spin around the vertical axis, then tilt.
            let x1 = p.x * cos_a + p.z * sin_a;
            let z1 = -p.x * sin_a + p.z * cos_a;
            let y2 = p.y * cos_t - z1 * sin_t;
            let z2 = p.y * sin_t + z1 * cos_t;

            // Nearer dots are larger, further apart and brighter.
            let perspective = 1.0 / (1.0 - z2 * 0.28);
            let px = cx + x1 * swell * radius * perspective;
            let py = cy - y2 * swell * radius * perspective;
            let size = (1.3 + level * 1.8) * perspective;
            let depth = if z2 < -0.3 { 0 } else if z2 < 0.3 { 1 } else { 2 };
            let mut color = color_at(px);
            color[3] *= depth_alpha[depth];
            dots[depth].push((px, py, size, color));
        }

        // Small copies circling the main shape, each on its own tilted orbit.
        if orbiters > 0 {
            let spin = self.orb_angle * 2.3;
            let (cos_s, sin_s) = (spin.cos(), spin.sin());
            let distance = 2.05f32;
            for moon in 0..orbiters {
                let theta = self.orbit_angle + moon as f32 / orbiters as f32 * 2.0 * PI;
                let band = (moon * n / orbiters + n / (2 * orbiters)).min(n - 1);
                let mini = 0.20 * (1.0 + soft[band] * 0.7 * vol);

                let lean = PI * moon as f32 / orbiters as f32;
                let swing = moon as f32 * 2.4;
                let flat_x = theta.cos() * distance;
                let flat_z = theta.sin() * distance;
                let leaned_x = flat_x * lean.cos();
                let oy = flat_x * lean.sin();
                let ox = leaned_x * swing.cos() + flat_z * swing.sin();
                let oz = -leaned_x * swing.sin() + flat_z * swing.cos();
                let centre_depth = oy * sin_t + oz * cos_t;
                let depth = if centre_depth < -0.6 { 0 } else if centre_depth < 0.6 { 1 } else { 2 };

                for p in all.iter().skip(moon % 5).step_by(5) {
                    let lx = (p.x * cos_s + p.z * sin_s) * mini + ox;
                    let lz = (-p.x * sin_s + p.z * cos_s) * mini + oz;
                    let ly = p.y * mini + oy;
                    let y2 = ly * cos_t - lz * sin_t;
                    let z2 = ly * sin_t + lz * cos_t;
                    let perspective = 1.0 / (1.0 - z2 * 0.14);
                    let px = cx + lx * radius * perspective;
                    let py = cy - y2 * radius * perspective;
                    let mut color = color_at(px);
                    color[3] *= depth_alpha[depth];
                    dots[depth].push((px, py, 1.1 * perspective, color));
                }
            }
        }

        let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
        let data = pixmap.data_mut();
        for layer in &dots {
            for &(x, y, r, color) in layer {
                dot(data, pw, ph, x * s, y * s, r * s, color);
            }
        }
    }

    /// The album cover rebuilt from a grid of dots that swell (and optionally wander) with the music.
    fn draw_cover(&mut self, pixmap: &mut Pixmap, stage: StageRect, screen: &ScreenInfo, s: f32) {
        let n = BANDS;
        let grid = self.config.cover_resolution.max(1) as usize;
        let cells = self.cover_cells(grid as u32);
        let soft = &self.soft;
        let vol = self.vol_scale;
        let mix = self.mix;
        let moving = self.config.cover_motion;

        let side = cover_side(&self.config, screen);
        let cell = side / grid as f32;
        let (centre_x, centre_y) = (stage.w / 2.0, stage.h / 2.0);
        let half = grid as f32 / 2.0;
        let push = 1.0 + self.bass * 0.10 * vol;
        let farthest = half * 2f32.sqrt();
        let alpha = 0.45 + 0.55 * mix;
        let clock = self.cover_clock;

        let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
        let data = pixmap.data_mut();
        for gy in 0..grid {
            for gx in 0..grid {
                let dx = gx as f32 + 0.5 - half;
                let dy = gy as f32 + 0.5 - half;
                let distance = (dx * dx + dy * dy).sqrt();

                // Symmetrical: bass in the centre, treble at the corners. Left to right: bass on the left edge.
                let t = if self.config.mirrored { distance / farthest } else { (gx as f32 + 0.5) / grid as f32 };
                let band = ((t * (n - 1) as f32) as usize).min(n - 1);
                let level = soft[band];

                let [mut r, mut g, mut b] = match &cells {
                    Some(cells) => cells[gy * grid + gx],
                    None => [0.6, 0.6, 0.6],
                };
                // Grey while silent, full colour while playing.
                let grey = 0.3 * r + 0.59 * g + 0.11 * b;
                r = grey + (r - grey) * mix;
                g = grey + (g - grey) * mix;
                b = grey + (b - grey) * mix;

                // Every dot gets its own rhythm so they don't move in step.
                let (mut ox, mut oy) = (0.0, 0.0);
                if moving {
                    let seed = (gx as f32 * 12.9898 + gy as f32 * 78.233).sin() * 43758.547;
                    let phase = (seed - seed.floor()) * 2.0 * PI;
                    let wander = cell * 0.95 * level * vol;
                    ox = (clock * 2.3 + phase).sin() * wander;
                    oy = (clock * 1.9 + phase * 1.37).cos() * wander;
                }

                let radius = if moving {
                    cell * 0.5 * (0.5 + level * 0.3 * vol).min(0.95)
                } else {
                    cell * 0.5 * (0.42 + level * 0.65 * vol).min(1.15)
                };
                let x = centre_x + dx * cell * push + ox;
                let y = centre_y + dy * cell * push + oy;
                dot(data, pw, ph, x * s, y * s, radius * s, [r, g, b, alpha]);
            }
        }
    }

    fn cover_cells(&mut self, grid: u32) -> Option<Arc<Vec<[f32; 3]>>> {
        let art = self.art.clone()?;
        if let Some((generation, size, cells)) = &self.cover_cache {
            if *generation == self.art_gen && *size == grid {
                return Some(cells.clone());
            }
        }
        let cells = Arc::new(palette::grid(&art.image, grid));
        self.cover_cache = Some((self.art_gen, grid, cells.clone()));
        Some(cells)
    }
}

fn paint(shader: Shader<'static>) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.shader = shader;
    paint.anti_alias = true;
    paint
}

/// Horizontal gradient through the five colours, between x0 and x1 (pixels).
fn gradient(colors: &[Rgba; 5], alpha: f32, x0: f32, x1: f32) -> Option<Shader<'static>> {
    let stops = colors
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let color = Color::from_rgba(
                c[0].clamp(0.0, 1.0),
                c[1].clamp(0.0, 1.0),
                c[2].clamp(0.0, 1.0),
                (c[3] * alpha).clamp(0.0, 1.0),
            )
            .unwrap_or(Color::TRANSPARENT);
            GradientStop::new(i as f32 / 4.0, color)
        })
        .collect();
    LinearGradient::new(
        Point::from_xy(x0, 0.0),
        Point::from_xy(x1.max(x0 + 1.0), 0.0),
        stops,
        SpreadMode::Pad,
        Transform::identity(),
    )
}

/// Colour at position t (0..1) along the five-stop gradient.
fn sample(colors: &[Rgba; 5], t: f32) -> Rgba {
    let x = t.clamp(0.0, 1.0) * 4.0;
    let i = (x.floor() as usize).min(3);
    let f = x - i as f32;
    let mut out = [0.0; 4];
    for c in 0..4 {
        out[c] = colors[i][c] + (colors[i + 1][c] - colors[i][c]) * f;
    }
    out
}

/// Draw an anti-aliased disc, blending over premultiplied RGBA pixels. Much faster than a path per dot.
fn dot(pixels: &mut [u8], width: i32, height: i32, cx: f32, cy: f32, r: f32, color: Rgba) {
    if color[3] <= 0.0 || r <= 0.0 {
        return;
    }
    let x0 = ((cx - r - 1.0).floor() as i32).max(0);
    let x1 = ((cx + r + 1.0).ceil() as i32).min(width - 1);
    let y0 = ((cy - r - 1.0).floor() as i32).max(0);
    let y1 = ((cy + r + 1.0).ceil() as i32).min(height - 1);
    if x0 > x1 || y0 > y1 {
        return;
    }
    for y in y0..=y1 {
        let fy = y as f32 + 0.5 - cy;
        for x in x0..=x1 {
            let fx = x as f32 + 0.5 - cx;
            let coverage = (r + 0.5 - (fx * fx + fy * fy).sqrt()).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let a = (color[3] * coverage).min(1.0);
            let inv = 1.0 - a;
            let i = ((y * width + x) * 4) as usize;
            for c in 0..3 {
                pixels[i + c] = (color[c] * a * 255.0 + pixels[i + c] as f32 * inv).round().min(255.0) as u8;
            }
            pixels[i + 3] = (a * 255.0 + pixels[i + 3] as f32 * inv).round().min(255.0) as u8;
        }
    }
}
