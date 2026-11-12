// Everything that isn't platform-specific: settings, levels, colours, timing and where things go.
// Each backend calls `tick()` once per frame and `render()` once per screen.

use crate::analyzer::{Analyzer, BANDS};
use crate::config::{Config, Style};
use crate::nowplaying::{Artwork, NowPlaying};
use crate::palette::Rgba;
use crate::volume::Volume;
use std::f32::consts::TAU;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

/// Colour of the resting line / dashes while nothing plays.
pub const RESTING: Rgba = [1.0, 1.0, 1.0, 0.55];

pub const DEFAULT_PALETTE: [Rgba; 5] = [
    [1.00, 0.55, 0.35, 1.0],
    [0.95, 0.35, 0.85, 1.0],
    [0.30, 0.75, 1.00, 1.0],
    [0.95, 0.35, 0.85, 1.0],
    [1.00, 0.55, 0.35, 1.0],
];

/// Space around the wave inside its drawing area.
pub const WAVE_PAD: f32 = 4.0;
/// How far below the Dock / taskbar edge the drawing area reaches, so the line isn't clipped.
pub const BASELINE_GAP: f32 = 3.0;

/// One monitor, in logical points.
#[derive(Clone, Copy, Debug)]
pub struct ScreenInfo {
    pub width: f32,
    pub height: f32,
    /// Height of the Dock / taskbar along the bottom (0 if it's elsewhere or hidden).
    pub bottom_inset: f32,
    /// Estimated Dock width on macOS; elsewhere the wave spans the screen.
    pub dock_width: Option<f32>,
}

/// The part of the screen the current style draws into, in logical points from the top-left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StageRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

pub struct TickResult {
    pub redraw: bool,
}

pub struct Engine {
    pub config: Config,
    config_stamp: Option<SystemTime>,
    analyzer: Arc<Analyzer>,
    volume: Volume,
    now_playing: NowPlaying,
    pub art_gen: u64,
    pub art: Option<Arc<Artwork>>,
    pub display: Vec<f32>,
    /// Levels after smoothing, shared by every style.
    pub soft: Vec<f32>,
    pub active: bool,
    /// 0 = resting (colourless), 1 = playing (full colour); eases between the two.
    pub mix: f32,
    pub palette: [Rgba; 5],
    palette_target: [Rgba; 5],
    pub vol_scale: f32,
    vol_target: f32,
    pub orb_angle: f32,
    pub orbit_angle: f32,
    pub cover_clock: f32,
    pub energy: f32,
    pub bass: f32,
    pub cover_cache: Option<(u64, u32, Arc<Vec<[f32; 3]>>)>,
    frame: u64,
    last: Instant,
    idle: bool,
}

impl Engine {
    pub fn new(analyzer: Arc<Analyzer>) -> Engine {
        Engine {
            config: Config::load(),
            config_stamp: Config::modified(),
            analyzer,
            volume: Volume::start(),
            now_playing: NowPlaying::start(),
            art_gen: 0,
            art: None,
            display: vec![0.0; BANDS],
            soft: vec![0.0; BANDS],
            active: false,
            mix: 0.0,
            palette: DEFAULT_PALETTE,
            palette_target: DEFAULT_PALETTE,
            vol_scale: 1.0,
            vol_target: 1.0,
            orb_angle: 0.0,
            orbit_angle: 0.0,
            cover_clock: 0.0,
            energy: 0.0,
            bass: 0.0,
            cover_cache: None,
            frame: 0,
            last: Instant::now(),
            idle: false,
        }
    }

    pub fn tick(&mut self) -> TickResult {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.05);
        self.last = now;
        self.frame += 1;
        let mut changed = self.frame == 1;

        // Settings written by the settings window.
        if self.frame % 30 == 0 {
            let stamp = Config::modified();
            if stamp != self.config_stamp {
                self.config_stamp = stamp;
                let config = Config::load();
                if config != self.config {
                    self.config = config;
                    self.cover_cache = None;
                    changed = true;
                }
            }
        }

        // Louder volume, taller wave.
        if self.frame % 10 == 0 {
            self.vol_target = self.volume.get().powf(0.8);
        }
        self.vol_scale += (self.vol_target - self.vol_scale) * 0.15;

        // New album cover.
        if let Some((generation, art)) = self.now_playing.newer_than(self.art_gen) {
            self.art_gen = generation;
            self.palette_target = art.as_ref().map(|a| expand(&a.palette)).unwrap_or(DEFAULT_PALETTE);
            self.art = art;
            self.cover_cache = None;
            changed = true;
        }

        // Levels: snap up, drop fast.
        let target = self.analyzer.snapshot();
        let mut peak = 0.0f32;
        for i in 0..BANDS {
            let t = target.get(i).copied().unwrap_or(0.0);
            let d = &mut self.display[i];
            if t > *d {
                *d += (t - *d) * 0.85;
            } else {
                *d += (t - *d) * 0.22;
            }
            peak = peak.max(*d);
        }

        // Colour only while there is sound.
        let now_active = if self.active { peak > 0.015 } else { peak > 0.04 };
        if now_active && !self.active {
            self.now_playing.poke();
        }
        self.active = now_active;
        self.now_playing.set_active(now_active);

