// macOS: thin Rust wrappers around macos_shim.m, plus asking Spotify / Music for the cover.

use crate::analyzer::Analyzer;
use crate::platform::{download, Poll};
use std::ffi::{c_void, CString};
use std::os::raw::{c_char, c_int};
use std::process::Command;
use std::sync::Arc;

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct ScreenMetrics {
    pub bottom_inset: f64,
    pub dock_width: f64,
    pub dock_on_side: c_int,
}

extern "C" {
    fn aw_audio_start(callback: extern "C" fn(*mut c_void, *const f32, c_int, f64), context: *mut c_void);
    fn aw_system_volume() -> f32;
    fn aw_configure_window(view: *mut c_void);
    fn aw_set_dim(view: *mut c_void, alpha: f64);
    fn aw_present_stage(
        view: *mut c_void,
        rgba: *const u8,
        width: c_int,
        height: c_int,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    );
    fn aw_screen_metrics(view: *mut c_void, out: *mut ScreenMetrics);
    fn aw_app_running(bundle_id: *const c_char) -> c_int;
    fn aw_bring_to_front();
}

extern "C" fn on_audio(context: *mut c_void, samples: *const f32, count: c_int, rate: f64) {
    if context.is_null() || samples.is_null() || count <= 0 {
        return;
    }
    let analyzer = unsafe { &*(context as *const Analyzer) };
    let samples = unsafe { std::slice::from_raw_parts(samples, count as usize) };
    analyzer.push(samples, rate);
}

/// Start capturing system audio (ScreenCaptureKit). macOS asks for permission the first time.
pub fn start_audio(analyzer: Arc<Analyzer>) {
    // The capture runs for the life of the app, so the analyser is intentionally never freed.
    let context = Arc::into_raw(analyzer) as *mut c_void;
    unsafe { aw_audio_start(on_audio, context) }
}

pub fn system_volume() -> f32 {
    unsafe { aw_system_volume() }
}

/// Put the window just above the wallpaper, below the desktop icons, on every Space, click-through.
pub fn configure_window(view: *mut c_void) {
    unsafe { aw_configure_window(view) }
}

pub fn set_dim(view: *mut c_void, alpha: f32) {
    unsafe { aw_set_dim(view, alpha as f64) }
}

/// Show premultiplied RGBA pixels in the given rectangle (logical points, from the top-left).
pub fn present(view: *mut c_void, rgba: &[u8], width: u32, height: u32, x: f32, y: f32, w: f32, h: f32) {
    if rgba.len() < (width as usize) * (height as usize) * 4 {
        return;
    }
    unsafe {
        aw_present_stage(view, rgba.as_ptr(), width as c_int, height as c_int, x as f64, y as f64, w as f64, h as f64)
    }
}

pub fn screen_metrics(view: *mut c_void) -> ScreenMetrics {
    let mut metrics = ScreenMetrics::default();
    unsafe { aw_screen_metrics(view, &mut metrics) };
    metrics
}

pub fn bring_to_front() {
    unsafe { aw_bring_to_front() }
}

fn app_running(bundle_id: &str) -> bool {
    let Ok(id) = CString::new(bundle_id) else { return false };
    unsafe { aw_app_running(id.as_ptr()) != 0 }
}

fn run_script(source: &str, javascript: bool, args: &[&str]) -> Vec<String> {
    let mut command = Command::new("/usr/bin/osascript");
    if javascript {
        command.args(["-l", "JavaScript"]);
    }
    command.arg("-e").arg(source).args(args);
    match command.output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.to_string())
            .collect(),
        Err(_) => Vec::new(),
    }
}

const SPOTIFY: &str = r#"tell application "Spotify"
  if player state is playing then
    return (id of current track) & linefeed & (artwork url of current track)
  end if
end tell
return """#;

const MUSIC: &str = r#"on run argv
  set lastKey to item 1 of argv
  set artPath to item 2 of argv
  set k to ""
  set d to missing value
  tell application "Music"
    if player state is not playing then return ""
    set t to current track
    set k to (name of t) & " / " & (artist of t) & " / " & (album of t)
    if k is lastKey then return k
    try
      set d to raw data of artwork 1 of t
    end try
  end tell
  if d is missing value then return k
  try
    set f to open for access (POSIX file artPath) with write permission
    set eof f to 0
    write d to f
    close access f
    return k & linefeed & "ART"
  end try
  return k
end run"#;

const SYSTEM: &str = r#"function run(argv) {
  try {
    ObjC.import('Foundation');
    const bundle = $.NSBundle.bundleWithPath('/System/Library/PrivateFrameworks/MediaRemote.framework/');
    bundle.load;
    const Request = $.NSClassFromString('MRNowPlayingRequest');
    const item = Request.localNowPlayingItem;
    if (!item || item.isNil()) return '';
    const info = item.nowPlayingInfo;
    if (!info || info.isNil()) return '';
    const text = function (k) {
      const v = info.valueForKey(k);
      return (v && !v.isNil()) ? ObjC.unwrap(v.description) : '';
    };
    const key = (text('kMRMediaRemoteNowPlayingInfoTitle') + ' / ' +
                 text('kMRMediaRemoteNowPlayingInfoArtist') + ' / ' +
                 text('kMRMediaRemoteNowPlayingInfoAlbum')).replace(/\n/g, ' ');
    if (key === argv[0]) return key;
    const art = info.valueForKey('kMRMediaRemoteNowPlayingInfoArtworkData');
    if (!art || art.isNil()) return key;
    return key + '\n' + ObjC.unwrap(art.base64EncodedStringWithOptions(0));
  } catch (e) {
    return '';
  }
}"#;

/// Ask Spotify, then Apple Music, then the system's Now Playing info.
pub fn now_playing(last: &str) -> Poll {
    // Only talk to apps that are already open: asking a closed app would launch it.
    if app_running("com.spotify.client") {
        let lines = run_script(SPOTIFY, false, &[]);
        if lines.len() >= 2 {
            let key = lines[0].clone();
            if key == last {
                return Poll::Same;
            }
            return Poll::New { key, art: download(&lines[1]) };
        }
    }

    if app_running("com.apple.Music") {
        let art_path = std::env::temp_dir().join("audiowallpaper-art");
        let art_path_text = art_path.to_string_lossy().to_string();
        let lines = run_script(MUSIC, false, &[last, art_path_text.as_str()]);
        if let Some(key) = lines.first() {
            if key.as_str() == last {
                return Poll::Same;
            }
            let art = if lines.len() >= 2 { std::fs::read(&art_path).ok() } else { None };
            return Poll::New { key: key.clone(), art };
        }
    }

    let lines = run_script(SYSTEM, true, &[last]);
    match lines.first() {
        None => Poll::Nothing,
        Some(key) if key.replace(" / ", "").trim().is_empty() => Poll::Nothing,
        Some(key) if key.as_str() == last => Poll::Same,
        Some(key) => {
            let art = if lines.len() >= 2 { lines.last().and_then(|text| base64_decode(text)) } else { None };
            Poll::New { key: key.clone(), art }
        }
    }
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for &c in text.as_bytes() {
        if c == b'=' {
            break;
        }
        let Some(v) = value(c) else { continue };
        buffer = (buffer << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}
