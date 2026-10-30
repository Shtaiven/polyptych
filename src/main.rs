//! An invisible Wayland window for padding tiling layouts.
//!
//! It always shows its idle look: a colour (transparent by default) with an
//! optional image over it. When it is focused, hovered or floating, a
//! translucent colour and centred text are drawn over that. Dragging with the left button moves it
//! and a right click closes it.
//!
//! Colours are single pixels scaled up with `wp_viewporter`, the image is
//! scaled the same way, and the text is a small subsurface. Only tiling an
//! image smaller than the window needs a window-sized buffer.

mod config;
mod font;
mod image;

use std::fs;
use std::mem::MaybeUninit;
use std::os::fd::AsFd;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, VariableFont, point};
use config::{Config, Fit};
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
            protocol::{
                wl_buffer, wl_output, wl_pointer, wl_seat, wl_shm, wl_subsurface, wl_surface,
            },
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
            XdgShell, XdgSurface,
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{Shm, ShmHandler, raw::RawPool, slot::SlotPool},
    subcompositor::SubcompositorState,
};

pub const APP: &str = env!("CARGO_PKG_NAME");
const DEFAULT_SIZE: (u32, u32) = (400, 300);
/// Linux input event codes for the mouse buttons.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

fn main() {
    if std::env::args()
        .skip(1)
        .any(|a| a == "--version" || a == "-V")
    {
        println!("{APP} {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let conn = Connection::connect_to_env().expect("not running under Wayland");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registry init failed");
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor missing");
    let subcompositor = SubcompositorState::bind(compositor.wl_compositor().clone(), &globals, &qh)
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
    // An explicit tiny minimum: COSMIC substitutes 320x240 for floating
    // windows that leave it unset.
    window.set_min_size(Some((1, 1)));
    window.commit();

    let wp_viewporter = viewporter.get().expect("wp_viewporter missing");
    let viewport = wp_viewporter.get_viewport(window.wl_surface(), &qh, ());

    // Layers above the main surface, bottom to top: the idle image, the focus
    // colour and the text. Each has an empty input region, so the pointer only
    // ever sees the main surface and hover tracking stays simple.
    let subsurface = || {
        let (sub, surface) = subcompositor.create_subsurface(window.wl_surface().clone(), &qh);
        if let Ok(region) = Region::new(&compositor) {
            surface.set_input_region(Some(region.wl_region()));
        }
        (sub, surface)
    };
    let layer = || {
        let (sub, surface) = subsurface();
        let viewport = wp_viewporter.get_viewport(&surface, &qh, ());
        Layer {
            surface,
            sub,
            viewport,
        }
    };
    let image_layer = layer();
    let overlay = layer();
    let (text_sub, text_surface) = subsurface();

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

    let idle_image = load_idle_image(&config, &shm);
    let mut state = State {
        qh: qh.clone(),
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        pool,
        viewporter,
        window,
        viewport,
        image_layer,
        overlay,
        text_surface,
        text_sub,
        pointer: None,
        seat: None,
        font: resolve_font(&config.font),
        idle_image,
        image_layout: None,
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

    while !state.exit {
        // No timeout: sleep until a Wayland or inotify event arrives.
        event_loop
            .dispatch(None, &mut state)
            .expect("event loop dispatch");
    }
}

/// Drains pending inotify events and reports whether any touched our config file.
fn config_changed(fd: impl AsFd) -> bool {
    let name = config::file_name();
    let mut buf = [MaybeUninit::uninit(); 4096];
    let mut reader = inotify::Reader::new(fd, &mut buf);
    let mut hit = false;
    while let Ok(event) = reader.next() {
        if event
            .file_name()
            .is_some_and(|n| n.to_bytes() == name.as_bytes())
        {
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

/// Loads the configured idle image. Buffers are views into its pool, which
/// never changes after this.
fn load_idle_image(config: &Config, shm: &Shm) -> Option<IdleImage> {
    let path = config.idle_image.as_ref()?;
    let image::Image {
        pool,
        width,
        height,
    } = image::load_any(path, shm)
        .map_err(|e| eprintln!("{APP}: {e}"))
        .ok()?;
    Some(IdleImage {
        pool,
        width,
        height,
        current: None,
        previous: None,
        tile_pool: None,
    })
}

/// A surface stacked above the window, sized with its own viewport.
struct Layer {
    surface: wl_surface::WlSurface,
    sub: wl_subsurface::WlSubsurface,
    viewport: WpViewport,
}

/// Covers the window with one pixel of `color`, stretched by the viewport.
fn fill(
    pool: &mut SlotPool,
    surface: &wl_surface::WlSurface,
    viewport: &WpViewport,
    (w, h): (u32, u32),
    color: u32,
) {
    let (buffer, canvas) = pool
        .create_buffer(1, 1, 4, wl_shm::Format::Argb8888)
        .expect("create buffer");
    canvas[..4].copy_from_slice(&color.to_le_bytes());
    buffer.attach_to(surface).expect("attach");
    viewport.set_destination(w as i32, h as i32);
    surface.damage_buffer(0, 0, 1, 1);
}

/// A rectangle (x, y, width, height).
type Rect = (u32, u32, u32, u32);

/// Where a scaled image goes: the part of the image to show, and where in the
/// window to draw it. Tile isn't scaled, so it is handled separately.
fn place(fit: Fit, (w, h): (u32, u32), (iw, ih): (u32, u32)) -> (Rect, Rect) {
    let (w, h, iw, ih) = (w as u64, h as u64, iw as u64, ih as u64);
    // Whether the window is wider than the image, relative to their heights.
    let wider = w * ih > h * iw;
    let whole = (0, 0, iw as u32, ih as u32);
    let window = (0, 0, w as u32, h as u32);
    match fit {
        Fit::Zoom => {
            let (dw, dh) = if wider {
                (iw * h / ih, h)
            } else {
                (w, ih * w / iw)
            };
            let (dw, dh) = (dw.max(1), dh.max(1));
            (
                whole,
                (
                    ((w - dw) / 2) as u32,
                    ((h - dh) / 2) as u32,
                    dw as u32,
                    dh as u32,
                ),
            )
        }
        Fit::Fill => {
            // Crop the image to the window's aspect ratio, centred.
            let (sw, sh) = if wider {
                (iw, (iw * h / w).clamp(1, ih))
            } else {
                ((ih * w / h).clamp(1, iw), ih)
            };
            let crop = (
                ((iw - sw) / 2) as u32,
                ((ih - sh) / 2) as u32,
                sw as u32,
                sh as u32,
            );
            (crop, window)
        }
        Fit::Stretch => (whole, window),
        Fit::Tile => unreachable!("tiles aren't scaled"),
    }
}

struct IdleImage {
    pool: RawPool,
    width: u32,
    height: u32,
    /// The attached view and the one it replaced, which may still be on
    /// screen until the next commit, so it is destroyed one view later.
    current: Option<wl_buffer::WlBuffer>,
    previous: Option<wl_buffer::WlBuffer>,
    /// Holds window-sized tiles, apart from the small text buffers so that
    /// pool doesn't stay large. Freed once tiling no longer needs it.
    tile_pool: Option<SlotPool>,
}

impl IdleImage {
    /// A buffer showing the (x, y, w, h) rectangle of the image, made by
    /// pointing into the pool rather than copying.
    fn view(
        &mut self,
        (x, y, w, h): (u32, u32, u32, u32),
        qh: &QueueHandle<State>,
    ) -> wl_buffer::WlBuffer {
        let offset = ((y * self.width + x) * 4) as i32;
        let stride = self.width as i32 * 4;
        let format = wl_shm::Format::Argb8888;
        let buffer = self
            .pool
            .create_buffer(offset, w as i32, h as i32, stride, format, (), qh);
        let replaced = std::mem::replace(&mut self.previous, self.current.replace(buffer.clone()));
        if let Some(old) = replaced {
            old.destroy();
        }
        buffer
    }
}

impl Drop for IdleImage {
    fn drop(&mut self) {
        self.current
            .iter()
            .chain(&self.previous)
            .for_each(|b| b.destroy());
    }
}

/// Rasterized text in premultiplied ARGB8888, sized in buffer pixels.
#[derive(Default)]
struct Text {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    /// The (window width, scale) this was laid out for; `None` means stale.
    layout: Option<(u32, i32)>,
}

struct State {
    qh: QueueHandle<State>,
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    viewporter: SimpleGlobal<WpViewporter, 1>,

    window: Window,
    viewport: WpViewport,
    image_layer: Layer,
    /// The focus colour, drawn over the idle look while visible.
    overlay: Layer,
    text_surface: wl_surface::WlSurface,
    text_sub: wl_subsurface::WlSubsurface,
    pointer: Option<wl_pointer::WlPointer>,
    /// The seat the pointer belongs to, needed to start an interactive move.
    seat: Option<wl_seat::WlSeat>,

    config: Config,
    font: Option<font::Face>,
    idle_image: Option<IdleImage>,
    /// The (width, height, scale) the image layer was set up for; `None` means stale.
    image_layout: Option<(u32, u32, i32)>,
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

    /// Applies the config file, or the defaults if it is invalid.
    fn reload_config(&mut self) {
        let config = config::load().unwrap_or_else(|e| {
            eprintln!("{APP}: {e} (using defaults)");
            Config::default()
        });
        if config == self.config {
            return;
        }
        if config.font != self.config.font {
            self.font = resolve_font(&config.font);
        }
        if config.idle_image != self.config.idle_image {
            // Free the old image before decoding the new one.
            self.idle_image = None;
            self.idle_image = load_idle_image(&config, &self.shm);
            self.image_layout = None;
        }
        if config.idle_image_fit != self.config.idle_image_fit {
            self.image_layout = None;
        }
        self.config = config;
        self.text = Text::default();
        self.draw();
    }

    /// Lays the text out word-wrapped to the window width and rasterizes it.
    fn rasterize_text(&mut self) {
        let layout = (self.size.0, self.scale);
        self.text = Text {
            layout: Some(layout),
            ..Text::default()
        };
        let Some(face) = &self.font else { return };
        // The font is read from disk only while rasterizing, then dropped.
        let font = fs::read(&face.path)
            .map_err(|e| e.to_string())
            .and_then(|data| {
                FontVec::try_from_vec_and_index(data, face.index).map_err(|e| e.to_string())
            });
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

        let measure = |line: &str| {
            let mut prev = None;
            line.chars().fold(0.0, |x, c| {
                let id = font.glyph_id(c);
                let kern = prev.map_or(0.0, |p| font.kern(p, id));
                prev = Some(id);
                x + kern + font.h_advance(id)
            })
        };

        // Greedy word wrap; explicit newlines in the text are kept. A word
        // wider than the window gets its own line and is cropped when drawn.
        let max_width = (self.size.0 * self.scale as u32) as f32;
        let mut lines = Vec::new();
        for paragraph in self.config.focus_text.split('\n') {
            let mut line = String::new();
            for word in paragraph.split_whitespace() {
                let candidate = if line.is_empty() {
                    word.to_string()
                } else {
                    format!("{line} {word}")
                };
                if !line.is_empty() && measure(&candidate) > max_width {
                    lines.push(std::mem::replace(&mut line, word.to_string()));
                } else {
                    line = candidate;
                }
            }
            lines.push(line);
        }
        let widths: Vec<f32> = lines.iter().map(|l| measure(l)).collect();
        let line_height = font.height() + font.line_gap();

        // Round up to a multiple of the scale so the logical size is whole.
        let s = self.scale as u32;
        let round = |v: f32| (v.ceil().max(0.0) as u32).div_ceil(s) * s;
        let w = round(widths.iter().copied().fold(0.0, f32::max));
        let h = round(line_height * lines.len() as f32 - font.line_gap());
        let mut coverage = vec![0f32; (w * h) as usize];
        for (i, line) in lines.iter().enumerate() {
            let mut x = (w as f32 - widths[i]) / 2.0;
            let y = i as f32 * line_height + font.ascent();
            let mut prev = None;
            for c in line.chars() {
                let id = font.glyph_id(c);
                if let Some(p) = prev {
                    x += font.kern(p, id);
                }
                prev = Some(id);
                let glyph = id.with_scale_and_position(px, point(x, y));
                x += font.h_advance(id);
                let Some(outline) = font.outline_glyph(glyph) else {
                    continue;
                };
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
        }

        let color = self.config.text_color.to_le_bytes();
        let mut pixels = Vec::with_capacity(coverage.len() * 4);
        for c in coverage {
            pixels.extend(color.map(|ch| (ch as f32 * c + 0.5) as u8));
        }
        self.text = Text {
            pixels,
            width: w,
            height: h,
            layout: Some(layout),
        };
    }

    fn draw(&mut self) {
        if !self.configured {
            return;
        }
        let visible = self.visible();
        let surface = self.window.wl_surface().clone();

        let (w, h) = self.size;
        // The idle colour, under the idle image if there is one.
        fill(
            &mut self.pool,
            &surface,
            &self.viewport,
            self.size,
            self.config.idle_color,
        );
        // Without explicit geometry the compositor would include the
        // subsurfaces in the window's size, so text could hold the window open.
        self.window.set_window_geometry(0, 0, w, h);

        if self.image_layout != Some((w, h, self.scale)) {
            self.image_layout = Some((w, h, self.scale));
            self.layout_image();
        }

        let Layer {
            surface: overlay,
            viewport,
            ..
        } = &self.overlay;
        if visible {
            fill(
                &mut self.pool,
                overlay,
                viewport,
                self.size,
                self.config.focus_color,
            );
        } else {
            overlay.attach(None, 0, 0);
        }

        // Text subsurface: attached when visible, unmapped otherwise. Text is
        // only laid out while visible, and again when the width changes; it is
        // cropped to the window so a small window just shows less of it.
        if visible && self.text.layout != Some((w, self.scale)) {
            self.rasterize_text();
        }
        let Text {
            width: tw,
            height: th,
            ..
        } = self.text;
        let s = self.scale as u32;
        let (cw, ch) = (tw.min(w * s) / s * s, th.min(h * s) / s * s);
        if visible && cw > 0 && ch > 0 {
            let (buffer, canvas) = self
                .pool
                .create_buffer(
                    cw as i32,
                    ch as i32,
                    cw as i32 * 4,
                    wl_shm::Format::Argb8888,
                )
                .expect("create buffer");
            let (ox, oy) = ((tw - cw) / 2, (th - ch) / 2);
            for row in 0..ch {
                let src = (((oy + row) * tw + ox) * 4) as usize;
                let dst = (row * cw * 4) as usize;
                let len = (cw * 4) as usize;
                canvas[dst..dst + len].copy_from_slice(&self.text.pixels[src..src + len]);
            }
            buffer.attach_to(&self.text_surface).expect("attach");
            self.text_surface.set_buffer_scale(self.scale);
            self.text_surface.damage_buffer(0, 0, cw as i32, ch as i32);
            let x = (w - cw / s) / 2;
            let y = (h - ch / s) / 2;
            self.text_sub.set_position(x as i32, y as i32);
        } else {
            self.text_surface.attach(None, 0, 0);
        }
        // Subsurfaces are synchronized, so these apply with the parent commit.
        self.image_layer.surface.commit();
        self.overlay.surface.commit();
        self.text_surface.commit();
        surface.commit();
    }

    /// Places the idle image for the current window size and fit mode.
    fn layout_image(&mut self) {
        let Layer {
            surface,
            sub,
            viewport,
        } = &self.image_layer;
        let Some(image) = &mut self.idle_image else {
            surface.attach(None, 0, 0);
            return;
        };
        if self.config.idle_image_fit == Fit::Tile {
            // One image pixel per buffer pixel, repeated from the top left.
            let s = self.scale as u32;
            let (bw, bh) = (self.size.0 * s, self.size.1 * s);
            if bw <= image.width && bh <= image.height {
                // An image that covers the window tiles as a plain crop.
                image.tile_pool = None;
                surface.attach(Some(&image.view((0, 0, bw, bh), &self.qh)), 0, 0);
            } else {
                let shm = &self.shm;
                let new_pool = || SlotPool::new(4096, shm).expect("shm pool");
                let pool = image.tile_pool.get_or_insert_with(new_pool);
                let (buffer, canvas) = pool
                    .create_buffer(
                        bw as i32,
                        bh as i32,
                        bw as i32 * 4,
                        wl_shm::Format::Argb8888,
                    )
                    .expect("create buffer");
                let (iw, ih) = (image.width as usize, image.height as usize);
                let src = &image.pool.mmap()[..iw * ih * 4];
                for (y, row) in canvas.chunks_exact_mut(bw as usize * 4).enumerate() {
                    let line = &src[(y % ih) * iw * 4..][..iw * 4];
                    for chunk in row.chunks_mut(iw * 4) {
                        chunk.copy_from_slice(&line[..chunk.len()]);
                    }
                }
                buffer.attach_to(surface).expect("attach");
            }
            surface.set_buffer_scale(self.scale);
            viewport.set_destination(-1, -1);
            sub.set_position(0, 0);
            surface.damage_buffer(0, 0, bw as i32, bh as i32);
        } else {
            // Crops are buffer views into the image, never a viewport source:
            // COSMIC clips surfaces with an offset source wrongly, hiding
            // most of the image.
            image.tile_pool = None;
            let image_size = (image.width, image.height);
            let (rect, dest) = place(self.config.idle_image_fit, self.size, image_size);
            surface.attach(Some(&image.view(rect, &self.qh)), 0, 0);
            surface.set_buffer_scale(1);
            viewport.set_destination(dest.2 as i32, dest.3 as i32);
            sub.set_position(dest.0 as i32, dest.1 as i32);
            surface.damage_buffer(0, 0, rect.2 as i32, rect.3 as i32);
        }
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
            self.text = Text::default();
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

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}

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

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
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
        self.floating = !state
            .intersects(WindowState::TILED | WindowState::MAXIMIZED | WindowState::FULLSCREEN);
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
            self.seat = Some(seat);
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer
            && let Some(pointer) = self.pointer.take()
        {
            pointer.release();
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
                // Left drag hands the move to the compositor; right click closes.
                PointerEventKind::Press {
                    button: BTN_LEFT,
                    serial,
                    ..
                } => {
                    if let Some(seat) = &self.seat {
                        self.window.move_(seat, serial);
                    }
                }
                PointerEventKind::Release {
                    button: BTN_RIGHT, ..
                } => self.exit = true,
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
delegate_noop!(State: ignore wl_buffer::WlBuffer);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement() {
        // A 400x200 image in a 100x100 window.
        let img = (400, 200);
        assert_eq!(
            place(Fit::Zoom, (100, 100), img),
            ((0, 0, 400, 200), (0, 25, 100, 50))
        );
        assert_eq!(
            place(Fit::Fill, (100, 100), img),
            ((100, 0, 200, 200), (0, 0, 100, 100))
        );
        assert_eq!(
            place(Fit::Stretch, (100, 100), img),
            ((0, 0, 400, 200), (0, 0, 100, 100))
        );
        // A tall window crops the sides; a wide one crops top and bottom.
        assert_eq!(place(Fit::Fill, (100, 400), img).0, (175, 0, 50, 200));
        assert_eq!(place(Fit::Fill, (800, 100), img).0, (0, 75, 400, 50));
    }
}
