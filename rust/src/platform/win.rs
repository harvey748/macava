// Windows: put the window behind the desktop icons, read the wallpaper, volume and what's playing.

use crate::platform::Poll;
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, FindWindowW, GetWindowLongPtrW, GetWindowRect, SendMessageTimeoutW, SetParent,
    SetWindowLongPtrW, SetWindowPos, SystemParametersInfoW, GWL_STYLE, SMTO_NORMAL, SPI_GETDESKWALLPAPER,
    SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW, WS_CHILD, WS_POPUP,
};

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Start COM on the current thread (needed for volume and media info).
pub fn init_com() {
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

/// Height of the taskbar along the bottom of this window's monitor, in physical pixels.
pub fn bottom_inset(hwnd: HWND) -> i32 {
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) != 0 {
            (info.rcMonitor.bottom - info.rcWork.bottom).max(0)
        } else {
            0
        }
    }
}

unsafe extern "system" fn find_workerw(top: HWND, lparam: LPARAM) -> BOOL {
    let defview_class = wide("SHELLDLL_DefView");
    let workerw_class = wide("WorkerW");
    let defview = FindWindowExW(top, null_mut(), defview_class.as_ptr(), null());
    if !defview.is_null() {
        let workerw = FindWindowExW(null_mut(), top, workerw_class.as_ptr(), null());
        *(lparam as *mut HWND) = workerw;
    }
    1
}

/// Re-parent the window into the desktop so it sits behind the icons, like a live wallpaper.
/// Returns false if the desktop windows couldn't be found (the window then stays a normal bottom window).
pub fn attach_to_desktop(hwnd: HWND) -> bool {
    unsafe {
        let progman_class = wide("Progman");
        let progman = FindWindowW(progman_class.as_ptr(), null());
        if progman.is_null() {
            return false;
        }
        // Ask Explorer to create the WorkerW layer that sits between the wallpaper and the icons.
        let mut result: usize = 0;
        SendMessageTimeoutW(progman, 0x052C, 0xD, 0x1, SMTO_NORMAL, 1000, &mut result);
        SendMessageTimeoutW(progman, 0x052C, 0, 0, SMTO_NORMAL, 1000, &mut result);

        let mut screen_rect: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut screen_rect);

        let defview_class = wide("SHELLDLL_DefView");
        let defview = FindWindowExW(progman, null_mut(), defview_class.as_ptr(), null());
        let (parent, insert_after) = if !defview.is_null() {
            // Windows 11 24H2 and later: the icons live directly inside Progman. Go just below them.
            (progman, defview)
        } else {
            // Earlier versions: a separate WorkerW window behind the icons.
            let mut workerw: HWND = null_mut();
            EnumWindows(Some(find_workerw), &mut workerw as *mut HWND as LPARAM);
            if workerw.is_null() {
                return false;
            }
            (workerw, null_mut())
        };

        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        SetWindowLongPtrW(hwnd, GWL_STYLE, (style & !(WS_POPUP as isize)) | WS_CHILD as isize);
        if SetParent(hwnd, parent).is_null() {
            return false;
        }

        let mut parent_rect: RECT = std::mem::zeroed();
        GetWindowRect(parent, &mut parent_rect);
        let mut flags = SWP_NOACTIVATE | SWP_SHOWWINDOW;
        if insert_after.is_null() {
            flags |= SWP_NOZORDER;
        }
        SetWindowPos(
            hwnd,
            insert_after,
            screen_rect.left - parent_rect.left,
            screen_rect.top - parent_rect.top,
            screen_rect.right - screen_rect.left,
            screen_rect.bottom - screen_rect.top,
            flags,
        );
        true
    }
}

fn wallpaper_path() -> Option<String> {
    let mut buffer = [0u16; 520];
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETDESKWALLPAPER, buffer.len() as u32, buffer.as_mut_ptr() as *mut c_void, 0)
    };
    if ok == 0 {
        return None;
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    let path = String::from_utf16_lossy(&buffer[..len]);
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

/// The current wallpaper, scaled to fill w x h. We draw it ourselves because our window covers the real one.
pub fn wallpaper(w: u32, h: u32) -> image::RgbaImage {
    let decoded = wallpaper_path().and_then(|path| {
        image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()
    });
    match decoded {
        Some(image) => crate::palette::fill(&image.to_rgba8(), w, h),
        None => image::RgbaImage::from_pixel(w.max(1), h.max(1), image::Rgba([0, 0, 0, 255])),
    }
}

pub fn system_volume() -> Option<f32> {
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let endpoint: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None).ok()?;
        if endpoint.GetMute().ok()?.as_bool() {
            return Some(0.0);
        }
        endpoint.GetMasterVolumeLevelScalar().ok()
    }
}

/// What's playing, from the media controls Windows shows on the lock screen and in quick settings.
pub fn now_playing(last: &str) -> Poll {
    use windows::core::Interface;
    use windows::Foundation::IAsyncOperation;
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSessionManager as Manager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
    };
    use windows::Storage::Streams::DataReader;

    let result = (|| -> windows::core::Result<Poll> {
        let manager = Manager::RequestAsync()?.get()?;
        let Ok(session) = manager.GetCurrentSession() else { return Ok(Poll::Nothing) };
        if session.GetPlaybackInfo()?.PlaybackStatus()? != Status::Playing {
            return Ok(Poll::Nothing);
        }
        let properties = session.TryGetMediaPropertiesAsync()?.get()?;
        let key = format!("{} / {} / {}", properties.Title()?, properties.Artist()?, properties.AlbumTitle()?);
        if key == last {
            return Ok(Poll::Same);
        }
        let art = (|| -> windows::core::Result<Vec<u8>> {
            let stream = properties.Thumbnail()?.OpenReadAsync()?.get()?;
            let size = stream.Size()? as u32;
            let reader = DataReader::CreateDataReader(&stream)?;
            let load: IAsyncOperation<u32> = reader.LoadAsync(size)?.cast()?;
            load.get()?;
            let mut bytes = vec![0u8; size as usize];
            reader.ReadBytes(&mut bytes)?;
            Ok(bytes)
        })()
        .ok()
        .filter(|bytes| !bytes.is_empty());
        Ok(Poll::New { key, art })
    })();
    result.unwrap_or(Poll::Nothing)
}
