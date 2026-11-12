// The visualiser on macOS, Windows and Linux X11: one borderless window per monitor, sitting on the desktop.
//   macOS:   transparent window just above the wallpaper; frames shown through Core Animation.
//   Windows: window placed inside the desktop behind the icons; draws the wallpaper itself underneath.
//   X11:     transparent "desktop" type window at the bottom of the stack.

use crate::analyzer::Analyzer;
use crate::engine::{Engine, ScreenInfo};
use crate::render::ensure_pixmap;
use crate::tray::{Tray, TrayAction};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tiny_skia::Pixmap;
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::monitor::MonitorHandle;
use winit::window::{Window, WindowId, WindowLevel};

const FRAME: Duration = Duration::from_micros(16_667);

pub fn run() {
    let analyzer = Analyzer::new();
    crate::audio::start(analyzer.clone());
    let engine = Engine::new(analyzer);

    #[cfg(target_os = "macos")]
    let event_loop = {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        EventLoop::builder().with_activation_policy(ActivationPolicy::Accessory).build()
    };
    #[cfg(not(target_os = "macos"))]
    let event_loop = EventLoop::new();

    let event_loop = match event_loop {
        Ok(event_loop) => event_loop,
        Err(error) => {
            eprintln!("AudioWallpaper: couldn't start ({error})");
            return;
        }
    };
    let mut app = App { engine, screens: Vec::new(), layout: Vec::new(), tray: None, next_frame: Instant::now(), ticks: 0 };
    if let Err(error) = event_loop.run_app(&mut app) {
        eprintln!("AudioWallpaper: {error}");
    }
}

type Layout = Vec<(i32, i32, u32, u32)>;

fn monitor_layout(event_loop: &ActiveEventLoop) -> Layout {
    event_loop
        .available_monitors()
        .map(|m| {
            let (p, s) = (m.position(), m.size());
            (p.x, p.y, s.width, s.height)
        })
        .collect()
}

struct App {
    engine: Engine,
    screens: Vec<Screen>,
    layout: Layout,
    tray: Option<Tray>,
    next_frame: Instant,
    ticks: u64,
}

impl App {
    fn rebuild(&mut self, event_loop: &ActiveEventLoop) {
        self.screens.clear();
        for monitor in event_loop.available_monitors() {
            if let Some(screen) = Screen::create(event_loop, &monitor) {
                self.screens.push(screen);
            }
        }
        self.layout = monitor_layout(event_loop);
    }
}

impl ApplicationHandler for App {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::Init) && self.tray.is_none() {
            self.tray = Tray::new();
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.screens.is_empty() {
            self.rebuild(event_loop);
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }

    fn window_event(&mut self, _event_loop: &ActiveEventLoop, _id: WindowId, _event: WindowEvent) {}

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(tray) = &self.tray {
            if tray.poll() == TrayAction::Quit {
                event_loop.exit();
                return;
            }
        }

