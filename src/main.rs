//! An invisible Wayland window for padding tiling layouts.
//!
//! It is fully transparent unless it is focused, hovered or floating. In any of
//! those states it shows a translucent background with centred text, and a click
//! closes it.
//!
//! The window never allocates a window-sized buffer. The background is a single
//! pixel scaled up with `wp_viewporter`, and the text is a small subsurface.

mod config;
mod font;

use std::fs;
use std::mem::MaybeUninit;
use std::os::fd::AsFd;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, VariableFont, point};
use config::Config;
use rustix::fs::inotify;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic},
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, Dispatch, QueueHandle, delegate_noop,
            globals::registry_queue_init,
            protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_subsurface, wl_surface},
        },
        csd_frame::WindowState,
        protocols::wp::viewporter::client::{
            wp_viewport::{self, WpViewport},
            wp_viewporter::WpViewporter,
        },
    },
    registry::{ProvidesRegistryState, RegistryState, SimpleGlobal},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        xdg::{
            XdgShell,
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
    subcompositor::SubcompositorState,
};

pub const APP: &str = env!("CARGO_PKG_NAME");
const DEFAULT_SIZE: (u32, u32) = (400, 300);

fn main() {
    let conn = Connection::connect_to_env().expect("not running under Wayland");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registry init failed");
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor missing");
    let subcompositor =
        SubcompositorState::bind(compositor.wl_compositor().clone(), &globals, &qh)
            .expect("wl_subcompositor missing");
    let xdg_shell = XdgShell::bind(&globals, &qh).expect("xdg_shell missing");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm missing");
    let pool = SlotPool::new(4096, &shm).expect("shm pool");
    let viewporter =
        SimpleGlobal::<WpViewporter, 1>::bind(&globals, &qh).expect("wp_viewporter missing");

    // Client-side decorations that we never draw: no title bar or border.
    let window = xdg_shell.create_window(
        compositor.create_surface(&qh),
        WindowDecorations::RequestClient,
        &qh,
    );
    window.set_title(APP);
    window.set_app_id(APP);
    window.commit();

    let viewport = viewporter
        .get()
        .expect("wp_viewporter missing")
        .get_viewport(window.wl_surface(), &qh, ());

    // Text lives on a subsurface with an empty input region, so the pointer only
    // ever sees the main surface and hover tracking stays simple.
    let (text_sub, text_surface) =
        subcompositor.create_subsurface(window.wl_surface().clone(), &qh);
    if let Ok(region) = Region::new(&compositor) {
        text_surface.set_input_region(Some(region.wl_region()));
    }

    config::ensure_exists();
    let config = config::load().unwrap_or_else(|e| {
        eprintln!("{APP}: {e}");
        Config::default()
    });

    let mut event_loop: EventLoop<State> = EventLoop::try_new().expect("event loop");
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), event_queue)
        .insert(handle.clone())
        .expect("wayland source");

    // Watch the config directory rather than the file so editors that save by
    // renaming a temp file are caught too.
    let watch = inotify::init(inotify::CreateFlags::NONBLOCK | inotify::CreateFlags::CLOEXEC)
        .and_then(|fd| {
            let flags = inotify::WatchFlags::CLOSE_WRITE
                | inotify::WatchFlags::MOVED_TO
                | inotify::WatchFlags::CREATE;
            inotify::add_watch(&fd, config::dir(), flags).map(|_| fd)
        });
    match watch {
        Ok(fd) => {
            let source = Generic::new(fd, Interest::READ, Mode::Level);
            handle
                .insert_source(source, |_, fd, state: &mut State| {
                    if config_changed(fd.as_fd()) {
                        state.reload_config();
                    }
                    Ok(PostAction::Continue)
                })
                .expect("inotify source");
        }
        Err(e) => eprintln!("{APP}: not watching config for changes: {e}"),
    }

    let mut state = State {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        pool,
        viewporter,
        window,
        viewport,
        text_surface,
        text_sub,
        pointer: None,
        font: resolve_font(&config.font),
        config,
        text: Text::default(),
        scale: 1,
        size: DEFAULT_SIZE,
        configured: false,
        focused: false,
        hovered: false,
        floating: false,
        exit: false,
    };
    state.rasterize_text();

    while !state.exit {
        // No timeout: sleep until a Wayland or inotify event arrives.
        event_loop.dispatch(None, &mut state).expect("event loop dispatch");
    }
}

