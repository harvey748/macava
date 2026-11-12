// Captures what the computer is playing (not the microphone) and feeds it to the analyser.
//   macOS:   ScreenCaptureKit system audio (see platform/macos_shim.m)
//   Windows: WASAPI loopback of the default output device
//   Linux:   the PulseAudio / PipeWire monitor of the default output

use crate::analyzer::Analyzer;
use std::sync::Arc;

pub fn start(analyzer: Arc<Analyzer>) {
    #[cfg(target_os = "macos")]
    crate::platform::mac::start_audio(analyzer);
    #[cfg(not(target_os = "macos"))]
    loopback::start(analyzer);
}

#[cfg(not(target_os = "macos"))]
mod loopback {
    use crate::analyzer::Analyzer;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    pub fn start(analyzer: Arc<Analyzer>) {
        #[cfg(target_os = "linux")]
        {
            // Ask PulseAudio / PipeWire for the monitor of whatever is playing, not the microphone.
            std::env::set_var("PULSE_SOURCE", "@DEFAULT_MONITOR@");
        }
        std::thread::spawn(move || loop {
            match open(&analyzer) {
                Ok((stream, failed, name)) => {
                    let _ = stream.play();
                    loop {
                        std::thread::sleep(Duration::from_secs(2));
                        if failed.load(Ordering::SeqCst) || device_changed(&name) {
                            break;
                        }
                    }
                    drop(stream);
                }
                Err(error) => {
                    eprintln!("AudioWallpaper: can't capture audio yet ({error}); retrying.");
                    std::thread::sleep(Duration::from_secs(5));
                }
            }
        });
    }

    fn device() -> Option<cpal::Device> {
        let host = cpal::default_host();
        #[cfg(target_os = "windows")]
        let device = host.default_output_device();
        #[cfg(not(target_os = "windows"))]
        let device = host
            .input_devices()
            .ok()
            .and_then(|mut devices| devices.find(|d| matches!(d.name().as_deref(), Ok("pulse") | Ok("pipewire"))))
            .or_else(|| host.default_input_device());
        device
    }

    /// On Windows, follow the default output (e.g. when headphones are plugged in).
    fn device_changed(name: &str) -> bool {
        if cfg!(target_os = "windows") {
            match device().and_then(|d| d.name().ok()) {
                Some(current) => current != name,
                None => false,
            }
        } else {
            false
        }
    }

    fn open(analyzer: &Arc<Analyzer>) -> Result<(cpal::Stream, Arc<AtomicBool>, String), String> {
        let device = device().ok_or("no audio device")?;
        let name = device.name().unwrap_or_default();
        // On Windows an input stream on an output device records what it plays (loopback).
        #[cfg(target_os = "windows")]
        let supported = device.default_output_config().map_err(|e| e.to_string())?;
        #[cfg(not(target_os = "windows"))]
        let supported = device.default_input_config().map_err(|e| e.to_string())?;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let failed = Arc::new(AtomicBool::new(false));
        let stream = match format {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config, analyzer.clone(), failed.clone()),
            cpal::SampleFormat::I16 => build::<i16>(&device, &config, analyzer.clone(), failed.clone()),
            cpal::SampleFormat::U16 => build::<u16>(&device, &config, analyzer.clone(), failed.clone()),
            cpal::SampleFormat::I32 => build::<i32>(&device, &config, analyzer.clone(), failed.clone()),
            other => return Err(format!("unsupported sample format {other:?}")),
        }
        .map_err(|e| e.to_string())?;
        Ok((stream, failed, name))
    }

    fn build<T>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        analyzer: Arc<Analyzer>,
        failed: Arc<AtomicBool>,
    ) -> Result<cpal::Stream, cpal::BuildStreamError>
    where
        T: cpal::SizedSample,
        f32: cpal::FromSample<T>,
    {
        let channels = (config.channels as usize).max(1);
        let rate = config.sample_rate.0 as f64;
        let mut mono: Vec<f32> = Vec::new();
        device.build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                mono.clear();
                for frame in data.chunks(channels) {
                    let sum: f32 = frame.iter().map(|&s| <f32 as cpal::FromSample<T>>::from_sample_(s)).sum();
                    mono.push(sum / channels as f32);
                }
                analyzer.push(&mono, rate);
            },
            move |error| {
                eprintln!("AudioWallpaper: audio stream error: {error}");
                failed.store(true, Ordering::SeqCst);
            },
            None,
        )
    }
}
