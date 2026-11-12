// Keeps an up-to-date copy of the system output volume (0..1, 0 when muted) from a background thread.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct Volume {
    level: Arc<AtomicU32>,
}

impl Volume {
    pub fn start() -> Volume {
        let level = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let writer = level.clone();
        std::thread::spawn(move || {
            #[cfg(target_os = "windows")]
            crate::platform::win::init_com();
            let pause = Duration::from_millis(if cfg!(target_os = "linux") { 600 } else { 200 });
            loop {
                let value = crate::platform::system_volume().unwrap_or(1.0).clamp(0.0, 1.0);
                writer.store(value.to_bits(), Ordering::Relaxed);
                std::thread::sleep(pause);
            }
        });
        Volume { level }
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
}