/// Drains pending inotify events and reports whether any touched our config file.
fn config_changed(fd: impl AsFd) -> bool {
    let name = config::file_name();
    let mut buf = [MaybeUninit::uninit(); 4096];
    let mut reader = inotify::Reader::new(fd, &mut buf);
    let mut hit = false;
    while let Ok(event) = reader.next() {
        if event.file_name().is_some_and(|n| n.to_bytes() == name.as_bytes()) {
            hit = true;
        }
    }
    hit
}

/// Looks up the configured font, falling back to any sans font.
fn resolve_font(query: &str) -> Option<font::Face> {
    let face = font::find(query).or_else(|| {
        if query.eq_ignore_ascii_case("sans") {
            return None;
        }
        eprintln!("{APP}: font {query:?} not found, using sans");
        font::find("sans")
    });
    if face.is_none() {
        eprintln!("{APP}: no usable font found, drawing without text");
    }
    face
}

/// Rasterized text in premultiplied ARGB8888, sized in buffer pixels.
#[derive(Default)]
struct Text {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

struct State {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    viewporter: SimpleGlobal<WpViewporter, 1>,

    window: Window,
    viewport: WpViewport,
    text_surface: wl_surface::WlSurface,
    text_sub: wl_subsurface::WlSubsurface,
    pointer: Option<wl_pointer::WlPointer>,

    config: Config,
    font: Option<font::Face>,
    text: Text,
    scale: i32,
    size: (u32, u32),
    configured: bool,

    focused: bool,
    hovered: bool,
    floating: bool,
    exit: bool,
}

impl State {
    fn visible(&self) -> bool {
        self.focused || self.hovered || self.floating
    }

    fn reload_config(&mut self) {
        match config::load() {
            Ok(config) if config != self.config => {
                if config.font != self.config.font {
                    self.font = resolve_font(&config.font);
                }
                self.config = config;
                self.rasterize_text();
                self.draw();
            }
            Ok(_) => {}
            Err(e) => eprintln!("{APP}: {e} (keeping previous config)"),
        }
    }

    fn rasterize_text(&mut self) {
        self.text = Text::default();
        let Some(face) = &self.font else { return };
        // The font is read from disk only while rasterizing, then dropped.
        let font = fs::read(&face.path)
            .map_err(|e| e.to_string())
            .and_then(|data| FontVec::try_from_vec_and_index(data, face.index).map_err(|e| e.to_string()));
        let mut font = match font {
            Ok(font) => font,
            Err(e) => {
                eprintln!("{APP}: {}: {e}", face.path.display());
                return;
            }
        };
        // Variable fonts get their weight here; static fonts ignore this.
        font.set_variation(b"wght", self.config.font_weight as f32);
        let px = self.config.font_size * self.scale as f32;
        let font = font.as_scaled(PxScale::from(px));

        let mut glyphs = Vec::new();
        let mut x = 0.0;
        let mut prev = None;
        for c in self.config.text.chars() {
            let id = font.glyph_id(c);
            if let Some(p) = prev {
                x += font.kern(p, id);
            }
            glyphs.push(id.with_scale_and_position(px, point(x, font.ascent())));
            x += font.h_advance(id);
            prev = Some(id);
        }

        // Round up to a multiple of the scale so the logical size is whole.
        let s = self.scale as u32;
        let round = |v: f32| (v.ceil().max(0.0) as u32).div_ceil(s) * s;
        let (w, h) = (round(x), round(font.ascent() - font.descent()));
        let mut coverage = vec![0f32; (w * h) as usize];
        for glyph in glyphs {
            let Some(outline) = font.outline_glyph(glyph) else { continue };
            let b = outline.px_bounds();
            outline.draw(|gx, gy, c| {
                let x = gx as i32 + b.min.x as i32;
                let y = gy as i32 + b.min.y as i32;
                if x >= 0 && y >= 0 && (x as u32) < w && (y as u32) < h {
                    let i = (y as u32 * w + x as u32) as usize;
                    coverage[i] = (coverage[i] + c).min(1.0);
                }
            });
        }

        let color = self.config.text_color.to_le_bytes();
        let mut pixels = Vec::with_capacity(coverage.len() * 4);
        for c in coverage {
            pixels.extend(color.map(|ch| (ch as f32 * c + 0.5) as u8));
        }
        self.text = Text { pixels, width: w, height: h };
    }

