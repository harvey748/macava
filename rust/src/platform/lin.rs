// Linux: volume from PipeWire / PulseAudio, and what's playing over MPRIS (Spotify, browsers, most players).

use crate::platform::{download, Poll};
use std::process::Command;

pub fn system_volume() -> Option<f32> {
    // PipeWire: "Volume: 0.40" or "Volume: 0.40 [MUTED]"
    if let Ok(output) = Command::new("wpctl").args(["get-volume", "@DEFAULT_AUDIO_SINK@"]).output() {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            if text.contains("[MUTED]") {
                return Some(0.0);
            }
            if let Some(volume) = text.split_whitespace().nth(1).and_then(|v| v.parse::<f32>().ok()) {
                return Some(volume.min(1.0));
            }
        }
    }
    // PulseAudio: "Mute: yes" and "Volume: front-left: 26214 /  40% / -23.88 dB, ..."
    let mute = Command::new("pactl").args(["get-sink-mute", "@DEFAULT_SINK@"]).output().ok()?;
    if String::from_utf8_lossy(&mute.stdout).contains("yes") {
        return Some(0.0);
    }
    let volume = Command::new("pactl").args(["get-sink-volume", "@DEFAULT_SINK@"]).output().ok()?;
    let text = String::from_utf8_lossy(&volume.stdout);
    let percent = text.split('/').nth(1)?.trim().trim_end_matches('%').trim().parse::<f32>().ok()?;
    Some((percent / 100.0).min(1.0))
}

pub fn now_playing(last: &str) -> Poll {
    let Ok(finder) = mpris::PlayerFinder::new() else { return Poll::Nothing };
    let Ok(player) = finder.find_active() else { return Poll::Nothing };
    match player.get_playback_status() {
        Ok(mpris::PlaybackStatus::Playing) => {}
        _ => return Poll::Nothing,
    }
    let Ok(metadata) = player.get_metadata() else { return Poll::Nothing };
    let key = format!(
        "{} / {} / {}",
        metadata.title().unwrap_or(""),
        metadata.artists().map(|artists| artists.join(", ")).unwrap_or_default(),
        metadata.album_name().unwrap_or("")
    );
    if key == last {
        return Poll::Same;
    }
    let art = metadata.art_url().and_then(download);
    Poll::New { key, art }
}
