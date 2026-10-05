//! A black pill at the bottom of the screen, drawn as a wlr-layer-shell
//! overlay (KWin supports it). While recording, its bars follow the mic; while
//! transcribing, a spinner joins them; if that fails, a retry button takes the
//! spinner's place for a few seconds. It never takes focus, and only the retry
//! button takes clicks. Runs on its own thread with its own Wayland connection.

use ab_glyph::{Font, FontRef, OutlineCurve, PxScale, ScaleFont};
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
            protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
        },
        protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1},
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{PointerEvent, PointerEventKind, PointerHandler, cursor_shape::CursorShapeManager},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use std::{
    f32::consts::{FRAC_PI_2, TAU},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use tiny_skia::{Color, FillRule, LineCap, Paint, Path, PathBuilder, Pixmap, Rect, Stroke, Transform};
use tokio::sync::mpsc::UnboundedSender;

use crate::Event;

const FONT: &[u8] = include_bytes!("../assets/NotoSans-Medium.ttf");
const NO_SPEECH: &str = "No speech detected";

// Sizes are in logical pixels.
const FONT_SIZE: f32 = 16.0;
const HEIGHT: f32 = 33.0;
/// Space between each end of the pill and the bars, while recording.
const PAD: f32 = 16.5;
const BARS: usize = 10;
const BAR_W: f32 = 3.0;
const BAR_GAP: f32 = 3.0;
const BARS_W: f32 = BARS as f32 * BAR_W + (BARS - 1) as f32 * BAR_GAP;
/// Bars rest as dots, and grow to this with loud speech.
const BAR_MAX_H: f32 = 18.0;
/// The spinner or retry button: a circle tucked into the pill's right end.
const SLOT: f32 = 21.0;
const SLOT_GAP: f32 = 10.5;
const SLOT_INSET: f32 = (HEIGHT - SLOT) / 2.0;
/// What the slot adds to the pill's width.
const SLOT_W: f32 = SLOT_GAP + SLOT + SLOT_INSET - PAD;
/// Transparent room around the pill, for the entrance bounce.
const MARGIN: f32 = 12.0;
const SURFACE_H: u32 = (HEIGHT + 2.0 * MARGIN) as u32;
/// From the bottom of the screen to the bottom of the pill.
const BOTTOM_MARGIN: i32 = 96;
const ENTER_SECS: f32 = 0.45;
const LEAVE_SECS: f32 = 0.18;
/// How long the retry button stays, not counting while the pointer is on it.
const RETRY_SECS: f32 = 10.0;
/// How fast the pill reshapes between states, per second.
const MORPH_RATE: f32 = 14.0;
/// How fast the meter follows the input level, per second. It rises quickly
/// with speech and falls back more slowly, like a VU meter.
const METER_ATTACK: f32 = 30.0;
const METER_RELEASE: f32 = 8.0;
/// Linux's BTN_LEFT.
const BUTTON_LEFT: u32 = 0x110;

enum Msg {
    Show,
    Busy,
    Fail { no_speech: bool },
    Hide,
    Level(f32),
}

#[derive(Clone)]
pub struct Hud {
    tx: Sender<Msg>,
    available: Arc<AtomicBool>,
}

impl Hud {
    /// Starts the HUD thread; its retry button sends `Event::Retry`. If
    /// Wayland or layer-shell isn't available, the HUD is disabled and
    /// `is_available` says so; the rest of the app works without it.
    pub fn spawn(events: UnboundedSender<Event>) -> Self {
        let (tx, rx) = channel::channel();
        let available = Arc::new(AtomicBool::new(true));
        std::thread::spawn({
            let available = available.clone();
            move || {
                if let Err(e) = run(rx, events) {
                    eprintln!("fishpr: HUD disabled: {e:#}");
                    available.store(false, Ordering::Relaxed);
                }
            }
        });
        Self { tx, available }
    }

    pub fn is_available(&self) -> bool {
        self.available.load(Ordering::Relaxed)
    }

    /// Shows the level bars.
    pub fn show(&self) {
        let _ = self.tx.send(Msg::Show);
    }

    /// Adds a spinner to the bars.
    pub fn busy(&self) {
        let _ = self.tx.send(Msg::Busy);
    }

    /// Offers a retry button for a few seconds, then hides. With
    /// `no_speech`, says so next to the button, in place of the bars.
    pub fn fail(&self, no_speech: bool) {
        let _ = self.tx.send(Msg::Fail { no_speech });
    }

    pub fn hide(&self) {
        let _ = self.tx.send(Msg::Hide);
    }

    /// Sets the mic level the bars show, from 0.0 (flat) to 1.0 (full height).
    pub fn set_level(&self, level: f32) {
        let _ = self.tx.send(Msg::Level(level));
    }
}

fn run(rx: Channel<Msg>, events: UnboundedSender<Event>) -> anyhow::Result<()> {
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
                    Msg::Show => state.set_mode(Mode::Recording),
                    Msg::Busy => state.set_mode(Mode::Busy),
                    Msg::Fail { no_speech } => {
                        state.no_speech = no_speech;
                        state.set_mode(Mode::Failed);
                    }
                    Msg::Hide => state.hide(),
                    Msg::Level(level) => state.target_level = level,
                }
            }
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let label = Label::new(&FontRef::try_from_slice(FONT)?, NO_SPEECH);
    let mut state = HudState {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        compositor: CompositorState::bind(&globals, &qh)?,
        layer_shell: LayerShell::bind(&globals, &qh)?,
        shm: Shm::bind(&globals, &qh)?,
        cursor_shapes: CursorShapeManager::bind(&globals, &qh).ok(),
        qh,
        conn,
        events,
        pool: None,
        layer: None,
        pointer: None,
        scale: 1,
        mode: Mode::Recording,
        phase: Phase::Shown,
        since: Instant::now(),
        started: Instant::now(),
        last_frame: Instant::now(),
        frame_pending: false,
        level: 0.0,
        target_level: 0.0,
        open: 0.0,
        busy: 0.0,
        failed: 0.0,
        labelled: 0.0,
        no_speech: false,
        countdown: 0.0,
        hovered: false,
        on_button: false,
        enter_serial: 0,
        label,
    };
    loop {
        event_loop.dispatch(None, &mut state)?;
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Recording,
    Busy,
    Failed,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Entering,
    Shown,
    Leaving,
}