        let now = Instant::now();
        if now >= self.next_frame {
            self.next_frame = (self.next_frame + FRAME).max(now);
            self.ticks += 1;

            // Every two seconds: monitors plugged in or out, Dock size changes.
            if self.ticks % 120 == 0 {
                if monitor_layout(event_loop) != self.layout {
                    self.rebuild(event_loop);
                } else {
                    for screen in &mut self.screens {
                        screen.refresh_metrics();
                    }
                }
            }

            let tick = self.engine.tick();
            if tick.redraw {
                for screen in &mut self.screens {
                    screen.draw(&mut self.engine);
                }
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}

/// A rectangle in physical pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
struct PxRect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

#[allow(dead_code)]
impl PxRect {
    fn union(self, other: PxRect) -> PxRect {
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = (self.x + self.w).max(other.x + other.w);
        let y1 = (self.y + self.h).max(other.y + other.h);
        PxRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
    }

    fn clip(self, width: i32, height: i32) -> Option<PxRect> {
        let x0 = self.x.max(0);
        let y0 = self.y.max(0);
        let x1 = (self.x + self.w).min(width);
        let y1 = (self.y + self.h).min(height);
        if x1 > x0 && y1 > y0 {
            Some(PxRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
        } else {
            None
        }
    }
}

struct Screen {
    window: Arc<Window>,
    info: ScreenInfo,
    /// Physical pixels per logical point that we render at (1 on macOS: Core Animation scales up).
    scale: f32,
    pixmap: Option<Pixmap>,
    output: Output,
}

#[cfg(target_os = "macos")]
struct Output {
    view: *mut std::ffi::c_void,
    dim: f32,
}

#[cfg(not(target_os = "macos"))]
struct Output {
    _context: softbuffer::Context<Arc<Window>>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    size: (u32, u32),
    /// What's on screen, as 0xAARRGGBB.
    frame: Vec<u32>,
    /// What's under the visualiser: the dimmed wallpaper on Windows, translucent black elsewhere.
    background: Vec<u32>,
    background_dim: Option<f32>,
    #[cfg(target_os = "windows")]
    wallpaper: Option<image::RgbaImage>,
    previous: Option<PxRect>,
    full: bool,
}

impl Screen {
    fn create(event_loop: &ActiveEventLoop, monitor: &MonitorHandle) -> Option<Screen> {
        let position = monitor.position();
        let size = monitor.size();
        let monitor_scale = monitor.scale_factor() as f32;

        let mut attributes = Window::default_attributes()
            .with_title("AudioWallpaper")
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(!cfg!(target_os = "windows"))
            .with_position(position)
            .with_inner_size(size)
            .with_window_level(WindowLevel::AlwaysOnBottom)
            .with_active(false);
        #[cfg(target_os = "linux")]
        {
            use winit::platform::x11::{WindowAttributesExtX11, WindowType};
            attributes = attributes.with_x11_window_type(vec![WindowType::Desktop]);
        }
        #[cfg(target_os = "windows")]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attributes = attributes.with_skip_taskbar(true);
        }

        let window = Arc::new(event_loop.create_window(attributes).ok()?);
        #[cfg(target_os = "linux")]
        let _ = window.set_cursor_hittest(false);

        let info = ScreenInfo {
            width: size.width as f32 / monitor_scale,
            height: size.height as f32 / monitor_scale,
            bottom_inset: 0.0,
            dock_width: None,
        };

        #[cfg(target_os = "macos")]
        {
            let view = view_pointer(&window)?;
            crate::platform::mac::configure_window(view);
            let mut screen = Screen { window, info, scale: 1.0, pixmap: None, output: Output { view, dim: -1.0 } };
            screen.refresh_metrics();
            Some(screen)
        }

        #[cfg(not(target_os = "macos"))]
        {
            let mut info = info;
            #[cfg(target_os = "windows")]
            {
                if let Some(hwnd) = hwnd_of(&window) {
                    info.bottom_inset = crate::platform::win::bottom_inset(hwnd) as f32 / monitor_scale;
                    if !crate::platform::win::attach_to_desktop(hwnd) {
                        eprintln!("AudioWallpaper: couldn't find the desktop window; showing as a bottom window.");
                    }
                }
            }
            let context = softbuffer::Context::new(window.clone()).ok()?;
            let surface = softbuffer::Surface::new(&context, window.clone()).ok()?;
            let output = Output {
                _context: context,
                surface,
                size: (0, 0),
                frame: Vec::new(),
                background: Vec::new(),
                background_dim: None,
                #[cfg(target_os = "windows")]
                wallpaper: None,
                previous: None,
                full: true,
            };
            Some(Screen { window, info, scale: monitor_scale, pixmap: None, output })
        }
    }

    fn refresh_metrics(&mut self) {
        #[cfg(target_os = "macos")]
        {
            let metrics = crate::platform::mac::screen_metrics(self.output.view);
            self.info.bottom_inset = metrics.bottom_inset as f32;
            self.info.dock_width = if metrics.dock_on_side != 0 || metrics.dock_width <= 0.0 {
                None
            } else {
                Some(metrics.dock_width as f32)
            };
        }
    }

    #[cfg(target_os = "macos")]
    fn draw(&mut self, engine: &mut Engine) {
        let _ = &self.window;
        let dim = engine.config.dim;
        if (dim - self.output.dim).abs() > 0.001 {
            crate::platform::mac::set_dim(self.output.view, dim);
            self.output.dim = dim;
        }
        let stage = engine.stage_rect(&self.info);
        let (w, h) = (stage.w.ceil().max(1.0) as u32, stage.h.ceil().max(1.0) as u32);
        let Some(pixmap) = ensure_pixmap(&mut self.pixmap, w, h) else { return };
        engine.render(pixmap, stage, &self.info, self.scale);
        crate::platform::mac::present(self.output.view, pixmap.data(), w, h, stage.x, stage.y, stage.w, stage.h);
    }

    #[cfg(not(target_os = "macos"))]
    fn draw(&mut self, engine: &mut Engine) {
        use std::num::NonZeroU32;

        let size = self.window.inner_size();
        let (pw, ph) = (size.width.max(1), size.height.max(1));
        let out = &mut self.output;
        if out.size != (pw, ph) {
            let (Some(nw), Some(nh)) = (NonZeroU32::new(pw), NonZeroU32::new(ph)) else { return };
            if out.surface.resize(nw, nh).is_err() {
                return;
            }
            out.size = (pw, ph);
            out.frame = vec![0; (pw * ph) as usize];
            out.background = vec![0; (pw * ph) as usize];
            out.background_dim = None;
            #[cfg(target_os = "windows")]
            {
                out.wallpaper = None;
            }
            out.previous = None;
            out.full = true;
        }

        let dim = engine.config.dim;
        if out.background_dim != Some(dim) {
            build_background(out, dim);
            out.background_dim = Some(dim);
            out.full = true;
        }

        let stage = engine.stage_rect(&self.info);
        let s = self.scale;
        let rect = PxRect {
            x: (stage.x * s).floor() as i32,
            y: (stage.y * s).floor() as i32,
            w: (stage.w * s).ceil().max(1.0) as i32,
            h: (stage.h * s).ceil().max(1.0) as i32,
        };
        let Some(pixmap) = ensure_pixmap(&mut self.pixmap, rect.w as u32, rect.h as u32) else { return };
        engine.render(pixmap, stage, &self.info, s);

        let (fw, fh) = (pw as i32, ph as i32);
        if out.full {
            out.frame.copy_from_slice(&out.background);
        } else if let Some(previous) = out.previous {
            restore(&mut out.frame, &out.background, fw, previous);
        }
        composite(&mut out.frame, fw, fh, pixmap, rect);

        let damage = if out.full {
            Some(PxRect { x: 0, y: 0, w: fw, h: fh })
        } else {
            let changed = match out.previous {
                Some(previous) => previous.union(rect),
                None => rect,
            };
            changed.clip(fw, fh)
        };
        out.previous = rect.clip(fw, fh);
        out.full = false;

        let Ok(mut buffer) = out.surface.buffer_mut() else { return };
        if buffer.len() == out.frame.len() {
            buffer.copy_from_slice(&out.frame);
        }
        let _ = match damage {
            Some(d) => buffer.present_with_damage(&[softbuffer::Rect {
                x: d.x as u32,
                y: d.y as u32,
                width: NonZeroU32::new(d.w as u32).unwrap_or(NonZeroU32::MIN),
                height: NonZeroU32::new(d.h as u32).unwrap_or(NonZeroU32::MIN),
            }]),
            None => buffer.present(),
        };
    }
}

#[cfg(target_os = "macos")]
fn view_pointer(window: &Window) -> Option<*mut std::ffi::c_void> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::AppKit(handle) => Some(handle.ns_view.as_ptr()),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
fn hwnd_of(window: &Window) -> Option<windows_sys::Win32::Foundation::HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as windows_sys::Win32::Foundation::HWND),
        _ => None,
    }
}