    fn draw(&mut self) {
        if !self.configured {
            return;
        }
        let visible = self.visible();
        let surface = self.window.wl_surface().clone();

        // Background: one pixel stretched over the whole window.
        let color = if visible { self.config.background } else { 0 };
        let (buffer, canvas) = self
            .pool
            .create_buffer(1, 1, 4, wl_shm::Format::Argb8888)
            .expect("create buffer");
        canvas[..4].copy_from_slice(&color.to_le_bytes());
        buffer.attach_to(&surface).expect("attach");
        self.viewport.set_destination(self.size.0 as i32, self.size.1 as i32);
        surface.damage_buffer(0, 0, 1, 1);

        // Text subsurface: attached when visible, unmapped otherwise.
        let Text { width: tw, height: th, .. } = self.text;
        if visible && tw > 0 && th > 0 {
            let (buffer, canvas) = self
                .pool
                .create_buffer(tw as i32, th as i32, tw as i32 * 4, wl_shm::Format::Argb8888)
                .expect("create buffer");
            canvas[..self.text.pixels.len()].copy_from_slice(&self.text.pixels);
            buffer.attach_to(&self.text_surface).expect("attach");
            self.text_surface.set_buffer_scale(self.scale);
            self.text_surface.damage_buffer(0, 0, tw as i32, th as i32);
            let s = self.scale as u32;
            let x = (self.size.0 as i32 - (tw / s) as i32) / 2;
            let y = (self.size.1 as i32 - (th / s) as i32) / 2;
            self.text_sub.set_position(x, y);
        } else {
            self.text_surface.attach(None, 0, 0);
        }
        // Subsurfaces are synchronized, so this applies with the parent commit.
        self.text_surface.commit();
        surface.commit();
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        if surface == self.window.wl_surface() && new_factor != self.scale {
            self.scale = new_factor.max(1);
            self.rasterize_text();
            self.draw();
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
    }
}

impl WindowHandler for State {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &Window,
        configure: WindowConfigure,
        _: u32,
    ) {
        self.size = (
            configure.new_size.0.map_or(DEFAULT_SIZE.0, |w| w.get()),
            configure.new_size.1.map_or(DEFAULT_SIZE.1, |h| h.get()),
        );
        let state = configure.state;
        self.focused = state.contains(WindowState::ACTIVATED);
        // Tiling compositors send tiled states to tiled windows; anything
        // else that isn't maximized or fullscreen is floating.
        self.floating = !state.intersects(
            WindowState::TILED | WindowState::MAXIMIZED | WindowState::FULLSCREEN,
        );
        self.configured = true;
        self.draw();
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        let was_visible = self.visible();
        for event in events {
            if &event.surface != self.window.wl_surface() {
                continue;
            }
            match event.kind {
                PointerEventKind::Enter { .. } => self.hovered = true,
                PointerEventKind::Leave { .. } => self.hovered = false,
                PointerEventKind::Release { .. } => self.exit = true,
                _ => {}
            }
        }
        if self.visible() != was_visible {
            self.draw();
        }
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_noop!(State: WpViewporter);

impl Dispatch<WpViewport, ()> for State {
    fn event(
        _: &mut State,
        _: &WpViewport,
        _: wp_viewport::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl AsMut<SimpleGlobal<WpViewporter, 1>> for State {
    fn as_mut(&mut self) -> &mut SimpleGlobal<WpViewporter, 1> {
        &mut self.viewporter
    }
}

delegate_registry!(State);

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(State);
