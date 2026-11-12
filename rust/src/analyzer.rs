// Turns incoming audio into 48 log-spaced band levels (0..1), like the Swift SpectrumAnalyzer.

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const BANDS: usize = 48;
const N: usize = 2048;
const HOP: usize = 512;

struct Window {
    ring: Vec<f32>,
    write: usize,
    pending: usize,
    hann: Vec<f32>,
    fft: Arc<dyn Fft<f32>>,
    scratch: Vec<Complex<f32>>,
}

pub struct Analyzer {
    window: Mutex<Window>,
    levels: Mutex<(Vec<f32>, Option<Instant>)>,
}

impl Analyzer {
    pub fn new() -> Arc<Analyzer> {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(N);
        let hann = (0..N)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos())
            .collect();
        Arc::new(Analyzer {
            window: Mutex::new(Window {
                ring: vec![0.0; N],
                write: 0,
                pending: 0,
                hann,
                fft,
                scratch: vec![Complex::new(0.0, 0.0); N],
            }),
            levels: Mutex::new((vec![0.0; BANDS], None)),
        })
    }

    /// Feed mono samples. Called from the audio thread.
    pub fn push(&self, samples: &[f32], rate: f64) {
        let Ok(mut window) = self.window.lock() else { return };
        for &sample in samples {
            let i = window.write;
            window.ring[i] = sample;
            window.write = (i + 1) % N;
        }
        window.pending += samples.len();
        if window.pending >= HOP {
            window.pending = 0;
            let levels = analyze(&mut window, rate);
            if let Ok(mut out) = self.levels.lock() {
                *out = (levels, Some(Instant::now()));
            }
        }
    }

    /// Latest band levels; all zeros if no audio arrived recently.
    pub fn snapshot(&self) -> Vec<f32> {
        if let Ok(out) = self.levels.lock() {
            if let Some(time) = out.1 {
                if time.elapsed() < Duration::from_millis(300) {
                    return out.0.clone();
                }
            }
        }
        vec![0.0; BANDS]
    }
}

fn analyze(window: &mut Window, rate: f64) -> Vec<f32> {
    // Unroll the ring buffer, oldest sample first, and apply the window.
    let start = window.write;
    for i in 0..N {
        let sample = window.ring[(start + i) % N] * window.hann[i];
        window.scratch[i] = Complex::new(sample, 0.0);
    }
    window.fft.process(&mut window.scratch);

    let half = N / 2;
    let mags: Vec<f32> = window.scratch[..half].iter().map(|c| c.norm()).collect();

    let f_min = 50.0f64;
    let f_max = 16_000.0f64;
    let rate = if rate > 0.0 { rate } else { 48_000.0 };
    let bin_width = rate / N as f64;
    let full_scale = N as f32 / 4.0; // a full-scale sine after a Hann window

    (0..BANDS)
        .map(|b| {
            let f0 = f_min * (f_max / f_min).powf(b as f64 / BANDS as f64);
            let f1 = f_min * (f_max / f_min).powf((b + 1) as f64 / BANDS as f64);
            let x0 = f0 / bin_width;
            let x1 = f1 / bin_width;
            let mag = if x1 - x0 < 1.5 {
                // Band narrower than a bin: interpolate so neighbouring bands differ.
                let centre = (x0 + x1) / 2.0;
                let i = (centre as usize).clamp(1, half - 2);
                let t = (centre - i as f64) as f32;
                mags[i] * (1.0 - t) + mags[i + 1] * t
            } else {
                let lo = (x0 as usize).clamp(1, half - 1);
                let hi = (x1 as usize).clamp(lo, half - 1);
                mags[lo..=hi].iter().cloned().fold(0.0, f32::max)
            };
            let mut db = 20.0 * (mag / full_scale).max(1e-7).log10();
            db += 14.0 * b as f32 / (BANDS - 1) as f32; // lift the treble, which is naturally quieter
            ((db + 65.0) / 55.0).clamp(0.0, 1.0)
        })
        .collect()
}
