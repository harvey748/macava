// The visualiser on Wayland desktops with layer-shell (KDE Plasma, Sway, Hyprland, river, Wayfire...).
// Two layer surfaces on the default monitor, just above the wallpaper and below windows:
//   - a full-screen backdrop for dimming
//   - the visualiser itself, anchored to the bottom (wave, bars) or centred (3D shape, album cover)
// Both ignore the mouse. Panels are avoided automatically by the compositor.

use crate::analyzer::Analyzer;
use crate::config::Style;
use crate::engine::{Engine, ScreenInfo};
use crate::render::ensure_pixmap;
use crate::tray::{Tray, TrayAction};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};
use tiny_skia::Pixmap;
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};

const FRAME: Duration = Duration::from_micros(16_667);

struct Surf {
    layer: LayerSurface,
    /// Size the compositor gave us.
    size: Option<(u32, u32)>,
    /// Size we last asked for.
    requested: (u32, u32),
    anchored_bottom: bool,
    margin: i32,
}

struct State {
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    backdrop: Surf,
    stage: Surf,
    engine: Engine,
    tray: Option<Tray>,
    pixmap: Option<Pixmap>,
    dim_drawn: Option<f32>,
    stage_dirty: bool,
    next_frame: Instant,
    exit: bool,
}

/// Returns Err only if layer-shell isn't available, so the caller can fall back to X11.
pub fn run() -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| e.to_string())?;
    let layer_shell =
        LayerShell::bind(&globals, &qh).map_err(|_| "this desktop doesn't support layer-shell".to_string())?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| e.to_string())?;
    let pool = SlotPool::new(512 * 512 * 4, &shm).map_err(|e| e.to_string())?;

    let backdrop = new_layer(&compositor, &layer_shell, &qh, "audiowallpaper-dim");
    backdrop.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    backdrop.set_exclusive_zone(-1);
    backdrop.set_size(0, 0);
    backdrop.commit();

    let stage = new_layer(&compositor, &layer_shell, &qh, "audiowallpaper");
    stage.set_anchor(Anchor::BOTTOM);
    stage.set_size(1, 1);
    stage.commit();

    let analyzer = Analyzer::new();
    crate::audio::start(analyzer.clone());

    let mut state = State {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        shm,
        pool,
        backdrop: Surf { layer: backdrop, size: None, requested: (0, 0), anchored_bottom: false, margin: 0 },
        stage: Surf { layer: stage, size: None, requested: (1, 1), anchored_bottom: true, margin: 0 },
        engine: Engine::new(analyzer),
        tray: Tray::new(),
        pixmap: None,
        dim_drawn: None,
        stage_dirty: true,
        next_frame: Instant::now(),
        exit: false,
    };

    if let Err(error) = queue.roundtrip(&mut state) {
        eprintln!("AudioWallpaper: Wayland error: {error}");
        std::process::exit(1);
    }

    loop {
        if let Err(error) = queue.dispatch_pending(&mut state) {
            eprintln!("AudioWallpaper: Wayland error: {error}");
            std::process::exit(1);
        }
        if let Err(error) = queue.flush() {
            eprintln!("AudioWallpaper: Wayland error: {error}");
            std::process::exit(1);
        }

        // Sleep until the next frame is due or the compositor sends something.
        let wait = state.next_frame.saturating_duration_since(Instant::now());
        if let Some(guard) = queue.prepare_read() {
            let mut poll_fd = libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 };
            let ready = unsafe { libc::poll(&mut poll_fd, 1, wait.as_millis().min(1000) as i32) };
            if ready > 0 {
                let _ = guard.read();
            }
        }
        if let Err(error) = queue.dispatch_pending(&mut state) {
            eprintln!("AudioWallpaper: Wayland error: {error}");
            std::process::exit(1);
        }

        if Instant::now() >= state.next_frame {
            state.next_frame = (state.next_frame + FRAME).max(Instant::now());
            state.frame();
        }
        if state.exit {
            return Ok(());
        }
    }
}

fn new_layer(
    compositor: &CompositorState,
    layer_shell: &LayerShell,
    qh: &QueueHandle<State>,
    name: &'static str,
) -> LayerSurface {
    let surface = compositor.create_surface(qh);
    let layer = layer_shell.create_layer_surface(qh, surface, Layer::Bottom, Some(name), None);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    // An empty input region: clicks go straight through to the desktop.
    if let Ok(region) = Region::new(compositor) {
        layer.wl_surface().set_input_region(Some(region.wl_region()));
    }
    layer
}