struct HudState {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    /// Lets the retry button show a hand cursor, where the compositor supports it.
    cursor_shapes: Option<CursorShapeManager>,
    qh: QueueHandle<Self>,
    conn: Connection,
    events: UnboundedSender<Event>,
    pool: Option<SlotPool>,
    layer: Option<LayerSurface>,
    pointer: Option<(wl_pointer::WlPointer, Option<WpCursorShapeDeviceV1>)>,
    scale: i32,
    mode: Mode,
    phase: Phase,
    /// When the current phase began.
    since: Instant,
    /// When the pill appeared; the clock for the looping animations.
    started: Instant,
    last_frame: Instant,
    frame_pending: bool,
    /// The level the bars currently show, easing toward `target_level`.
    level: f32,
    /// The latest mic level from the recorder.
    target_level: f32,
    /// How far the pill has widened to make room for the slot, 0.0–1.0.
    open: f32,
    /// How much the slot shows the spinner, and the retry button, 0.0–1.0.
    busy: f32,
    failed: f32,
    /// How much the label shows in place of the bars, 0.0–1.0.
    labelled: f32,
    /// Whether the last failure was hearing no speech, which the label says.
    no_speech: bool,
    /// Seconds left before the retry button goes away.
    countdown: f32,
    /// Whether the pointer is on the pill, which pauses the countdown, and
    /// whether it's on the retry button.
    hovered: bool,
    on_button: bool,
    /// The pointer's latest entry onto the pill, which changing the cursor needs.
    enter_serial: u32,
    label: Label,
}

