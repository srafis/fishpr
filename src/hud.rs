//! A black pill at the bottom of the screen, drawn as a wlr-layer-shell
//! overlay (KWin supports it). While recording, its bars follow the mic; while
//! transcribing, a spinner joins them; if that fails, a retry button takes the
//! spinner's place for a few seconds. When a transcription has nowhere to
//! paste, the pill grows into a card that shows it with a copy button, for a
//! few seconds. It never takes focus, and takes clicks only while it offers a
//! retry or the card. Runs on its own thread with its own Wayland connection.

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
use tiny_skia::{Color, FillRule, FilterQuality, IntSize, LineCap, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke, Transform};
use tokio::sync::mpsc::UnboundedSender;

use crate::Event;

const FONT: &[u8] = include_bytes!("../assets/NotoSans-Medium.ttf");
const NO_SPEECH: &str = "No speech detected";
const HINT: &str = "Select a text field first, then dictate";

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

// The card that offers a transcription with nowhere to paste.
const CARD_W: f32 = 400.0;
const CARD_PAD: f32 = 18.0;
const CARD_RADIUS: f32 = 24.0;
const CARD_GAP: f32 = 12.0;
/// The top row: logo, hint, and close button.
const HEADER_H: f32 = 30.0;
const LOGO: f32 = 24.0;
const CLOSE: f32 = 28.0;
const HINT_SIZE: f32 = 15.5;
const BODY_SIZE: f32 = 17.0;
const LINE_H: f32 = 25.0;
/// Longer text is cut short with an ellipsis; the copy button copies all of it.
const MAX_LINES: usize = 4;
const COPY_H: f32 = 34.0;
/// Room on either side of the copy button's label.
const COPY_PAD: f32 = 16.0;
/// The copy button's icon, and the space between it and the label.
const COPY_ICON: f32 = 14.0;
const COPY_ICON_GAP: f32 = 7.0;
/// How long the card stays, not counting while the pointer is on it.
const OFFER_SECS: f32 = 10.0;
/// How long the copy button says "Copied" before the card goes.
const COPIED_SECS: f32 = 0.7;

enum Msg {
    Show,
    Busy,
    Fail { no_speech: bool },
    Offer(String),
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

    /// Grows into a card that shows `text`, with a copy button, for a few
    /// seconds. The button sends `Event::CopyLast`.
    pub fn offer(&self, text: &str) {
        let _ = self.tx.send(Msg::Offer(text.to_owned()));
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
                    Msg::Offer(text) => {
                        state.card = Some(Card::new(&state.font, &text));
                        state.set_mode(Mode::Offer);
                    }
                    Msg::Hide => state.hide(),
                    Msg::Level(level) => state.target_level = level,
                }
            }
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let font = FontRef::try_from_slice(FONT)?;
    let labels = Labels {
        no_speech: Label::new(&font, NO_SPEECH, FONT_SIZE),
        hint: Label::new(&font, HINT, HINT_SIZE),
        copy: Label::new(&font, "Copy", FONT_SIZE),
        copied: Label::new(&font, "Copied", FONT_SIZE),
    };
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
        size: (0, 0),
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
        grow: 0.0,
        no_speech: false,
        countdown: 0.0,
        copied: None,
        hovered: false,
        button: None,
        enter_serial: 0,
        font,
        labels,
        card: None,
        logo: None,
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
    Offer,
}

/// What the pointer can click.
#[derive(Clone, Copy, PartialEq)]
enum Button {
    Retry,
    Close,
    Copy,
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
    /// The surface's size, as the compositor last configured it.
    size: (u32, u32),
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
    /// How far the pill has grown into the card, 0.0–1.0.
    grow: f32,
    /// Whether the last failure was hearing no speech, which the label says.
    no_speech: bool,
    /// Seconds left before the retry button, or the card, goes away.
    countdown: f32,
    /// When the copy button was clicked.
    copied: Option<Instant>,
    /// Whether the pointer is on the pill, which pauses the countdown, and
    /// which button it's on.
    hovered: bool,
    button: Option<Button>,
    /// The pointer's latest entry onto the pill, which changing the cursor needs.
    enter_serial: u32,
    font: FontRef<'static>,
    labels: Labels,
    /// The transcription the card offers.
    card: Option<Card>,
    /// The app icon for the card, at the surface's scale.
    logo: Option<Pixmap>,
}

