# AudioWallpaper (Rust)

A cross-platform port of the macOS Swift app: a live audio visualiser that sits on your desktop,
behind your icons, and reacts to whatever your computer is playing.

Styles: **Wave**, **Bars**, **3D Shape** (sphere, cube, donut, diamond, double helix, with orbiting
shapes) and **Album Cover** (the current cover drawn in dots). Colours follow the album cover, height
follows your volume, and the wallpaper can be dimmed.

## Running

Start the app and it puts an icon in the menu bar / system tray. Use **Settings…** there to open the
settings window; changes apply straight away. **Quit** stops it.

| | How it captures sound | Where it draws |
|---|---|---|
| macOS 13+ | System audio via ScreenCaptureKit (asks for Screen & System Audio Recording permission) | Above the wallpaper, below the icons, on every Space |
| Windows 10/11 | WASAPI loopback of the default output | Inside the desktop, behind the icons (draws your wallpaper underneath) |
| Linux, KDE / Sway / Hyprland (Wayland) | PulseAudio / PipeWire monitor | Layer-shell surfaces above the wallpaper |
| Linux, X11 (and GNOME, via XWayland) | PulseAudio / PipeWire monitor | A transparent desktop window (needs a compositor for transparency) |

Album covers come from Spotify / Apple Music on macOS, the Windows media controls on Windows, and
MPRIS on Linux.

Settings are stored in `config.json` in your config folder (`~/Library/Application Support/AudioWallpaper`,
`%APPDATA%\AudioWallpaper` or `~/.config/AudioWallpaper`).

## Building

Builds for all three platforms are made by GitHub Actions (`.github/workflows/rust.yml`) on every push
that touches `rust/`, and attached to a release when you push a `v*` tag.

To build locally you need Rust (https://rustup.rs):

```sh
cd rust
cargo build --release
```

On Linux you also need: `pkg-config libasound2-dev libdbus-1-dev libxkbcommon-dev libwayland-dev`.

On macOS the binary must run from an app bundle to get the audio permission; see the macOS step in
the workflow for how the `.app` is assembled.
