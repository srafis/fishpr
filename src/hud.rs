//! A small "Recording…" pill at the bottom of the screen, drawn as a
//! wlr-layer-shell overlay (KWin supports it). It never takes focus and lets
//! clicks through. Runs on its own thread with its own Wayland connection.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop,
            channel::{self, Channel, Sender},
        },
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, QueueHandle,
            globals::registry_queue_init,
            protocol::{wl_output, wl_shm, wl_surface},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Transform};

const FONT: &[u8] = include_bytes!("../assets/NotoSans-Medium.ttf");
const LABEL: &str = "Recording…";
const HEIGHT: u32 = 44;
const FONT_SIZE: f32 = 17.0;
const BOTTOM_MARGIN: i32 = 96;

pub struct Hud {
    tx: Sender<bool>,
}

impl Hud {
    /// Starts the HUD thread. If Wayland or layer-shell isn't available, the
    /// HUD is silently disabled; the rest of the app works without it.
    pub fn spawn() -> Self {
        let (tx, rx) = channel::channel();
        std::thread::spawn(move || {
            if let Err(e) = run(rx) {
                eprintln!("fishpr: HUD disabled: {e:#}");
            }
        });
        Self { tx }
    }

    pub fn show(&self) {
        let _ = self.tx.send(true);
    }

    pub fn hide(&self) {
        let _ = self.tx.send(false);
    }
}

