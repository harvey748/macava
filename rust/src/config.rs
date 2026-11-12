// Settings shared between the visualiser and the settings window, stored as JSON.
// The settings window writes the file; the visualiser notices the change and applies it.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Style {
    Wave,
    Bars,
    Shape,
    Cover,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Shape {
    Sphere,
    Cube,
    Donut,
    Diamond,
    Helix,
}

impl Shape {
    pub const ALL: [Shape; 5] = [Shape::Sphere, Shape::Cube, Shape::Donut, Shape::Diamond, Shape::Helix];

    pub fn title(self) -> &'static str {
        match self {
            Shape::Sphere => "Sphere",
            Shape::Cube => "Cube",
            Shape::Donut => "Donut",
            Shape::Diamond => "Diamond",
            Shape::Helix => "Double Helix",
        }
    }

    pub fn index(self) -> usize {
        Shape::ALL.iter().position(|&s| s == self).unwrap_or(0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub style: Style,
    pub shape: Shape,
    /// Small copies of the 3D shape circling the main one (0-8).
    pub orbiters: u32,
    /// true: bass in the middle; false: bass on the left, treble on the right.
    pub mirrored: bool,
    pub monstercat: bool,
    /// Album cover dots wander with the music (true) or only grow (false).
    pub cover_motion: bool,
    /// Dots across the album cover (10-96).
    pub cover_resolution: u32,
    /// Width of the wave and bars compared with the Dock / taskbar (0.5-1.3).
    pub wave_width: f32,
    /// Size of the 3D shape and the album cover (0.4-2.5).
    pub shape_size: f32,
    /// How much to darken the wallpaper (0-0.95).
    pub dim: f32,
    /// Extra lift above the Dock / taskbar, in points.
    pub bottom_offset: f32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            style: Style::Wave,
            shape: Shape::Sphere,
            orbiters: 0,
            mirrored: true,
            monstercat: false,
            cover_motion: true,
            cover_resolution: 40,
            wave_width: 1.0,
            shape_size: 1.0,
            dim: 0.0,
            bottom_offset: 0.0,
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("AudioWallpaper")
            .join("config.json")
    }

    pub fn load() -> Config {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|text| serde_json::from_str::<Config>(&text).ok())
            .unwrap_or_default()
            .sanitized()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            // Write then rename, so the visualiser never reads a half-written file.
            let temp = path.with_extension("json.tmp");
            if std::fs::write(&temp, text).is_ok() {
                let _ = std::fs::rename(&temp, &path);
            }
        }
    }

    pub fn modified() -> Option<SystemTime> {
        std::fs::metadata(Self::path()).and_then(|m| m.modified()).ok()
    }

    fn sanitized(mut self) -> Config {
        self.orbiters = self.orbiters.min(8);
        self.cover_resolution = self.cover_resolution.clamp(10, 96);
        self.wave_width = self.wave_width.clamp(0.5, 1.3);
        self.shape_size = self.shape_size.clamp(0.4, 2.5);
        self.dim = self.dim.clamp(0.0, 0.95);
        self.bottom_offset = self.bottom_offset.clamp(0.0, 400.0);
        self
    }
}