        let target_mix = if self.active { 1.0 } else { 0.0 };
        let rate = if self.active { 0.12 } else { 0.05 };
        self.mix += (target_mix - self.mix) * rate;
        if (target_mix - self.mix).abs() < 0.002 {
            self.mix = target_mix;
        }

        let mut palette_moving = false;
        for i in 0..5 {
            for c in 0..4 {
                let d = self.palette_target[i][c] - self.palette[i][c];
                if d.abs() > 0.002 {
                    self.palette[i][c] += d * 0.06;
                    palette_moving = true;
                } else {
                    self.palette[i][c] = self.palette_target[i][c];
                }
            }
        }

        self.compute_soft();
        self.energy = self.soft.iter().sum::<f32>() / BANDS as f32;
        self.bass = self.soft[..6].iter().sum::<f32>() / 6.0;

        // Motion: spins faster as the music gets busier.
        self.orb_angle = (self.orb_angle + dt * (0.35 + self.energy * 1.6)) % TAU;
        self.orbit_angle = (self.orbit_angle - dt * (0.45 + self.energy * 1.0)) % TAU;
        self.cover_clock += dt * (1.0 + self.energy * 3.0);

        // Nothing playing and nothing changing: draw the resting state once, then stop.
        let settled = !palette_moving && (self.mix == 0.0 || self.mix == 1.0);
        let still = peak < 0.003 && settled && !changed && self.config.style != Style::Shape;
        let redraw = if still {
            let first = !self.idle;
            self.idle = true;
            first
        } else {
            self.idle = false;
            true
        };
        TickResult { redraw }
    }

    fn compute_soft(&mut self) {
        let n = BANDS;
        let d = &self.display;
        let soft = &mut self.soft;
        for i in 0..n {
            let a = d[i.saturating_sub(1)];
            let b = d[i];
            let c = d[(i + 1).min(n - 1)];
            let mixed = 0.15 * a + 0.7 * b + 0.15 * c;
            // Exaggerate the difference between loud and quiet so peaks punch upwards.
            soft[i] = (mixed.max(0.0).powf(1.7) * 1.2).min(1.1);
        }
        // The very lowest bands are often empty; stop them carving a notch.
        soft[1] = soft[1].max(soft[2]);
        soft[0] = soft[0].max(soft[1]);

        // Monstercat smoothing: loud bands lift their neighbours into rounded hills.
        if self.config.monstercat {
            for i in 0..n {
                let mut falloff = 1.0f32;
                for step in 1..n {
                    falloff *= 1.5;
                    let spread = soft[i] / falloff;
                    if spread < 0.01 {
                        break;
                    }
                    if i >= step {
                        soft[i - step] = soft[i - step].max(spread);
                    }
                    if i + step < n {
                        soft[i + step] = soft[i + step].max(spread);
                    }
                }
            }
        }
    }

    /// Where the current style draws on this screen.
    pub fn stage_rect(&self, screen: &ScreenInfo) -> StageRect {
        let usable_bottom = (screen.height - screen.bottom_inset - self.config.bottom_offset).max(screen.height * 0.3);
        let centre_x = screen.width / 2.0;
        let centre_y = (screen.height - screen.bottom_inset) / 2.0;
        let square = |side: f32| StageRect {
            x: centre_x - side / 2.0,
            y: centre_y - side / 2.0,
            w: side.max(1.0),
            h: side.max(1.0),
        };
        match self.config.style {
            Style::Wave | Style::Bars => {
                let span = (screen.dock_width.unwrap_or(screen.width) * self.config.wave_width).min(screen.width);
                let w = (span + 2.0 * WAVE_PAD).min(screen.width);
                let h = (screen.height * 0.46 + 6.0).min(usable_bottom);
                StageRect {
                    x: ((screen.width - w) / 2.0).max(0.0),
                    y: usable_bottom - (h - BASELINE_GAP),
                    w,
                    h,
                }
            }
            Style::Shape => {
                let radius = shape_radius(&self.config, screen) * 1.1;
                let half = radius * if self.config.orbiters > 0 { 3.9 } else { 2.4 };
                square(half * 2.0)
            }
            Style::Cover => {
                let side = cover_side(&self.config, screen);
                let cell = side / self.config.cover_resolution.max(1) as f32;
                square(side * 1.3 + cell * 6.0)
            }
        }
    }
}

/// Radius of the 3D shape before it pulses.
pub fn shape_radius(config: &Config, screen: &ScreenInfo) -> f32 {
    screen.width.min(screen.height - screen.bottom_inset) * 0.17 * config.shape_size
}

/// Width of the album cover before the bass pushes it outward.
pub fn cover_side(config: &Config, screen: &ScreenInfo) -> f32 {
    (screen.width.min(screen.height - screen.bottom_inset) * 0.42 * config.shape_size).round()
}

/// Stretch a 1-5 colour palette to five evenly spaced stops.
fn expand(palette: &[Rgba]) -> [Rgba; 5] {
    if palette.is_empty() {
        return DEFAULT_PALETTE;
    }
    let mut out = [[0.0; 4]; 5];
    let last = palette.len() - 1;
    for (i, stop) in out.iter_mut().enumerate() {
        let t = i as f32 / 4.0 * last as f32;
        let a = (t.floor() as usize).min(last);
        let b = (a + 1).min(last);
        let f = t - a as f32;
        for c in 0..4 {
            stop[c] = palette[a][c] + (palette[b][c] - palette[a][c]) * f;
        }
    }
    out
}
