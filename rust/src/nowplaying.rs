// Watches what's playing (in a background thread) and turns the album cover into colours and a dot grid.

use crate::palette::{self, Rgba};
use crate::platform::{self, Poll};
use image::RgbaImage;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct Artwork {
    pub palette: Vec<Rgba>,
    pub image: RgbaImage,
}

struct Shared {
    generation: u64,
    art: Option<Arc<Artwork>>,
}

pub struct NowPlaying {
    shared: Arc<Mutex<Shared>>,
    active: Arc<AtomicBool>,
    poke: Arc<AtomicBool>,
}

impl NowPlaying {
    pub fn start() -> NowPlaying {
        let shared = Arc::new(Mutex::new(Shared { generation: 0, art: None }));
        let active = Arc::new(AtomicBool::new(false));
        let poke = Arc::new(AtomicBool::new(false));
        let (thread_shared, thread_active, thread_poke) = (shared.clone(), active.clone(), poke.clone());

        std::thread::spawn(move || {
            #[cfg(target_os = "windows")]
            platform::win::init_com();
            let mut last = String::new();
            let mut last_check: Option<Instant> = None;
            loop {
                let poked = thread_poke.swap(false, Ordering::SeqCst);
                let due = poked || last_check.map_or(true, |t| t.elapsed() >= Duration::from_secs(4));
                // Only ask while sound is playing: checking costs a little on every platform.
                if due && thread_active.load(Ordering::SeqCst) {
                    last_check = Some(Instant::now());
                    match platform::now_playing(&last) {
                        Poll::Nothing => {
                            if !last.is_empty() {
                                last.clear();
                                publish(&thread_shared, None);
                            }
                        }
                        Poll::Same => {}
                        Poll::New { key, art: Some(bytes) } => {
                            // If the cover can't be read, leave `last` alone so it's tried again.
                            if let Some(artwork) = decode(&bytes) {
                                last = key;
                                publish(&thread_shared, Some(Arc::new(artwork)));
                            }
                        }
                        Poll::New { key, art: None } => {
                            last = key;
                            publish(&thread_shared, None);
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        });

        NowPlaying { shared, active, poke }
    }

    pub fn set_active(&self, on: bool) {
        self.active.store(on, Ordering::SeqCst);
    }

    /// Check straight away instead of waiting for the next regular check.
    pub fn poke(&self) {
        self.poke.store(true, Ordering::SeqCst);
    }

    /// The current cover, if it changed since `generation`.
    pub fn newer_than(&self, generation: u64) -> Option<(u64, Option<Arc<Artwork>>)> {
        let shared = self.shared.lock().ok()?;
        if shared.generation != generation {
            Some((shared.generation, shared.art.clone()))
        } else {
            None
        }
    }
}

fn publish(shared: &Mutex<Shared>, art: Option<Arc<Artwork>>) {
    if let Ok(mut s) = shared.lock() {
        s.generation += 1;
        s.art = art;
    }
}

fn decode(bytes: &[u8]) -> Option<Artwork> {
    let image = image::load_from_memory(bytes).ok()?.to_rgba8();
    let image = if image.width() > 256 || image.height() > 256 {
        image::imageops::resize(&image, 256, 256, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let palette = palette::palette(&image);
    Some(Artwork { palette, image })
}
