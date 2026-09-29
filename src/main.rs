mod desktop;
mod hud;
mod paste;
mod recorder;
mod shortcut;
mod transcribe;

use std::{
    io::Cursor,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::Result;
use image::{AnimationDecoder, RgbaImage, codecs::gif::GifDecoder, imageops::FilterType};
use ksni::TrayMethods;
use tokio::{sync::mpsc, task::JoinHandle};

use recorder::Recording;
use transcribe::Transcriber;

const ICON_SIZE: u32 = 64;
/// How often the loading animation advances; frames are picked by elapsed
/// time, so this only trades smoothness for D-Bus traffic.
const FRAME_INTERVAL: Duration = Duration::from_millis(66);

/// Everything the main loop reacts to: tray clicks toggle, the shortcut
/// starts on press and stops on release.
#[derive(Clone, Copy, PartialEq)]
pub enum Event {
    Toggle,
    Start,
    Stop,
    Quit,
}

#[derive(Clone, Copy)]
enum State {
    Loading,
    Idle,
    Recording,
    Transcribing,
}

struct Icons {
    idle: ksni::Icon,
    active: ksni::Icon,
    loading: Animation,
}

struct Animation {
    frames: Vec<ksni::Icon>,
    /// When each frame ends, measured from the start of the loop.
    ends: Vec<Duration>,
}

impl Animation {
    fn frame_at(&self, elapsed: Duration) -> usize {
        let total = self.ends.last().map_or(1, |d| d.as_millis().max(1));
        let t = Duration::from_millis((elapsed.as_millis() % total) as u64);
        self.ends.iter().position(|&end| t < end).unwrap_or(0)
    }
}

struct FishTray {
    state: State,
    frame: usize,
    icons: Icons,
    events: mpsc::UnboundedSender<Event>,
}

impl ksni::Tray for FishTray {
    fn id(&self) -> String {
        env!("CARGO_PKG_NAME").into()
    }

    fn title(&self) -> String {
        "fishpr".into()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.events.send(Event::Toggle);
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let icon = match self.state {
            State::Idle => &self.icons.idle,
            State::Recording => &self.icons.active,
            State::Loading | State::Transcribing => &self.icons.loading.frames[self.frame],
        };
        vec![icon.clone()]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let description = match self.state {
            State::Loading => "Starting…",
            State::Idle => "Click or hold Ctrl+Space to record",
            State::Recording => "Recording… click or release to transcribe",
            State::Transcribing => "Transcribing…",
        };
        ksni::ToolTip {
            title: "fishpr".into(),
            description: description.into(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.events.send(Event::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Bounding box `(x, y, w, h)` of the visibly opaque pixels, if any.
fn opaque_bounds(img: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, p) in img.enumerate_pixels() {
        if p[3] > 8 {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
    }
    (x0 <= x1).then(|| (x0, y0, x1 - x0 + 1, y1 - y0 + 1))
}

fn union(a: Option<(u32, u32, u32, u32)>, b: Option<(u32, u32, u32, u32)>) -> Option<(u32, u32, u32, u32)> {
    match (a, b) {
        (Some((ax, ay, aw, ah)), Some((bx, by, bw, bh))) => {
            let (x, y) = (ax.min(bx), ay.min(by));
            Some((x, y, (ax + aw).max(bx + bw) - x, (ay + ah).max(by + bh) - y))
        }
        (a, b) => a.or(b),
    }
}

/// Crops to `bounds`, centers on a transparent square (so the tray doesn't
/// stretch it), and scales to a tray icon.
fn to_icon(img: &RgbaImage, bounds: Option<(u32, u32, u32, u32)>) -> ksni::Icon {
    let (x, y, w, h) = bounds.unwrap_or((0, 0, img.width(), img.height()));
    let cropped = image::imageops::crop_imm(img, x, y, w, h).to_image();
    let side = w.max(h);
    let mut canvas = RgbaImage::new(side, side);
    image::imageops::overlay(&mut canvas, &cropped, ((side - w) / 2).into(), ((side - h) / 2).into());
    let mut data = image::imageops::resize(&canvas, ICON_SIZE, ICON_SIZE, FilterType::Lanczos3).into_vec();
    for px in data.chunks_exact_mut(4) {
        px.rotate_right(1); // RGBA -> ARGB
    }
    ksni::Icon { width: ICON_SIZE as i32, height: ICON_SIZE as i32, data }
}

fn load_icon(png: &[u8]) -> ksni::Icon {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).expect("valid icon png").to_rgba8();
    to_icon(&img, opaque_bounds(&img))
}

fn load_animation(gif: &[u8]) -> Animation {
    let frames = GifDecoder::new(Cursor::new(gif)).and_then(|d| d.into_frames().collect_frames()).expect("valid icon gif");
    // One shared crop for every frame, or the fish would jump around as the dot moves.
    let bounds = frames.iter().fold(None, |acc, f| union(acc, opaque_bounds(f.buffer())));
    let mut end = Duration::ZERO;
    let mut ends = Vec::with_capacity(frames.len());
    for frame in &frames {
        let (num, den) = frame.delay().numer_denom_ms();
        let delay = Duration::from_millis((num / den.max(1)).into());
        // Browsers treat near-zero GIF delays as 100 ms; do the same.
        end += if delay < Duration::from_millis(20) { Duration::from_millis(100) } else { delay };
        ends.push(end);
    }
    let frames = frames.iter().map(|f| to_icon(f.buffer(), bounds)).collect();
    Animation { frames, ends }
}

/// Owns the tray icon and HUD, keeping both in step with the app state.
struct Ui {
    tray: ksni::Handle<FishTray>,
    hud: hud::Hud,
    animation: Option<JoinHandle<()>>,
}

impl Ui {
    async fn set(&mut self, state: State) {
        if let Some(task) = self.animation.take() {
            task.abort();
        }
        self.tray.update(|t| (t.state, t.frame) = (state, 0)).await;
        if matches!(state, State::Recording) { self.hud.show() } else { self.hud.hide() }
        if matches!(state, State::Loading | State::Transcribing) {
            let tray = self.tray.clone();
            self.animation = Some(tokio::spawn(async move {
                let start = Instant::now();
                let mut tick = tokio::time::interval(FRAME_INTERVAL);
                loop {
                    tick.tick().await;
                    let elapsed = start.elapsed();
                    tray.update(|t| t.frame = t.icons.loading.frame_at(elapsed)).await;
                }
            }));
        }
    }
}

/// `~/.local/share/fishpr`, where the model and portal token live.
fn data_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or_else(|| anyhow::anyhow!("neither XDG_DATA_HOME nor HOME is set"))?;
    Ok(base.join("fishpr"))
}

fn recording_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join(format!("fishpr-{}.wav", std::process::id()))
}

async fn finish(recording: Recording, transcriber: &Transcriber) -> Result<String> {
    let path = recording.stop().await?;
    let result = transcriber.transcribe(&path).await;
    let _ = tokio::fs::remove_file(&path).await;
    let text = result?;
    if text.is_empty() {
        anyhow::bail!("no speech detected");
    }
    desktop::copy_to_clipboard(&text).await?;
    Ok(text)
}

/// Pastes the transcription into the focused window. The text is already on
/// the clipboard, so a failed paste still leaves it one Ctrl+V away.
async fn deliver(paster: &mut paste::Paster, text: &str) {
    // Give wl-copy's background process a moment to take clipboard ownership.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    if let Err(e) = paster.paste().await {
        eprintln!("fishpr: paste failed: {e:#}");
        desktop::notify("Couldn't paste — text is on your clipboard", &format!("{e:#}\n\n{text}"));
    }
}

/// Drops queued input that arrived while we were busy, so a click or key
/// press made during transcription doesn't start a new recording. Quit is kept.
fn drain(events: &mut mpsc::UnboundedReceiver<Event>) -> bool {
    let mut quit = false;
    while let Ok(event) = events.try_recv() {
        quit |= event == Event::Quit;
    }
    quit
}

#[tokio::main]
async fn main() -> Result<()> {
    let icons = Icons {
        idle: load_icon(include_bytes!("../assets/icon-idle.png")),
        active: load_icon(include_bytes!("../assets/icon-active.png")),
        loading: load_animation(include_bytes!("../assets/icon-loading.gif")),
    };

    let (events_tx, mut events) = mpsc::unbounded_channel();
    let tray = FishTray { state: State::Loading, frame: 0, icons, events: events_tx.clone() };
    let mut ui = Ui { tray: tray.spawn().await?, hud: hud::Hud::spawn(), animation: None };
    ui.set(State::Loading).await;

    // systemctl stop / Ctrl+C should also release the global shortcut.
    for kind in [tokio::signal::unix::SignalKind::terminate(), tokio::signal::unix::SignalKind::interrupt()] {
        let mut signal = tokio::signal::unix::signal(kind)?;
        let tx = events_tx.clone();
        tokio::spawn(async move {
            signal.recv().await;
            let _ = tx.send(Event::Quit);
        });
    }

    let shortcut = match shortcut::Shortcut::register(events_tx.clone()).await {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("fishpr: global shortcut unavailable: {e:#}");
            desktop::notify("fishpr: Ctrl+Space unavailable", &format!("{e:#}\n\nThe tray icon still works."));
            None
        }
    };

    let transcriber = match transcribe::ensure_vad_model().await.and_then(|m| Transcriber::load(&m)) {
        Ok(t) => t,
        Err(e) => {
            desktop::notify("fishpr couldn't start", &format!("{e:#}"));
            if let Some(s) = &shortcut {
                s.unregister().await;
            }
            return Err(e);
        }
    };
    let mut quit = drain(&mut events);
    ui.set(State::Idle).await;
    let mut paster = paste::Paster::new();
    let path = recording_path();
    let mut recording: Option<Recording> = None;

    while !quit {
        let Some(event) = events.recv().await else { break };
        match (event, recording.take()) {
            (Event::Quit, _) => quit = true,
            (Event::Toggle | Event::Start, None) => match Recording::start(&path, {
                let hud = ui.hud.clone();
                move |level| hud.set_level(level)
            }) {
                Ok(r) => {
                    recording = Some(r);
                    ui.set(State::Recording).await;
                }
                Err(e) => desktop::notify("Couldn't start recording", &format!("{e:#}")),
            },
            // Key auto-repeat sends more presses while held; keep recording.
            (Event::Start, Some(r)) => recording = Some(r),
            (Event::Stop, None) => {}
            (Event::Toggle | Event::Stop, Some(r)) => {
                ui.set(State::Transcribing).await;
                match finish(r, &transcriber).await {
                    Ok(text) => deliver(&mut paster, &text).await,
                    Err(e) => {
                        eprintln!("fishpr: {e:#}");
                        desktop::notify("Transcription failed", &format!("{e:#}"));
                    }
                }
                quit = drain(&mut events);
                ui.set(State::Idle).await;
            }
        }
    }

    ui.hud.hide();
    if let Some(s) = &shortcut {
        s.unregister().await;
    }
    Ok(())
}