impl HudState {
    fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        if mode == Mode::Failed {
            self.countdown = RETRY_SECS;
        }
        match (&self.layer, self.phase) {
            (None, _) => self.create_layer(),
            (Some(_), Phase::Leaving) => self.set_phase(Phase::Entering),
            (Some(_), _) => {}
        }
        self.update_input_region();
    }

    fn set_phase(&mut self, phase: Phase) {
        self.phase = phase;
        self.since = Instant::now();
    }

    fn create_layer(&mut self) {
        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(&self.qh, surface, Layer::Overlay, Some("fishpr-hud"), None);
        layer.set_anchor(Anchor::BOTTOM);
        layer.set_margin(0, 0, BOTTOM_MARGIN - MARGIN as i32, 0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_exclusive_zone(-1);
        layer.set_size(self.surface_width(), SURFACE_H);
        self.set_phase(Phase::Entering);
        self.started = self.since;
        self.last_frame = self.since;
        self.frame_pending = false;
        (self.hovered, self.on_button) = (false, false);
        (self.level, self.target_level) = (0.0, 0.0);
        // Appear already in shape, rather than morphing while popping in.
        self.open = if self.mode == Mode::Recording { 0.0 } else { 1.0 };
        self.busy = if self.mode == Mode::Busy { 1.0 } else { 0.0 };
        self.failed = if self.mode == Mode::Failed { 1.0 } else { 0.0 };
        self.labelled = if self.showing_label() { 1.0 } else { 0.0 };
        // First commit has no buffer; we draw once the compositor configures us.
        layer.commit();
        self.layer = Some(layer);
        let _ = self.conn.flush();
    }

    fn showing_label(&self) -> bool {
        self.mode == Mode::Failed && self.no_speech
    }

    /// Wide enough for the widest pill, at the peak of its entrance bounce.
    fn surface_width(&self) -> u32 {
        (pill_width(BARS_W.max(self.label.width), 1.0) * 1.1 + 4.0).ceil() as u32
    }

    fn hide(&mut self) {
        if self.layer.is_some() && self.phase != Phase::Leaving {
            self.set_phase(Phase::Leaving);
            self.update_input_region();
        }
    }

    /// Where the failed pill sits once settled, centered in the surface: its
    /// left edge, its width, and the middle of its retry button.
    fn failed_layout(&self) -> (f32, f32, (f32, f32)) {
        let content = if self.no_speech { self.label.width } else { BARS_W };
        let pw = pill_width(content, 1.0);
        let left = (self.surface_width() as f32 - pw) / 2.0;
        (left, pw, (left + pw - SLOT_INSET - SLOT / 2.0, SURFACE_H as f32 / 2.0))
    }

    /// Clicks go through the HUD, except while it offers a retry: then the
    /// pill takes the pointer, so resting on it pauses the countdown.
    fn update_input_region(&mut self) {
        let Some(layer) = &self.layer else { return };
        let Ok(region) = Region::new(&self.compositor) else { return };
        if self.mode == Mode::Failed && self.phase != Phase::Leaving {
            let (left, pw, _) = self.failed_layout();
            let top = (SURFACE_H as f32 - HEIGHT) / 2.0;
            region.add(left as i32, top as i32, pw.ceil() as i32, HEIGHT as i32);
        } else {
            (self.hovered, self.on_button) = (false, false);
        }
        layer.wl_surface().set_input_region(Some(region.wl_region()));
    }

    /// Tracks whether the pointer, at `(x, y)` on the surface, is on the
    /// retry button, and shows a hand there.
    fn point_at(&mut self, (x, y): (f64, f64)) {
        let (_, _, (bx, by)) = self.failed_layout();
        let on_button = (x as f32 - bx).hypot(y as f32 - by) <= SLOT / 2.0 + 1.0;
        if on_button != self.on_button || !self.hovered {
            if let Some((_, Some(shape))) = &self.pointer {
                shape.set_shape(self.enter_serial, if on_button { Shape::Pointer } else { Shape::Default });
            }
        }
        (self.hovered, self.on_button) = (true, on_button);
    }

    fn draw(&mut self) {
        if self.layer.is_none() {
            return;
        }
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.1);
        self.last_frame = now;
        let time = (now - self.started).as_secs_f32();
        let elapsed = (now - self.since).as_secs_f32();

        // Pops up with a little bounce; shrinks and fades away.
        let (zoom, drop, opacity) = match self.phase {
            Phase::Entering => {
                let p = (elapsed / ENTER_SECS).min(1.0);
                if p >= 1.0 {
                    self.phase = Phase::Shown;
                }
                let e = bounce(p);
                (0.6 + 0.4 * e, 10.5 * (1.0 - e), (p * 4.0).min(1.0))
            }
            Phase::Shown => (1.0, 0.0, 1.0),
            Phase::Leaving => {
                let p = elapsed / LEAVE_SECS;
                if p >= 1.0 {
                    self.layer = None; // dropping the layer surface unmaps and destroys it
                    self.frame_pending = false;
                    let _ = self.conn.flush();
                    return;
                }
                (1.0 - 0.25 * p * p, 4.5 * p * p, 1.0 - p)
            }
        };

        if self.mode == Mode::Failed && self.phase != Phase::Leaving && !self.hovered {
            self.countdown -= dt;
            if self.countdown <= 0.0 {
                self.hide();
            }
        }
        // Once recording stops, the bars settle back to dots.
        let target_level = if self.mode == Mode::Recording { self.target_level } else { 0.0 };
        let rate = if target_level > self.level { METER_ATTACK } else { METER_RELEASE };
        self.level += (target_level - self.level) * (rate * dt).min(1.0);
        let morph = (MORPH_RATE * dt).min(1.0);
        self.open += (f32::from(self.mode != Mode::Recording) - self.open) * morph;
        self.busy += (f32::from(self.mode == Mode::Busy) - self.busy) * morph;
        self.failed += (f32::from(self.mode == Mode::Failed) - self.failed) * morph;
        self.labelled += (f32::from(self.showing_label()) - self.labelled) * morph;

        let (w, h) = (self.surface_width(), SURFACE_H);
        let s = self.scale.max(1) as u32;
        let Some(mut pixmap) = Pixmap::new(w * s, h * s) else { return };
        let look = Look {
            time,
            zoom,
            drop,
            level: self.level,
            open: self.open,
            busy: self.busy,
            failed: self.failed,
            labelled: self.labelled,
            left: (self.countdown / RETRY_SECS).clamp(0.0, 1.0),
            hovered: self.on_button,
        };
        paint_pill(&mut pixmap, s as f32, &look, &self.label);

        let Some(layer) = &self.layer else { return };
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
        // tiny-skia is premultiplied RGBA; wl_shm ARGB8888 is premultiplied BGRA
        // in memory. Fading scales all four channels.
        let fade = (opacity.clamp(0.0, 1.0) * 256.0) as u32;
        let fade = |c: u8| (u32::from(c) * fade >> 8) as u8;
        for (dst, src) in canvas.chunks_exact_mut(4).zip(pixmap.data().chunks_exact(4)) {
            dst.copy_from_slice(&[fade(src[2]), fade(src[1]), fade(src[0]), fade(src[3])]);
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

/// The pill's width around content `content` wide, with the slot `open`.
fn pill_width(content: f32, open: f32) -> f32 {
    PAD + content + PAD + SLOT_W * open
}

/// Text drawn as a path, so it scales and fades with the pill.
struct Label {
    /// Its baseline is at y = 0.
    path: Option<Path>,
    width: f32,
    /// How far the baseline sits below the middle of the text.
    middle: f32,
}

impl Label {
    fn new(font: &FontRef, text: &str) -> Self {
        let scaled = font.as_scaled(PxScale::from(FONT_SIZE));
        let (sx, sy) = (scaled.h_scale_factor(), scaled.v_scale_factor());
        let mut pb = PathBuilder::new();
        let mut caret = 0.0;
        let mut prev = None;
        for c in text.chars() {
            let id = font.glyph_id(c);
            if let Some(prev) = prev {
                caret += scaled.kern(prev, id);
            }
            prev = Some(id);
            // Outlines are in font units, with y pointing up.
            let at = |p: ab_glyph::Point| (caret + p.x * sx, -p.y * sy);
            let mut pen = None;
            for curve in font.outline(id).map(|o| o.curves).unwrap_or_default() {
                let (from, to) = match curve {
                    OutlineCurve::Line(a, b) | OutlineCurve::Quad(a, _, b) | OutlineCurve::Cubic(a, _, _, b) => (a, b),
                };
                if pen != Some(from) {
                    if pen.is_some() {
                        pb.close();
                    }
                    let (x, y) = at(from);
                    pb.move_to(x, y);
                }
                match curve {
                    OutlineCurve::Line(_, b) => {
                        let (x, y) = at(b);
                        pb.line_to(x, y);
                    }
                    OutlineCurve::Quad(_, c, b) => {
                        let ((cx, cy), (x, y)) = (at(c), at(b));
                        pb.quad_to(cx, cy, x, y);
                    }
                    OutlineCurve::Cubic(_, c1, c2, b) => {
                        let ((c1x, c1y), (c2x, c2y), (x, y)) = (at(c1), at(c2), at(b));
                        pb.cubic_to(c1x, c1y, c2x, c2y, x, y);
                    }
                }
                pen = Some(to);
            }
            if pen.is_some() {
                pb.close();
            }
            caret += scaled.h_advance(id);
        }
        Self { path: pb.finish(), width: caret, middle: (scaled.ascent() + scaled.descent()) / 2.0 }
    }
}

/// One frame of the pill's animation. The mixes run from 0.0 to 1.0.
struct Look {
    /// Seconds since the pill appeared, for the looping animations.
    time: f32,
    /// The entrance and exit: size, and how far below its place the pill is.
    zoom: f32,
    drop: f32,
    level: f32,
    /// How far the pill has widened for the slot.
    open: f32,
    /// How much the slot shows the spinner, and the retry button.
    busy: f32,
    failed: f32,
    /// How much the label shows in place of the bars.
    labelled: f32,
    /// How much of the retry window is left.
    left: f32,
    /// Whether the pointer is on the retry button.
    hovered: bool,
}

/// Paints the pill, centered in `pixmap`, at `scale` device pixels per logical pixel.
fn paint_pill(pixmap: &mut Pixmap, scale: f32, look: &Look, label: &Label) {
    let Look { time, zoom, drop, level, open, busy, failed, labelled, left, hovered } = *look;
    let pw = pill_width(BARS_W + (label.width - BARS_W) * labelled, open);
    let cy = HEIGHT / 2.0;
    // Pill coordinates: the pill's top-left corner is (0, 0).
    let t = Transform::from_scale(scale, scale)
        .pre_translate(pixmap.width() as f32 / scale / 2.0, pixmap.height() as f32 / scale / 2.0 + drop)
        .pre_scale(zoom, zoom)
        .pre_translate(-pw / 2.0, -cy);

    if let Some(pill) = rounded_rect(pw, HEIGHT, HEIGHT / 2.0) {
        pixmap.fill_path(&pill, &paint(10, 10, 10, 0.94), FillRule::Winding, t, None);
    }
    // A thin dark rim keeps the pill's edge visible over black windows.
    if let Some(rim) = rounded_rect(pw - 1.0, HEIGHT - 1.0, (HEIGHT - 1.0) / 2.0) {
        pixmap.stroke_path(&rim, &paint(48, 48, 47, 1.0), &Stroke { width: 1.0, ..Default::default() }, t.pre_translate(0.5, 0.5), None);
    }

    // Level bars, tallest in the middle. Their height follows the mic, and
    // each wobbles a little so speech looks alive; in silence they rest as
    // dots. While transcribing, a ripple runs along them; on failure they
    // turn red.
    let bars = 1.0 - labelled;
    if bars > 0.01 {
        let mid = (BARS - 1) as f32 / 2.0;
        let (r, g, b) = mix((255, 255, 255), (255, 92, 92), failed);
        let bar_paint = paint(r, g, b, (0.55 + 0.45 * level + 0.25 * failed) * bars);
        for i in 0..BARS {
            let f = i as f32;
            let d = f - mid;
            let wave = 0.5 * (time * 5.3 + f * 1.4).sin() + 0.5 * (time * 3.1 + f * 2.3).sin();
            let speech = level * (1.0 - 0.5 * (d / mid).powi(2)) * (0.75 + 0.25 * wave);
            let ripple = busy * 0.3 * (time * 7.0 - f * 0.6).sin().max(0.0).powi(3);
            let bar_h = BAR_W + (BAR_MAX_H - BAR_W) * (speech + ripple).min(1.0);
            let x = PAD + f * (BAR_W + BAR_GAP);
            if let Some(bar) = rounded_rect(BAR_W, bar_h, BAR_W / 2.0) {
                pixmap.fill_path(&bar, &bar_paint, FillRule::Winding, t.pre_translate(x, cy - bar_h / 2.0), None);
            }
        }
    }
    if let Some(text) = label.path.as_ref().filter(|_| labelled > 0.01) {
        pixmap.fill_path(text, &paint(255, 255, 255, 0.92 * labelled), FillRule::Winding, t.pre_translate(PAD, cy + label.middle), None);
    }

    let (sx, sy) = (pw - SLOT_INSET - SLOT / 2.0, cy);
    let round = |width| Stroke { width, line_cap: LineCap::Round, ..Default::default() };

    // Spinner: a short arc running around a faint track.
    let a = busy * open;
    if a > 0.01 {
        if let Some(track) = PathBuilder::from_circle(sx, sy, 6.0) {
            pixmap.stroke_path(&track, &paint(255, 255, 255, 0.18 * a), &round(1.9), t, None);
        }
        if let Some(spin) = arc(sx, sy, 6.0, time * TAU * 1.1, 1.9) {
            pixmap.stroke_path(&spin, &paint(255, 255, 255, 0.95 * a), &round(1.9), t, None);
        }
    }

    // Retry button, ringed by the time left to press it.
    let a = failed * open;
    if a > 0.01 {
        if let Some(button) = PathBuilder::from_circle(sx, sy, SLOT / 2.0) {
            let fill = if hovered { 0.24 } else { 0.12 };
            pixmap.fill_path(&button, &paint(255, 255, 255, fill * a), FillRule::Winding, t, None);
        }
        if let Some(ring) = arc(sx, sy, SLOT / 2.0 - 0.95, -FRAC_PI_2, TAU * left) {
            pixmap.stroke_path(&ring, &paint(255, 255, 255, 0.9 * a), &round(1.9), t, None);
        }
        // A clockwise arrow, its head in the gap at the top.
        let (start, sweep, radius) = (-FRAC_PI_2 + 0.9, TAU - 1.2, 4.5);
        if let Some(curve) = arc(sx, sy, radius, start, sweep) {
            pixmap.stroke_path(&curve, &paint(255, 255, 255, a), &round(1.5), t, None);
        }
        let end = start + sweep;
        let (tip_x, tip_y) = (sx + radius * end.cos(), sy + radius * end.sin());
        let (tx, ty) = (-end.sin(), end.cos()); // direction of travel
        let (nx, ny) = (end.cos(), end.sin()); // outward
        let mut head = PathBuilder::new();
        head.move_to(tip_x + 2.25 * tx, tip_y + 2.25 * ty);
        head.line_to(tip_x + 2.4 * nx - 0.75 * tx, tip_y + 2.4 * ny - 0.75 * ty);
        head.line_to(tip_x - 2.4 * nx - 0.75 * tx, tip_y - 2.4 * ny - 0.75 * ty);
        head.close();
        if let Some(head) = head.finish() {
            pixmap.fill_path(&head, &paint(255, 255, 255, a), FillRule::Winding, t, None);
        }
    }
}

/// Eases from 0.0 to 1.0 over `p` in 0.0–1.0, overshooting once and settling,
/// like a spring.
fn bounce(p: f32) -> f32 {
    1.0 - (-6.0 * p).exp() * (3.0 * std::f32::consts::PI * p).cos()
}

fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    (lerp(a.0, b.0), lerp(a.1, b.1), lerp(a.2, b.2))
}

fn paint(r: u8, g: u8, b: u8, alpha: f32) -> Paint<'static> {
    let mut paint = Paint { anti_alias: true, ..Default::default() };
    paint.set_color(Color::from_rgba8(r, g, b, (alpha.clamp(0.0, 1.0) * 255.0) as u8));
    paint
}

