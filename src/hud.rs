//! A small "Recording…" pill at the bottom of the screen, drawn as a
//! wlr-layer-shell overlay (KWin supports it, GNOME doesn't). It never takes
//! focus and lets clicks through. Runs on its own thread with its own Wayland
//! connection.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData, Region},
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
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use tiny_skia::{Color, FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, Point, Rect, SpreadMode, Transform};

const FONT: &[u8] = include_bytes!("../assets/NotoSans-Medium.ttf");
const LABEL: &str = "Recording…";
const HEIGHT: u32 = 44;
const FONT_SIZE: f32 = 17.0;
const BOTTOM_MARGIN: i32 = 96;
/// Transparent border around the pill, leaving room for the drop shadow.
const SHADOW: f32 = 12.0;
const PAD: f32 = 20.0;
const BARS: usize = 5;
const BAR_W: f32 = 3.0;
const BAR_GAP: f32 = 3.0;
const BARS_W: f32 = BARS as f32 * BAR_W + (BARS - 1) as f32 * BAR_GAP;
const BAR_MIN_H: f32 = 4.0;
const BAR_MAX_H: f32 = 16.0;
const PULSE_PERIOD: f32 = 1.4;
/// How fast the meter follows the input level, per second. It rises quickly
/// with speech and falls back more slowly, like a VU meter.
const METER_ATTACK: f32 = 30.0;
const METER_RELEASE: f32 = 8.0;

enum Msg {
    Show,
    Hide,
    Level(f32),
}

#[derive(Clone)]
pub struct Hud {
    tx: Sender<Msg>,
    running: Arc<AtomicBool>,
}

