// Clouds of dots for each 3D shape, sized to fit roughly inside a ball of radius 1.

use crate::config::Shape;
use std::f32::consts::PI;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug)]
pub struct ShapePoint {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// Height within the shape, -1 (bottom) to 1 (top): decides which slice of the sound moves it.
    pub v: f32,
}

pub fn points(shape: Shape) -> &'static [ShapePoint] {
    static ALL: OnceLock<Vec<Vec<ShapePoint>>> = OnceLock::new();
    let all = ALL.get_or_init(|| Shape::ALL.iter().map(|&s| make(s)).collect());
    &all[shape.index()]
}

fn point(x: f32, y: f32, z: f32) -> ShapePoint {
    ShapePoint { x, y, z, v: 0.0 }
}

/// Dots spread evenly over a ball.
fn sphere(count: usize) -> Vec<ShapePoint> {
    let golden = PI * (3.0 - 5f32.sqrt());
    (0..count)
        .map(|k| {
            let y = 1.0 - 2.0 * (k as f32 + 0.5) / count as f32;
            let r = (1.0 - y * y).max(0.0).sqrt();
            let phi = k as f32 * golden;
            point(phi.cos() * r, y, phi.sin() * r)
        })
        .collect()
}

fn make(shape: Shape) -> Vec<ShapePoint> {
    let mut points = match shape {
        Shape::Sphere => sphere(720),

        Shape::Cube => {
            // A grid of dots on each of the six faces.
            let steps = 11;
            let half = 0.68f32;
            let mut out = Vec::new();
            for i in 0..steps {
                for j in 0..steps {
                    let a = (i as f32 / (steps - 1) as f32 * 2.0 - 1.0) * half;
                    let b = (j as f32 / (steps - 1) as f32 * 2.0 - 1.0) * half;
                    out.push(point(a, b, half));
                    out.push(point(a, b, -half));
                    out.push(point(half, a, b));
                    out.push(point(-half, a, b));
                    out.push(point(a, half, b));
                    out.push(point(a, -half, b));
                }
            }
            out
        }

        Shape::Donut => {
            // An upright ring facing the viewer.
            let (around, tube) = (44, 16);
            let (major, minor) = (0.72f32, 0.28f32);
            let mut out = Vec::new();
            for i in 0..around {
                let u = i as f32 / around as f32 * 2.0 * PI;
                for j in 0..tube {
                    let t = j as f32 / tube as f32 * 2.0 * PI;
                    let ring = major + minor * t.cos();
                    out.push(point(ring * u.cos(), ring * u.sin(), minor * t.sin()));
                }
            }
            out
        }

        Shape::Diamond => sphere(720)
            .into_iter()
            .map(|p| {
                // Push ball dots onto the faces of an eight-sided gem.
                let sum = p.x.abs() + p.y.abs() + p.z.abs();
                let scale = 1.05 / sum.max(0.0001);
                point(p.x * scale, p.y * scale, p.z * scale)
            })
            .collect(),

        Shape::Helix => {
            // Two twisting strands joined by rungs, like DNA.
            let steps = 240;
            let turns = 2.5f32;
            let radius = 0.42f32;
            let mut out = Vec::new();
            for k in 0..steps {
                let t = k as f32 / (steps - 1) as f32;
                let y = (1.0 - 2.0 * t) * 0.98;
                let angle = t * turns * 2.0 * PI;
                let (ax, az) = (angle.cos() * radius, angle.sin() * radius);
                out.push(point(ax, y, az));
                out.push(point(-ax, y, -az));
                if k % 10 == 0 {
                    for r in 1..8 {
                        let f = r as f32 / 8.0 * 2.0 - 1.0;
                        out.push(point(ax * f, y, az * f));
                    }
                }
            }
            out
        }
    };

    let top = points.iter().fold(0.0001f32, |m, p| m.max(p.y.abs()));
    for p in &mut points {
        p.v = p.y / top;
    }
    points
}