fn rounded_rect(w: f32, h: f32, r: f32) -> Option<Path> {
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

/// A circular arc from angle `start` through `sweep` radians, clockwise on screen.
fn arc(cx: f32, cy: f32, r: f32, start: f32, sweep: f32) -> Option<Path> {
    if sweep < 0.01 {
        return None;
    }
    let n = (sweep / 0.1).ceil() as usize;
    let mut pb = PathBuilder::new();
    pb.move_to(cx + r * start.cos(), cy + r * start.sin());
    for i in 1..=n {
        let a = start + sweep * i as f32 / n as f32;
        pb.line_to(cx + r * a.cos(), cy + r * a.sin());
    }
    pb.finish()
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

impl SeatHandler for HudState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            if let Ok(pointer) = self.seat_state.get_pointer(qh, &seat) {
                let shape = self.cursor_shapes.as_ref().map(|m| m.get_shape_device(&pointer, qh));
                self.pointer = Some((pointer, shape));
            }
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer {
            if let Some((pointer, shape)) = self.pointer.take() {
                if let Some(shape) = shape {
                    shape.destroy();
                }
                pointer.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for HudState {
    // The pill only takes the pointer while it offers a retry.
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for event in events {
            if self.layer.as_ref().is_none_or(|layer| layer.wl_surface() != &event.surface) {
                continue;
            }
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    self.enter_serial = serial;
                    self.hovered = false; // so point_at sets the cursor
                    self.point_at(event.position);
                }
                PointerEventKind::Motion { .. } => self.point_at(event.position),
                PointerEventKind::Leave { .. } => (self.hovered, self.on_button) = (false, false),
                PointerEventKind::Press { button: BUTTON_LEFT, .. } if self.on_button && self.mode == Mode::Failed && self.phase != Phase::Leaving => {
                    let _ = self.events.send(Event::Retry);
                    self.set_mode(Mode::Busy);
                }
                _ => {}
            }
        }
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
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(HudState);
