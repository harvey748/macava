// The menu bar / system tray icon: "Settings…" and "Quit".

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayAction {
    None,
    Quit,
}

/// Open the settings window (the same program, run with --settings).
pub fn open_settings() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg("--settings").spawn();
    }
}

/// A small waveform icon: five bars of different heights.
#[allow(dead_code)]
fn icon_rgba(size: u32, color: [u8; 4]) -> Vec<u8> {
    let heights = [0.35f32, 0.7, 1.0, 0.6, 0.3];
    let mut pixels = vec![0u8; (size * size * 4) as usize];
    let bar = size as f32 / 9.0;
    for (i, h) in heights.iter().enumerate() {
        let x0 = bar * (1.0 + i as f32 * 1.6);
        let bar_h = h * size as f32 * 0.8;
        let y0 = (size as f32 - bar_h) / 2.0;
        for y in 0..size {
            for x in 0..size {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                if fx >= x0 && fx < x0 + bar && fy >= y0 && fy < y0 + bar_h {
                    let index = ((y * size + x) * 4) as usize;
                    pixels[index..index + 4].copy_from_slice(&color);
                }
            }
        }
    }
    pixels
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod imp {
    use super::{icon_rgba, open_settings, TrayAction};
    use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    pub struct Tray {
        _icon: TrayIcon,
        settings: MenuId,
        quit: MenuId,
    }

    impl Tray {
        /// Must be called on the main thread once the event loop is running.
        pub fn new() -> Option<Tray> {
            let menu = Menu::new();
            let settings = MenuItem::new("Settings…", true, None);
            let quit = MenuItem::new("Quit AudioWallpaper", true, None);
            menu.append_items(&[&settings, &PredefinedMenuItem::separator(), &quit]).ok()?;

            let color = if cfg!(target_os = "macos") { [0, 0, 0, 255] } else { [120, 200, 255, 255] };
            let icon = Icon::from_rgba(icon_rgba(32, color), 32, 32).ok()?;
            let builder = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip("AudioWallpaper")
                .with_icon(icon);
            #[cfg(target_os = "macos")]
            let builder = builder.with_icon_as_template(true);
            let tray = builder.build().ok()?;
            Some(Tray { _icon: tray, settings: settings.id().clone(), quit: quit.id().clone() })
        }

        pub fn poll(&self) -> TrayAction {
            while let Ok(event) = MenuEvent::receiver().try_recv() {
                if event.id == self.settings {
                    open_settings();
                } else if event.id == self.quit {
                    return TrayAction::Quit;
                }
            }
            TrayAction::None
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{open_settings, TrayAction};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    // A StatusNotifierItem: shows in KDE, Xfce, Cinnamon, waybar and GNOME with the AppIndicator extension.
    struct Item {
        quit: Arc<AtomicBool>,
    }

    impl ksni::Tray for Item {
        fn id(&self) -> String {
            "audiowallpaper".into()
        }
        fn title(&self) -> String {
            "AudioWallpaper".into()
        }
        fn icon_name(&self) -> String {
            "audio-x-generic".into()
        }
        fn activate(&mut self, _x: i32, _y: i32) {
            open_settings();
        }
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::StandardItem;
            vec![
                StandardItem { label: "Settings…".into(), activate: Box::new(|_: &mut Self| open_settings()), ..Default::default() }
                    .into(),
                ksni::MenuItem::Separator,
                StandardItem {
                    label: "Quit AudioWallpaper".into(),
                    activate: Box::new(|item: &mut Self| item.quit.store(true, Ordering::SeqCst)),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    pub struct Tray {
        quit: Arc<AtomicBool>,
    }

    impl Tray {
        pub fn new() -> Option<Tray> {
            let quit = Arc::new(AtomicBool::new(false));
            let service = ksni::TrayService::new(Item { quit: quit.clone() });
            service.spawn();
            Some(Tray { quit })
        }

        pub fn poll(&self) -> TrayAction {
            if self.quit.load(Ordering::SeqCst) {
                TrayAction::Quit
            } else {
                TrayAction::None
            }
        }
    }
}

pub use imp::Tray;