fn run(rx: Channel<bool>) -> anyhow::Result<()> {
    let conn = Connection::connect_to_env()?;
    let (globals, event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();
    let mut event_loop: EventLoop<HudState> = EventLoop::try_new()?;
    WaylandSource::new(conn.clone(), event_queue).insert(event_loop.handle()).map_err(|e| anyhow::anyhow!("{e}"))?;
    event_loop
        .handle()
        .insert_source(rx, |event, _, state| {
            if let channel::Event::Msg(show) = event {
                if show { state.show() } else { state.hide() }
            }
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let mut state = HudState {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        compositor: CompositorState::bind(&globals, &qh)?,
        layer_shell: LayerShell::bind(&globals, &qh)?,
        shm: Shm::bind(&globals, &qh)?,
        qh,
        conn,
        pool: None,
        layer: None,
        scale: 1,
        font: FontRef::try_from_slice(FONT)?,
    };
    loop {
        event_loop.dispatch(None, &mut state)?;
    }
}

struct HudState {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    qh: QueueHandle<Self>,
    conn: Connection,
    pool: Option<SlotPool>,
    layer: Option<LayerSurface>,
    scale: i32,
    font: FontRef<'static>,
}

impl HudState {
    fn width(&self) -> u32 {
        // Dot, gap, then the label, with equal padding on both ends.
        let font = self.font.as_scaled(PxScale::from(FONT_SIZE));
        let text: f32 = LABEL.chars().map(|c| font.h_advance(font.glyph_id(c))).sum();
        (20.0 + 10.0 + 8.0 + text + 22.0).ceil() as u32
    }

    fn show(&mut self) {
        if self.layer.is_some() {
            return;
        }
        let surface = self.compositor.create_surface(&self.qh);
        // An empty input region makes the HUD click-through.
        if let Ok(region) = Region::new(&self.compositor) {
            surface.set_input_region(Some(region.wl_region()));
        }
        let layer = self.layer_shell.create_layer_surface(&self.qh, surface, Layer::Overlay, Some("fishpr-hud"), None);
        layer.set_anchor(Anchor::BOTTOM);
        layer.set_margin(0, 0, BOTTOM_MARGIN, 0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_exclusive_zone(-1);
        layer.set_size(self.width(), HEIGHT);
        // First commit has no buffer; we draw once the compositor configures us.
        layer.commit();
        self.layer = Some(layer);
        let _ = self.conn.flush();
    }

    fn hide(&mut self) {
        self.layer = None; // dropping the layer surface unmaps and destroys it
        let _ = self.conn.flush();
    }

    fn draw(&mut self) {
        let Some(layer) = &self.layer else { return };
        let (w, h, s) = (self.width(), HEIGHT, self.scale.max(1) as u32);
        let Some(mut pixmap) = Pixmap::new(w * s, h * s) else { return };
        let t = Transform::from_scale(s as f32, s as f32);

        let mut paint = Paint { anti_alias: true, ..Default::default() };
        paint.set_color(Color::from_rgba8(24, 26, 32, 235));
        if let Some(pill) = rounded_rect(w as f32, h as f32, h as f32 / 2.0) {
            pixmap.fill_path(&pill, &paint, FillRule::Winding, t, None);
        }
        // A faint outline keeps the pill visible over dark windows.
        if let Some(outline) = rounded_rect(w as f32 - 1.0, h as f32 - 1.0, (h as f32 - 1.0) / 2.0) {
            let mut stroke_paint = Paint { anti_alias: true, ..Default::default() };
            stroke_paint.set_color(Color::from_rgba8(255, 255, 255, 46));
            let stroke = tiny_skia::Stroke { width: 1.0, ..Default::default() };
            pixmap.stroke_path(&outline, &stroke_paint, &stroke, t.pre_translate(0.5, 0.5), None);
        }
        paint.set_color(Color::from_rgba8(230, 36, 48, 255));
        if let Some(dot) = PathBuilder::from_circle(20.0 + 4.0, h as f32 / 2.0, 6.0) {
            pixmap.fill_path(&dot, &paint, FillRule::Winding, t, None);
        }
        draw_text(&mut pixmap, &self.font, LABEL, 20.0 + 10.0 + 8.0, h as f32 / 2.0, s as f32);

        let pool = match &mut self.pool {
            Some(pool) => pool,
            None => match SlotPool::new((w * s * h * s * 4) as usize, &self.shm) {
                Ok(pool) => self.pool.insert(pool),
                Err(_) => return,
            },
        };
        let stride = (w * s * 4) as i32;
        let Ok((buffer, canvas)) = pool.create_buffer((w * s) as i32, (h * s) as i32, stride, wl_shm::Format::Argb8888) else {
            return;
        };
        // tiny-skia is premultiplied RGBA; wl_shm ARGB8888 is premultiplied BGRA in memory.
        for (dst, src) in canvas.chunks_exact_mut(4).zip(pixmap.data().chunks_exact(4)) {
            dst.copy_from_slice(&[src[2], src[1], src[0], src[3]]);
        }
        let surface = layer.wl_surface();
        surface.set_buffer_scale(s as i32);
        surface.damage_buffer(0, 0, (w * s) as i32, (h * s) as i32);
        if buffer.attach_to(surface).is_ok() {
            layer.commit();
        }
        let _ = self.conn.flush();
    }
}

fn rounded_rect(w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let rect = Rect::from_xywh(0.0, 0.0, w, h)?;
    let mut pb = PathBuilder::new();
    let k = 0.552_284_8 * r; // cubic approximation of a quarter circle
    let (l, t, rt, b) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    pb.move_to(l + r, t);
    pb.line_to(rt - r, t);
    pb.cubic_to(rt - r + k, t, rt, t + r - k, rt, t + r);
    pb.line_to(rt, b - r);
    pb.cubic_to(rt, b - r + k, rt - r + k, b, rt - r, b);
    pb.line_to(l + r, b);
    pb.cubic_to(l + r - k, b, l, b - r + k, l, b - r);
    pb.line_to(l, t + r);
    pb.cubic_to(l, t + r - k, l + r - k, t, l + r, t);
    pb.close();
    pb.finish()
}

/// Draws white text with its vertical center at `cy` (logical px).
fn draw_text(pixmap: &mut Pixmap, font: &FontRef, text: &str, x: f32, cy: f32, scale: f32) {
    let font = font.as_scaled(PxScale::from(FONT_SIZE * scale));
    let baseline = cy * scale + (font.ascent() + font.descent()) / 2.0;
    let mut caret = x * scale;
    let width = pixmap.width() as i32;
    let height = pixmap.height() as i32;
    let pixels = pixmap.data_mut();
    for c in text.chars() {
        let id = font.glyph_id(c);
        let glyph = id.with_scale_and_position(font.scale(), ab_glyph::point(caret, baseline));
        caret += font.h_advance(id);
        let Some(outline) = font.outline_glyph(glyph) else { continue };
        let bounds = outline.px_bounds();
        outline.draw(|gx, gy, coverage| {
            let (px, py) = (bounds.min.x as i32 + gx as i32, bounds.min.y as i32 + gy as i32);
            if px < 0 || py < 0 || px >= width || py >= height {
                return;
            }
            let i = ((py * width + px) * 4) as usize;
            let a = coverage.clamp(0.0, 1.0);
            for ch in &mut pixels[i..i + 4] {
                // Source is opaque white, premultiplied: blend every channel toward 255.
                *ch = (255.0 * a + *ch as f32 * (1.0 - a)).round() as u8;
            }
        });
    }
}

impl CompositorHandler for HudState {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, factor: i32) {
        if factor != self.scale {
            self.scale = factor;
            self.draw();
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for HudState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for HudState {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.layer = None;
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface, _: LayerSurfaceConfigure, _: u32) {
        self.draw();
    }
}

impl ShmHandler for HudState {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(HudState);

impl ProvidesRegistryState for HudState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

smithay_client_toolkit::delegate_dispatch2!(HudState);