#[cfg(not(target_os = "macos"))]
fn build_background(out: &mut Output, dim: f32) {
    let dim = dim.clamp(0.0, 1.0);
    #[cfg(target_os = "windows")]
    {
        let (w, h) = out.size;
        if out.wallpaper.is_none() {
            out.wallpaper = Some(crate::platform::win::wallpaper(w, h));
        }
        let keep = 1.0 - dim;
        if let Some(wallpaper) = &out.wallpaper {
            for (dst, p) in out.background.iter_mut().zip(wallpaper.pixels()) {
                let r = (p[0] as f32 * keep) as u32;
                let g = (p[1] as f32 * keep) as u32;
                let b = (p[2] as f32 * keep) as u32;
                *dst = 0xFF00_0000 | (r << 16) | (g << 8) | b;
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let alpha = (dim * 255.0).round() as u32;
        out.background.fill(alpha << 24);
    }
}

/// Put the background back where the visualiser was last frame.
#[cfg(not(target_os = "macos"))]
fn restore(frame: &mut [u32], background: &[u32], width: i32, rect: PxRect) {
    for y in rect.y..rect.y + rect.h {
        let start = (y * width + rect.x) as usize;
        let end = start + rect.w as usize;
        if end <= frame.len() && end <= background.len() {
            frame[start..end].copy_from_slice(&background[start..end]);
        }
    }
}

/// Blend the visualiser's premultiplied RGBA pixels over the frame at `at`.
#[cfg(not(target_os = "macos"))]
fn composite(frame: &mut [u32], width: i32, height: i32, pixmap: &Pixmap, at: PxRect) {
    let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
    let data = pixmap.data();
    let (x0, y0) = (at.x.max(0), at.y.max(0));
    let (x1, y1) = ((at.x + pw).min(width), (at.y + ph).min(height));
    for y in y0..y1 {
        let sy = y - at.y;
        for x in x0..x1 {
            let sx = x - at.x;
            let si = ((sy * pw + sx) * 4) as usize;
            let a = data[si + 3] as u32;
            if a == 0 {
                continue;
            }
            let (r, g, b) = (data[si] as u32, data[si + 1] as u32, data[si + 2] as u32);
            let fi = (y * width + x) as usize;
            if a == 255 {
                frame[fi] = 0xFF00_0000 | (r << 16) | (g << 8) | b;
                continue;
            }
            let d = frame[fi];
            let inv = 255 - a;
            let na = a + ((d >> 24) & 255) * inv / 255;
            let nr = r + ((d >> 16) & 255) * inv / 255;
            let ng = g + ((d >> 8) & 255) * inv / 255;
            let nb = b + (d & 255) * inv / 255;
            frame[fi] = (na.min(255) << 24) | (nr.min(255) << 16) | (ng.min(255) << 8) | nb.min(255);
        }
    }
}
