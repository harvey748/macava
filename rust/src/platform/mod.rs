// Per-operating-system pieces: system volume, what's playing, and desktop helpers.

#[cfg(target_os = "linux")]
pub mod lin;
#[cfg(target_os = "macos")]
pub mod mac;
#[cfg(target_os = "windows")]
pub mod win;

/// Result of asking the system what's playing.
pub enum Poll {
    /// Nothing is playing.
    Nothing,
    /// Same track as last time.
    Same,
    /// A new track, with its cover image file (PNG/JPEG...) if one could be fetched.
    New { key: String, art: Option<Vec<u8>> },
}

/// Output volume 0..1 (0 when muted); None if it can't be read.
pub fn system_volume() -> Option<f32> {
    #[cfg(target_os = "macos")]
    let volume = Some(mac::system_volume());
    #[cfg(target_os = "windows")]
    let volume = win::system_volume();
    #[cfg(target_os = "linux")]
    let volume = lin::system_volume();
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let volume = None;
    volume
}

/// What's playing now, compared with the track key from last time.
pub fn now_playing(last: &str) -> Poll {
    #[cfg(target_os = "macos")]
    let poll = mac::now_playing(last);
    #[cfg(target_os = "windows")]
    let poll = win::now_playing(last);
    #[cfg(target_os = "linux")]
    let poll = lin::now_playing(last);
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let poll = {
        let _ = last;
        Poll::Nothing
    };
    poll
}

/// Fetch a cover image from a web address or a file:// path.
#[allow(dead_code)]
pub fn download(url: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::read(percent_decode(path)).ok();
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }
    let response = ureq::get(url).timeout(std::time::Duration::from_secs(8)).call().ok()?;
    let mut bytes = Vec::new();
    response.into_reader().take(20 * 1024 * 1024).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