impl Hud {
    /// Starts the HUD thread. If Wayland or layer-shell isn't available, the
    /// HUD is disabled and `is_running` turns false; the rest of the app works
    /// without it.
    pub fn spawn() -> Self {
        let (tx, rx) = channel::channel();
        let running = Arc::new(AtomicBool::new(true));
        std::thread::spawn({
            let running = running.clone();
            move || {
                if let Err(e) = run(rx) {
                    eprintln!("fishpr: HUD disabled: {e:#}");
                    running.store(false, Ordering::Relaxed);
                }
            }
        });
        Self { tx, running }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn show(&self) {
        let _ = self.tx.send(Msg::Show);
    }

    pub fn hide(&self) {
        let _ = self.tx.send(Msg::Hide);
    }

    /// Sets the mic level the bars show, from 0.0 (flat) to 1.0 (full height).
    pub fn set_level(&self, level: f32) {
        let _ = self.tx.send(Msg::Level(level));
    }
}

fn run(rx: Channel<Msg>) -> anyhow::Result<()> {
    let conn = Connection::connect_to_env()?;
    let (globals, event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();
    let mut event_loop: EventLoop<HudState> = EventLoop::try_new()?;
    WaylandSource::new(conn.clone(), event_queue).insert(event_loop.handle()).map_err(|e| anyhow::anyhow!("{e}"))?;
    event_loop
        .handle()
        .insert_source(rx, |event, _, state| {
            if let channel::Event::Msg(msg) = event {
                match msg {
                    Msg::Show => state.show(),
                    Msg::Hide => state.hide(),
                    Msg::Level(level) => state.target_level = level,
                }
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
        started: Instant::now(),
        last_frame: Instant::now(),
        frame_pending: false,
        level: 0.0,
        target_level: 0.0,
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
    started: Instant,
    last_frame: Instant,
    frame_pending: bool,
    /// The level the bars currently show, easing toward `target_level`.
    level: f32,
    /// The latest mic level from the recorder.
    target_level: f32,
    font: FontRef<'static>,
}

impl HudState {
    fn label_width(&self) -> f32 {
        let font = self.font.as_scaled(PxScale::from(FONT_SIZE));
        LABEL.chars().map(|c| font.h_advance(font.glyph_id(c))).sum()
    }

    /// Width of the pill itself: dot, label, then the level bars, with equal padding.
    fn pill_width(&self) -> f32 {
        (PAD + 14.0 + self.label_width() + 14.0 + BARS_W + PAD).ceil()
    }

    /// Surface size: the pill plus the shadow border on every side.
    fn surface_size(&self) -> (u32, u32) {
        let border = (SHADOW * 2.0) as u32;
        (self.pill_width() as u32 + border, HEIGHT + border)
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
        layer.set_margin(0, 0, BOTTOM_MARGIN - SHADOW as i32, 0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_exclusive_zone(-1);
        let (w, h) = self.surface_size();
        layer.set_size(w, h);
        self.started = Instant::now();
        self.last_frame = self.started;
        self.frame_pending = false;
        (self.level, self.target_level) = (0.0, 0.0);
        // First commit has no buffer; we draw once the compositor configures us.
        layer.commit();
        self.layer = Some(layer);
        let _ = self.conn.flush();
    }

    fn hide(&mut self) {
        self.layer = None; // dropping the layer surface unmaps and destroys it
        self.frame_pending = false;
        let _ = self.conn.flush();
    }

    fn draw(&mut self) {
        let Some(layer) = &self.layer else { return };
        let (w, h) = self.surface_size();
        let s = self.scale.max(1) as u32;
        let Some(mut pixmap) = Pixmap::new(w * s, h * s) else { return };
        let t = Transform::from_scale(s as f32, s as f32);
        let time = self.started.elapsed().as_secs_f32();
        let dt = self.last_frame.elapsed().as_secs_f32();
        self.last_frame = Instant::now();
        let rate = if self.target_level > self.level { METER_ATTACK } else { METER_RELEASE };
        self.level += (self.target_level - self.level) * (rate * dt).min(1.0);
        let (pw, ph) = (self.pill_width(), HEIGHT as f32);
        let cy = ph / 2.0;
        let pill_t = t.pre_translate(SHADOW, SHADOW);

        // Soft drop shadow: stacked, slightly offset pills of fading opacity.
        let mut shadow = Paint { anti_alias: true, ..Default::default() };
        shadow.set_color(Color::from_rgba8(0, 0, 0, 9));
        for i in 1..=10 {
            let e = i as f32 * 1.1;
            if let Some(path) = rounded_rect(pw + 2.0 * e, ph + 2.0 * e, ph / 2.0 + e) {
                pixmap.fill_path(&path, &shadow, FillRule::Winding, t.pre_translate(SHADOW - e, SHADOW - e + 3.0), None);
            }
        }

        // Pill body: a subtle vertical gradient.
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        paint.shader = LinearGradient::new(
            Point::from_xy(0.0, 0.0),
            Point::from_xy(0.0, ph),
            vec![
                GradientStop::new(0.0, Color::from_rgba8(40, 42, 54, 240)),
                GradientStop::new(1.0, Color::from_rgba8(20, 21, 28, 240)),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        )
        .unwrap_or(tiny_skia::Shader::SolidColor(Color::from_rgba8(24, 26, 32, 240)));
        if let Some(pill) = rounded_rect(pw, ph, ph / 2.0) {
            pixmap.fill_path(&pill, &paint, FillRule::Winding, pill_t, None);
        }
        // A faint outline keeps the pill visible over dark windows.
        if let Some(outline) = rounded_rect(pw - 1.0, ph - 1.0, (ph - 1.0) / 2.0) {
            let mut stroke_paint = Paint { anti_alias: true, ..Default::default() };
            stroke_paint.set_color(Color::from_rgba8(255, 255, 255, 40));
            let stroke = tiny_skia::Stroke { width: 1.0, ..Default::default() };
            pixmap.stroke_path(&outline, &stroke_paint, &stroke, pill_t.pre_translate(0.5, 0.5), None);
        }

        // Recording dot with an expanding, fading halo.
        let dot_x = PAD + 7.0;
        let phase = (time % PULSE_PERIOD) / PULSE_PERIOD;
        let ease = 1.0 - (1.0 - phase) * (1.0 - phase);
        paint.shader = tiny_skia::Shader::SolidColor(Color::from_rgba8(255, 69, 82, (110.0 * (1.0 - ease)) as u8));
        if let Some(halo) = PathBuilder::from_circle(dot_x, cy, 6.0 + 7.0 * ease) {
            pixmap.fill_path(&halo, &paint, FillRule::Winding, pill_t, None);
        }
        paint.shader = tiny_skia::Shader::SolidColor(Color::from_rgba8(255, 69, 82, 255));
        if let Some(dot) = PathBuilder::from_circle(dot_x, cy, 5.5) {
            pixmap.fill_path(&dot, &paint, FillRule::Winding, pill_t, None);
        }

        let label_x = dot_x + 14.0;
        draw_text(&mut pixmap, &self.font, LABEL, SHADOW + label_x, SHADOW + cy, s as f32);

        // Level bars driven by the mic. Their height scales with loudness, and
        // each bar wobbles a little so speech looks alive; with no input (silence
        // or a muted mic) they lie flat and dim.
        let level = self.level;
        paint.shader = tiny_skia::Shader::SolidColor(Color::from_rgba8(255, 255, 255, (90.0 + 110.0 * level) as u8));
        let bars_x = label_x + self.label_width() + 14.0;
        for i in 0..BARS {
            let f = i as f32;
            let wave = 0.5 * (time * 5.3 + f * 1.4).sin() + 0.5 * (time * 3.1 + f * 2.3).sin();
            let bar_h = BAR_MIN_H + (BAR_MAX_H - BAR_MIN_H) * level * (0.75 + 0.25 * wave);
            let x = bars_x + f * (BAR_W + BAR_GAP);
            if let Some(bar) = rounded_rect(BAR_W, bar_h, BAR_W / 2.0) {
                pixmap.fill_path(&bar, &paint, FillRule::Winding, pill_t.pre_translate(x, cy - bar_h / 2.0), None);
            }
        }

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
        // Ask for a callback so the next frame of the animation gets drawn.
        if !self.frame_pending {
            surface.frame(&self.qh, FrameCallbackData(surface.clone()));
            self.frame_pending = true;
        }
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
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
        self.draw();
    }
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