impl HudState {
    fn set_mode(&mut self, mode: Mode) {
        let was = self.mode;
        self.mode = mode;
        self.copied = None;
        match mode {
            Mode::Failed => self.countdown = RETRY_SECS,
            Mode::Offer => self.countdown = OFFER_SECS,
            Mode::Recording | Mode::Busy => {}
        }
        // From the card, a new pill pops up in its own surface.
        if was == Mode::Offer && mode != Mode::Offer {
            self.layer = None;
        }
        match (&self.layer, self.phase) {
            (None, _) => self.create_layer(),
            (Some(_), Phase::Leaving) => self.set_phase(Phase::Entering),
            (Some(_), _) => {}
        }
        // The pill grows into the card once the surface has room for it.
        if mode == Mode::Offer
            && let Some(layer) = &self.layer
        {
            let (w, h) = self.surface_size();
            layer.set_size(w, h);
            layer.commit();
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
        let (w, h) = self.surface_size();
        layer.set_size(w, h);
        self.size = (0, 0);
        self.set_phase(Phase::Entering);
        self.started = self.since;
        self.last_frame = self.since;
        self.frame_pending = false;
        (self.hovered, self.button) = (false, None);
        (self.level, self.target_level) = (0.0, 0.0);
        // Appear already in shape, rather than morphing while popping in.
        self.open = if self.mode == Mode::Recording { 0.0 } else { 1.0 };
        self.busy = if self.mode == Mode::Busy { 1.0 } else { 0.0 };
        self.failed = if self.mode == Mode::Failed { 1.0 } else { 0.0 };
        self.labelled = if self.showing_label() { 1.0 } else { 0.0 };
        self.grow = if self.mode == Mode::Offer { 1.0 } else { 0.0 };
        // First commit has no buffer; we draw once the compositor configures us.
        layer.commit();
        self.layer = Some(layer);
        let _ = self.conn.flush();
    }

    fn showing_label(&self) -> bool {
        self.mode == Mode::Failed && self.no_speech
    }

    /// For the pill, wide enough for its widest, at the peak of its entrance
    /// bounce. The card's surface is taller, and the card's bottom edge sits
    /// where the pill's does.
    fn surface_size(&self) -> (u32, u32) {
        match &self.card {
            Some(card) if self.mode == Mode::Offer => ((CARD_W + 2.0 * MARGIN).ceil() as u32, (card.height + 2.0 * MARGIN).ceil() as u32),
            _ => ((pill_width(BARS_W.max(self.labels.no_speech.width), 1.0) * 1.1 + 4.0).ceil() as u32, SURFACE_H),
        }
    }

    /// The card's top-left corner on the surface, once grown.
    fn card_origin(&self, card: &Card) -> (f32, f32) {
        ((self.size.0 as f32 - CARD_W) / 2.0, self.size.1 as f32 - MARGIN - card.height)
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
        let content = if self.no_speech { self.labels.no_speech.width } else { BARS_W };
        let pw = pill_width(content, 1.0);
        let left = (self.size.0 as f32 - pw) / 2.0;
        (left, pw, (left + pw - SLOT_INSET - SLOT / 2.0, self.size.1 as f32 - MARGIN - HEIGHT / 2.0))
    }

    /// Clicks go through the HUD, except while it offers a retry or the card:
    /// then the pill or card takes the pointer, so resting on it pauses the
    /// countdown.
    fn update_input_region(&mut self) {
        let Some(layer) = &self.layer else { return };
        let Ok(region) = Region::new(&self.compositor) else { return };
        match (self.mode, &self.card) {
            _ if self.phase == Phase::Leaving => (self.hovered, self.button) = (false, None),
            (Mode::Failed, _) => {
                let (left, pw, _) = self.failed_layout();
                let top = self.size.1 as f32 - MARGIN - HEIGHT;
                region.add(left as i32, top as i32, pw.ceil() as i32, HEIGHT as i32);
            }
            (Mode::Offer, Some(card)) => {
                let (left, top) = self.card_origin(card);
                region.add(left as i32, top as i32, CARD_W.ceil() as i32, card.height.ceil() as i32);
            }
            _ => (self.hovered, self.button) = (false, None),
        }
        layer.wl_surface().set_input_region(Some(region.wl_region()));
    }

    /// Tracks which button the pointer, at `(x, y)` on the surface, is on,
    /// and shows a hand there.
    fn point_at(&mut self, (x, y): (f64, f64)) {
        let (x, y) = (x as f32, y as f32);
        let button = match (self.mode, &self.card) {
            (Mode::Failed, _) => {
                let (_, _, (bx, by)) = self.failed_layout();
                ((x - bx).hypot(y - by) <= SLOT / 2.0 + 1.0).then_some(Button::Retry)
            }
            (Mode::Offer, Some(card)) => {
                let (left, top) = self.card_origin(card);
                card.button_at(x - left, y - top, &self.labels)
            }
            _ => None,
        };
        if button.is_some() != self.button.is_some() || !self.hovered {
            if let Some((_, Some(shape))) = &self.pointer {
                shape.set_shape(self.enter_serial, if button.is_some() { Shape::Pointer } else { Shape::Default });
            }
        }
        (self.hovered, self.button) = (true, button);
    }

    fn click(&mut self) {
        match (self.mode, self.button) {
            (Mode::Failed, Some(Button::Retry)) => {
                let _ = self.events.send(Event::Retry);
                self.set_mode(Mode::Busy);
            }
            (Mode::Offer, Some(Button::Close)) => self.hide(),
            (Mode::Offer, Some(Button::Copy)) if self.copied.is_none() => {
                let _ = self.events.send(Event::CopyLast);
                self.copied = Some(Instant::now());
            }
            _ => {}
        }
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

        if matches!(self.mode, Mode::Failed | Mode::Offer) && self.phase != Phase::Leaving && !self.hovered {
            self.countdown -= dt;
            if self.countdown <= 0.0 {
                self.hide();
            }
        }
        if self.mode == Mode::Offer && self.phase != Phase::Leaving && self.copied.is_some_and(|at| at.elapsed().as_secs_f32() >= COPIED_SECS) {
            self.hide();
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
        // Taller than the pill's surface once the compositor has resized it.
        let grown = self.mode == Mode::Offer && self.size.1 > SURFACE_H;
        self.grow += (f32::from(grown) - self.grow) * morph;

        let (w, h) = self.size;
        let s = self.scale.max(1) as u32;
        let Some(mut pixmap) = Pixmap::new(w * s, h * s) else { return };
        let logo_px = (LOGO * s as f32).round() as u32;
        if self.card.is_some() && self.logo.as_ref().is_none_or(|l| l.width() != logo_px) {
            self.logo = logo(logo_px);
        }
        let look = Look {
            time,
            zoom,
            drop,
            level: self.level,
            open: self.open,
            busy: self.busy,
            failed: self.failed,
            labelled: self.labelled,
            grow: self.grow,
            left: (self.countdown / if self.mode == Mode::Offer { OFFER_SECS } else { RETRY_SECS }).clamp(0.0, 1.0),
            hovered: self.button,
            copied: self.copied.is_some(),
        };
        if let Some(card) = self.card.as_ref().filter(|_| self.grow > 0.001) {
            paint_card(&mut pixmap, s as f32, &look, &self.labels, card, self.logo.as_ref());
        }
        paint_pill(&mut pixmap, s as f32, &look, &self.labels.no_speech);

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
    fn new(font: &FontRef, text: &str, size: f32) -> Self {
        let scaled = font.as_scaled(PxScale::from(size));
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

/// How wide `text` is, set in `font` at `size`.
fn text_width(font: &FontRef, size: f32, text: &str) -> f32 {
    let scaled = font.as_scaled(PxScale::from(size));
    let mut prev = None;
    let mut width = 0.0;
    for c in text.chars() {
        let id = font.glyph_id(c);
        if let Some(prev) = prev {
            width += scaled.kern(prev, id);
        }
        prev = Some(id);
        width += scaled.h_advance(id);
    }
    width
}

/// Breaks `text` into lines no wider than `width`, between words where it
/// can. Past `max_lines`, the last line ends in an ellipsis.
fn wrap(font: &FontRef, size: f32, text: &str, width: f32, max_lines: usize) -> Vec<String> {
    let fits = |line: &str| text_width(font, size, line) <= width;
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if lines.len() > max_lines {
            break;
        }
        let joined = if line.is_empty() { word.to_owned() } else { format!("{line} {word}") };
        if fits(&joined) {
            line = joined;
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        // A word too long for a line of its own breaks where it overflows.
        for c in word.chars() {
            line.push(c);
            if !fits(&line) && line.chars().count() > 1 {
                line.pop();
                lines.push(std::mem::replace(&mut line, c.to_string()));
            }
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        let last = lines.last_mut().expect("max_lines is positive");
        while !last.is_empty() && !fits(&format!("{}…", last.trim_end())) {
            last.pop();
        }
        *last = format!("{}…", last.trim_end());
    }
    lines
}

/// The pill's fixed texts.
struct Labels {
    no_speech: Label,
    hint: Label,
    copy: Label,
    copied: Label,
}

impl Labels {
    fn copy_width(&self) -> f32 {
        COPY_ICON + COPY_ICON_GAP + self.copy.width.max(self.copied.width) + 2.0 * COPY_PAD
    }
}

/// A transcription on offer, laid out on the card. Card coordinates have the
/// card's top-left corner at (0, 0).
struct Card {
    lines: Vec<Label>,
    height: f32,
}

impl Card {
    fn new(font: &FontRef, text: &str) -> Self {
        let lines: Vec<Label> =
            wrap(font, BODY_SIZE, text, CARD_W - 2.0 * CARD_PAD, MAX_LINES).iter().map(|line| Label::new(font, line, BODY_SIZE)).collect();
        let height = CARD_PAD + HEADER_H + CARD_GAP + LINE_H * lines.len().max(1) as f32 + CARD_GAP + COPY_H + CARD_PAD;
        Self { lines, height }
    }

    fn close_center() -> (f32, f32) {
        (CARD_W - CARD_PAD - CLOSE / 2.0, CARD_PAD + HEADER_H / 2.0)
    }

    /// The copy button's left edge, top edge, and width.
    fn copy_rect(&self, labels: &Labels) -> (f32, f32, f32) {
        let w = labels.copy_width();
        (CARD_W - CARD_PAD - w, self.height - CARD_PAD - COPY_H, w)
    }

    fn button_at(&self, x: f32, y: f32, labels: &Labels) -> Option<Button> {
        let (cx, cy) = Self::close_center();
        let (bx, by, bw) = self.copy_rect(labels);
        if (x - cx).hypot(y - cy) <= CLOSE / 2.0 + 1.0 {
            Some(Button::Close)
        } else if (bx..=bx + bw).contains(&x) && (by..=by + COPY_H).contains(&y) {
            Some(Button::Copy)
        } else {
            None
        }
    }
}

/// The app icon, `px` device pixels across, premultiplied for tiny-skia.
fn logo(px: u32) -> Option<Pixmap> {
    let mut data = crate::icon(px).into_raw();
    for p in data.chunks_exact_mut(4) {
        let a = u16::from(p[3]);
        for c in &mut p[..3] {
            *c = (u16::from(*c) * a / 255) as u8;
        }
    }
    Pixmap::from_vec(data, IntSize::from_wh(px, px)?)
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
    /// How far the pill has grown into the card.
    grow: f32,
    /// How much of the retry window, or the card's time, is left.
    left: f32,
    /// The button the pointer is on.
    hovered: Option<Button>,
    /// Whether the card's text has been copied.
    copied: bool,
}

/// Paints the pill, centered at the bottom of `pixmap`, at `scale` device
/// pixels per logical pixel. As it grows into the card, the card draws its
/// shape and the pill's contents fade.
fn paint_pill(pixmap: &mut Pixmap, scale: f32, look: &Look, label: &Label) {
    let Look { time, zoom, drop, level, open, busy, failed, labelled, grow, left, hovered, .. } = *look;
    let fade = (1.0 - 4.0 * grow).clamp(0.0, 1.0);
    if fade < 0.01 {
        return;
    }
    let pw = pill_width(BARS_W + (label.width - BARS_W) * labelled, open);
    let cy = HEIGHT / 2.0;
    // Pill coordinates: the pill's top-left corner is (0, 0).
    let t = Transform::from_scale(scale, scale)
        .pre_translate(pixmap.width() as f32 / scale / 2.0, pixmap.height() as f32 / scale - MARGIN - cy + drop)
        .pre_scale(zoom, zoom)
        .pre_translate(-pw / 2.0, -cy);

    if grow < 0.001 {
        paint_shape(pixmap, pw, HEIGHT, HEIGHT / 2.0, 0.94, t);
    }

    // Level bars, tallest in the middle. Their height follows the mic, and
    // each wobbles a little so speech looks alive; in silence they rest as
    // dots. While transcribing, a ripple runs along them; on failure they
    // turn red.
    let bars = (1.0 - labelled) * fade;
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
        pixmap.fill_path(text, &paint(255, 255, 255, 0.92 * labelled * fade), FillRule::Winding, t.pre_translate(PAD, cy + label.middle), None);
    }

    let (sx, sy) = (pw - SLOT_INSET - SLOT / 2.0, cy);
    let round = |width| Stroke { width, line_cap: LineCap::Round, ..Default::default() };

    // Spinner: a short arc running around a faint track.
    let a = busy * open * fade;
    if a > 0.01 {
        if let Some(track) = PathBuilder::from_circle(sx, sy, 6.0) {
            pixmap.stroke_path(&track, &paint(255, 255, 255, 0.18 * a), &round(1.9), t, None);
        }
        if let Some(spin) = arc(sx, sy, 6.0, time * TAU * 1.1, 1.9) {
            pixmap.stroke_path(&spin, &paint(255, 255, 255, 0.95 * a), &round(1.9), t, None);
        }
    }

    // Retry button, ringed by the time left to press it.
    let a = failed * open * fade;
    if a > 0.01 {
        if let Some(button) = PathBuilder::from_circle(sx, sy, SLOT / 2.0) {
            let fill = if hovered == Some(Button::Retry) { 0.24 } else { 0.12 };
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

/// The pill's or card's black body, `w` by `h` with corners of radius `r`.
/// `opacity` lets a little of what's behind show through.
fn paint_shape(pixmap: &mut Pixmap, w: f32, h: f32, r: f32, opacity: f32, t: Transform) {
    if let Some(body) = rounded_rect(w, h, r) {
        pixmap.fill_path(&body, &paint(10, 10, 10, opacity), FillRule::Winding, t, None);
    }
    // A thin dark rim keeps the edge visible over black windows.
    if let Some(rim) = rounded_rect(w - 1.0, h - 1.0, r - 0.5) {
        pixmap.stroke_path(&rim, &paint(48, 48, 47, 1.0), &Stroke { width: 1.0, ..Default::default() }, t.pre_translate(0.5, 0.5), None);
    }
}

/// Paints the card, its bottom edge where the pill's is, grown `look.grow`
/// of the way from the pill. Its contents fade in once it's nearly there.
fn paint_card(pixmap: &mut Pixmap, scale: f32, look: &Look, labels: &Labels, card: &Card, logo: Option<&Pixmap>) {
    let Look { zoom, drop, open, labelled, grow, left, hovered, copied, .. } = *look;
    let g = grow * grow * (3.0 - 2.0 * grow);
    let lerp = |a: f32, b: f32| a + (b - a) * g;
    let pw = pill_width(BARS_W + (labels.no_speech.width - BARS_W) * labelled, open);
    let (w, h) = (lerp(pw, CARD_W), lerp(HEIGHT, card.height));
    let t = Transform::from_scale(scale, scale)
        .pre_translate(pixmap.width() as f32 / scale / 2.0, pixmap.height() as f32 / scale - MARGIN - h / 2.0 + drop)
        .pre_scale(zoom, zoom)
        .pre_translate(-w / 2.0, -h / 2.0);
    // Opaque once grown, so what's behind doesn't show through the text.
    paint_shape(pixmap, w, h, lerp(HEIGHT / 2.0, CARD_RADIUS), lerp(0.94, 1.0), t);

    let a = ((grow - 0.6) / 0.4).clamp(0.0, 1.0);
    if a < 0.01 {
        return;
    }
    let round = |width| Stroke { width, line_cap: LineCap::Round, ..Default::default() };
    if let Some(logo) = logo {
        let k = LOGO / logo.width() as f32 * scale;
        let paint = PixmapPaint { opacity: a, quality: FilterQuality::Bicubic, ..Default::default() };
        pixmap.draw_pixmap(0, 0, logo.as_ref(), &paint, t.pre_translate(CARD_PAD, CARD_PAD + (HEADER_H - LOGO) / 2.0).pre_scale(k / scale, k / scale), None);
    }
    let (cx, cy) = Card::close_center();
    if let Some(hint) = &labels.hint.path {
        let x = cx - CLOSE / 2.0 - 10.0 - labels.hint.width;
        pixmap.fill_path(hint, &paint(255, 255, 255, 0.5 * a), FillRule::Winding, t.pre_translate(x, cy + labels.hint.middle), None);
    }

    // Close button, ringed by the time left before the card goes.
    if let Some(button) = PathBuilder::from_circle(cx, cy, CLOSE / 2.0) {
        let fill = if hovered == Some(Button::Close) { 0.16 } else { 0.06 };
        pixmap.fill_path(&button, &paint(255, 255, 255, fill * a), FillRule::Winding, t, None);
    }
    if let Some(track) = PathBuilder::from_circle(cx, cy, CLOSE / 2.0 - 0.9) {
        pixmap.stroke_path(&track, &paint(255, 255, 255, 0.2 * a), &round(1.6), t, None);
    }
    if let Some(ring) = arc(cx, cy, CLOSE / 2.0 - 0.9, -FRAC_PI_2, TAU * left) {
        pixmap.stroke_path(&ring, &paint(255, 255, 255, 0.85 * a), &round(1.6), t, None);
    }
    let mut cross = PathBuilder::new();
    let arm = 4.25;
    cross.move_to(cx - arm, cy - arm);
    cross.line_to(cx + arm, cy + arm);
    cross.move_to(cx + arm, cy - arm);
    cross.line_to(cx - arm, cy + arm);
    if let Some(cross) = cross.finish() {
        pixmap.stroke_path(&cross, &paint(255, 255, 255, a), &round(1.7), t, None);
    }

    for (i, line) in card.lines.iter().enumerate() {
        if let Some(path) = &line.path {
            let y = CARD_PAD + HEADER_H + CARD_GAP + LINE_H * (i as f32 + 0.5) + line.middle;
            pixmap.fill_path(path, &paint(255, 255, 255, 0.88 * a), FillRule::Winding, t.pre_translate(CARD_PAD, y), None);
        }
    }

    let (bx, by, bw) = card.copy_rect(labels);
    if let Some(button) = rounded_rect(bw, COPY_H, 10.0) {
        let fill = if hovered == Some(Button::Copy) && !copied { 0.5 } else { 0.4 };
        pixmap.fill_path(&button, &paint(255, 255, 255, fill * a), FillRule::Winding, t.pre_translate(bx, by), None);
    }
    // The icon and label, centered together: two overlapping pages, or a
    // check once copied.
    let label = if copied { &labels.copied } else { &labels.copy };
    let left = bx + (bw - COPY_ICON - COPY_ICON_GAP - label.width) / 2.0;
    if let Some(icon) = if copied { check_icon() } else { copy_icon() } {
        let at = t.pre_translate(left, by + (COPY_H - COPY_ICON) / 2.0);
        pixmap.stroke_path(&icon, &paint(255, 255, 255, a), &round(1.5), at, None);
    }
    if let Some(path) = &label.path {
        let at = t.pre_translate(left + COPY_ICON + COPY_ICON_GAP, by + COPY_H / 2.0 + label.middle);
        pixmap.fill_path(path, &paint(255, 255, 255, a), FillRule::Winding, at, None);
    }
}

/// Two overlapping pages, COPY_ICON square: the front one whole, the back
/// one only where it shows above and left of it.
fn copy_icon() -> Option<Path> {
    let (s, r) = (COPY_ICON * 0.7, 2.0);
    let o = COPY_ICON - s;
    let mut pb = PathBuilder::new();
    pb.move_to(o, s);
    pb.line_to(r, s);
    pb.quad_to(0.0, s, 0.0, s - r);
    pb.line_to(0.0, r);
    pb.quad_to(0.0, 0.0, r, 0.0);
    pb.line_to(s - r, 0.0);
    pb.quad_to(s, 0.0, s, r);
    pb.line_to(s, o);
    pb.push_path(&rounded_rect(s, s, r)?.transform(Transform::from_translate(o, o))?);
    pb.finish()
}

/// A check mark, COPY_ICON square.
fn check_icon() -> Option<Path> {
    let k = COPY_ICON / 14.0;
    let mut pb = PathBuilder::new();
    pb.move_to(1.5 * k, 7.5 * k);
    pb.line_to(5.5 * k, 11.5 * k);
    pb.line_to(12.5 * k, 3.0 * k);
    pb.finish()
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
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface, configure: LayerSurfaceConfigure, _: u32) {
        let (w, h) = configure.new_size;
        self.size = if w > 0 && h > 0 { (w, h) } else { self.surface_size() };
        self.update_input_region();
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
    // The pill only takes the pointer while it offers a retry or the card.
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
                PointerEventKind::Leave { .. } => (self.hovered, self.button) = (false, None),
                PointerEventKind::Press { button: BUTTON_LEFT, .. } if self.phase != Phase::Leaving => self.click(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> FontRef<'static> {
        FontRef::try_from_slice(FONT).unwrap()
    }

    #[test]
    fn wraps_between_words() {
        let lines = wrap(&font(), BODY_SIZE, "one two three four five six", text_width(&font(), BODY_SIZE, "one two three"), 4);
        assert_eq!(lines, ["one two three", "four five six"]);
    }

    #[test]
    fn cuts_long_text_short_with_an_ellipsis() {
        let font = font();
        let lines = wrap(&font, BODY_SIZE, &"word ".repeat(200), 200.0, MAX_LINES);
        assert_eq!(lines.len(), MAX_LINES);
        assert!(lines[MAX_LINES - 1].ends_with('…'));
        assert!(lines.iter().all(|l| text_width(&font, BODY_SIZE, l) <= 200.0));
    }

    #[test]
    fn breaks_a_word_too_long_for_a_line() {
        let font = font();
        let lines = wrap(&font, BODY_SIZE, &"x".repeat(100), 100.0, MAX_LINES);
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|l| text_width(&font, BODY_SIZE, l) <= 100.0));
    }
}