impl State {
    fn screen(&self) -> ScreenInfo {
        let size = self
            .output_state
            .outputs()
            .next()
            .and_then(|output| self.output_state.info(&output))
            .and_then(|info| info.logical_size)
            .unwrap_or((1920, 1080));
        ScreenInfo { width: size.0 as f32, height: size.1 as f32, bottom_inset: 0.0, dock_width: None }
    }

    fn frame(&mut self) {
        if let Some(tray) = &self.tray {
            if tray.poll() == TrayAction::Quit {
                self.exit = true;
                return;
            }
        }
        let tick = self.engine.tick();
        let screen = self.screen();
        self.place_stage(&screen);

        let dim = self.engine.config.dim;
        if self.dim_drawn != Some(dim) && self.draw_backdrop(dim) {
            self.dim_drawn = Some(dim);
        }
        if tick.redraw || self.stage_dirty {
            self.draw_stage(&screen);
        }
    }

    /// Size and anchor the visualiser surface for the current style.
    fn place_stage(&mut self, screen: &ScreenInfo) {
        let rect = self.engine.stage_rect(screen);
        let size = (rect.w.ceil().max(1.0) as u32, rect.h.ceil().max(1.0) as u32);
        let bottom = matches!(self.engine.config.style, Style::Wave | Style::Bars);
        let margin = if bottom { self.engine.config.bottom_offset.round() as i32 } else { 0 };
        let stage = &mut self.stage;
        if size == stage.requested && bottom == stage.anchored_bottom && margin == stage.margin {
            return;
        }
        stage.layer.set_anchor(if bottom { Anchor::BOTTOM } else { Anchor::empty() });
        stage.layer.set_margin(0, 0, margin, 0);
        stage.layer.set_size(size.0, size.1);
        stage.layer.commit();
        stage.requested = size;
        stage.anchored_bottom = bottom;
        stage.margin = margin;
        self.stage_dirty = true;
    }

    fn draw_stage(&mut self, screen: &ScreenInfo) {
        let Some((w, h)) = self.stage.size else { return };
        let mut rect = self.engine.stage_rect(screen);
        rect.w = w as f32;
        rect.h = h as f32;
        let Some(pixmap) = ensure_pixmap(&mut self.pixmap, w, h) else { return };
        self.engine.render(pixmap, rect, screen, 1.0);

        let Ok((buffer, canvas)) = self.pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888)
        else {
            return;
        };
        // RGBA -> the BGRA byte order Wayland calls ARGB8888 (both premultiplied).
        for (dst, src) in canvas.chunks_exact_mut(4).zip(pixmap.data().chunks_exact(4)) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = src[3];
        }
        let surface = self.stage.layer.wl_surface();
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(surface).is_ok() {
            self.stage.layer.commit();
            self.stage_dirty = false;
        }
    }

    fn draw_backdrop(&mut self, dim: f32) -> bool {
        let Some((w, h)) = self.backdrop.size else { return false };
        let Ok((buffer, canvas)) = self.pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888)
        else {
            return false;
        };
        let alpha = (dim.clamp(0.0, 1.0) * 255.0).round() as u8;
        for pixel in canvas.chunks_exact_mut(4) {
            pixel[0] = 0;
            pixel[1] = 0;
            pixel[2] = 0;
            pixel[3] = alpha;
        }
        let surface = self.backdrop.layer.wl_surface();
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(surface).is_err() {
            return false;
        }
        self.backdrop.layer.commit();
        true
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let (w, h) = configure.new_size;
        if layer.wl_surface() == self.stage.layer.wl_surface() {
            self.stage.size = Some(if w == 0 || h == 0 { self.stage.requested } else { (w, h) });
            self.stage_dirty = true;
        } else {
            let screen = self.screen();
            let fallback = (screen.width as u32, screen.height as u32);
            self.backdrop.size = Some(if w == 0 || h == 0 { fallback } else { (w, h) });
            self.dim_drawn = None;
        }
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

delegate_compositor!(State);
delegate_output!(State);
delegate_shm!(State);
delegate_layer!(State);
delegate_registry!(State);
