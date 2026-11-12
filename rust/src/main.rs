// AudioWallpaper: a live audio visualiser that sits on the desktop, behind your icons.
//
// Run with no arguments to start the visualiser (it puts an icon in the menu bar / system tray).
// Run with `--settings` to open the settings window; the tray icon does this for you.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod analyzer;
mod audio;
mod config;
mod desktop;
mod engine;
mod nowplaying;
mod palette;
mod platform;
mod render;
mod settings;
mod shapes;
mod tray;
mod volume;
#[cfg(target_os = "linux")]
mod wayland;

fn main() {
    if std::env::args().any(|arg| arg == "--settings") {
        settings::run();
        return;
    }

    #[cfg(target_os = "linux")]
    {
        // Wayland desktops that support layer-shell (KDE, Sway, Hyprland...) get native surfaces.
        // Anything else (including GNOME) falls back to X11 / XWayland.
        if std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var_os("AUDIOWALLPAPER_X11").is_none() {
            match wayland::run() {
                Ok(()) => return,
                Err(reason) => {
                    eprintln!("AudioWallpaper: {reason}; falling back to X11.");
                    std::env::remove_var("WAYLAND_DISPLAY");
                }
            }
        }
    }

    desktop::run();
}
