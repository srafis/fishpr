mod control;
mod desktop;
mod hud;
mod paste;
mod recorder;
mod shortcut;
mod transcribe;

use std::path::PathBuf;

use anyhow::Result;
use image::{RgbaImage, imageops::FilterType};
use ksni::TrayMethods;
use tokio::sync::mpsc;

use recorder::Recording;
use transcribe::{Session, Transcriber};

/// Matches the installed `<APP_ID>.desktop`. Portals only accept an
/// unsandboxed app whose reverse-DNS ID has a .desktop file.
pub const APP_ID: &str = "io.github.srafis.fishpr";

const USAGE: &str = "usage: fishpr [--toggle]

With no options, starts fishpr. --toggle starts or stops recording in the
running fishpr; bind it to a key where fishpr can't register its shortcut.";

const ICON_SIZE: u32 = 64;

/// Everything the main loop reacts to: tray clicks and `fishpr --toggle`
/// toggle, the shortcut starts on press and stops on release, and the HUD's
/// retry button transcribes the last failed recording again.
#[derive(Clone, Copy, PartialEq)]
pub enum Event {
    Toggle,
    Start,
    Stop,
    Retry,
    Quit,
}

#[derive(Clone, Copy)]
enum State {
    Loading,
    Idle,
    Recording,
    Transcribing,
    /// Idle, with the HUD offering to retry for a few seconds.
    Failed { no_speech: bool },
}

/// The tray icon is the same in every state; the HUD shows what's going on.
struct FishTray {
    state: State,
    icon: ksni::Icon,
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
        vec![self.icon.clone()]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let description = match self.state {
            State::Loading => "Starting…",
            State::Idle | State::Failed { .. } => "Click or hold Ctrl+Space to record",
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

/// Crops to the opaque pixels, centers on a transparent square (so the tray
/// doesn't stretch it), and scales to a tray icon.
fn load_icon(png: &[u8]) -> ksni::Icon {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).expect("valid icon png").to_rgba8();
    let (x, y, w, h) = opaque_bounds(&img).unwrap_or((0, 0, img.width(), img.height()));
    let cropped = image::imageops::crop_imm(&img, x, y, w, h).to_image();
    let side = w.max(h);
    let mut canvas = RgbaImage::new(side, side);
    image::imageops::overlay(&mut canvas, &cropped, ((side - w) / 2).into(), ((side - h) / 2).into());
    let mut data = image::imageops::resize(&canvas, ICON_SIZE, ICON_SIZE, FilterType::Lanczos3).into_vec();
    for px in data.chunks_exact_mut(4) {
        px.rotate_right(1); // RGBA -> ARGB
    }
    ksni::Icon { width: ICON_SIZE as i32, height: ICON_SIZE as i32, data }
}

/// Owns the tray icon and HUD, keeping both in step with the app state.
struct Ui {
    tray: ksni::Handle<FishTray>,
    hud: hud::Hud,
}

impl Ui {
    async fn set(&mut self, state: State) {
        self.tray.update(|t| t.state = state).await;
        match state {
            State::Recording => self.hud.show(),
            State::Transcribing => self.hud.busy(),
            State::Failed { no_speech } => self.hud.fail(no_speech),
            State::Loading | State::Idle => self.hud.hide(),
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

/// Waits for the transcript of `samples` and puts it on the clipboard. None
/// means nobody spoke. A retry skips the voice check, in case it was wrong.
async fn transcribe(transcriber: &Transcriber, session: Session, samples: &[i16], check_speech: bool) -> Result<Option<String>> {
    if check_speech && !transcriber.has_speech(samples).await? {
        return Ok(None);
    }
    let text = session.finish().await?;
    if text.is_empty() {
        return Ok(None);
    }
    desktop::copy_to_clipboard(&text).await?;
    Ok(Some(text))
}

/// Pastes a finished transcription, or offers a retry in the HUD when it
/// failed or heard no speech. Returns the recording to keep for that retry.
async fn settle(ui: &mut Ui, paster: &mut paste::Paster, result: Result<Option<String>>, samples: Vec<i16>) -> Option<Vec<i16>> {
    let (no_speech, problem) = match result {
        Ok(Some(text)) => {
            ui.set(State::Idle).await;
            deliver(paster, &text).await;
            return None;
        }
        Ok(None) => (true, "no speech detected".to_string()),
        Err(e) => (false, format!("{e:#}")),
    };
    eprintln!("fishpr: {problem}");
    if !ui.hud.is_available() {
        desktop::notify("Transcription failed", &problem);
    }
    ui.set(State::Failed { no_speech }).await;
    Some(samples)
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
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("--toggle") => return control::toggle().await,
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return Ok(());
        }
        Some(arg) => {
            eprintln!("fishpr: unknown option {arg}\n\n{USAGE}");
            std::process::exit(2);
        }
    }

    // Must come before any other portal call. Portals older than 1.19 don't
    // have it and guess the ID from the systemd unit name instead.
    if let Err(e) = ashpd::register_host_app(APP_ID.parse().expect("valid app id")).await {
        eprintln!("fishpr: couldn't register {APP_ID} with the desktop portal: {e}");
    }

    let (events_tx, mut events) = mpsc::unbounded_channel();
    let _control = control::serve(events_tx.clone()).await?;

    let icon = load_icon(include_bytes!("../assets/icon.png"));

    let tray = FishTray { state: State::Loading, icon, events: events_tx.clone() };
    // Without a tray host (no System Tray widget in the panel), keep running
    // without the icon; it appears if one shows up later.
    let tray = tray.assume_sni_available(true).spawn().await?;
    let mut ui = Ui { tray, hud: hud::Hud::spawn(events_tx.clone()) };
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

    // Said once rather than at every login: where there's no KGlobalAccel
    // (desktops other than Plasma), the user binds `fishpr --toggle` once and is done.
    let told_marker = data_dir().map(|d| d.join("shortcut-unavailable"));
    let shortcut = match shortcut::Shortcut::register(events_tx.clone()).await {
        Ok(s) => {
            if let Ok(marker) = &told_marker {
                let _ = std::fs::remove_file(marker);
            }
            Some(s)
        }
        Err(e) => {
            eprintln!("fishpr: global shortcut unavailable: {e:#}");
            if told_marker.as_ref().is_ok_and(|m| !m.exists()) {
                desktop::notify(
                    "fishpr: Ctrl+Space unavailable",
                    &format!("{e:#}\n\nBind the command \"fishpr --toggle\" to a key in your keyboard settings, or use the tray icon."),
                );
                if let Ok(marker) = &told_marker {
                    let _ = std::fs::create_dir_all(marker.parent().unwrap()).and_then(|()| std::fs::write(marker, ""));
                }
            }
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
    let mut recording: Option<(Recording, Session)> = None;
    // The last recording, while it failed to transcribe and can be retried.
    let mut failed: Option<Vec<i16>> = None;

    while !quit {
        let Some(event) = events.recv().await else { break };
        match (event, recording.take()) {
            (Event::Quit, _) => quit = true,
            (Event::Toggle | Event::Start, None) => {
                failed = None;
                // Transcription starts with the recording, so the text is ready soon after it ends.
                let (session, audio) = transcriber.begin();
                match Recording::start(audio, {
                    let hud = ui.hud.clone();
                    move |level| hud.set_level(level)
                }) {
                    Ok(r) => {
                        recording = Some((r, session));
                        ui.set(State::Recording).await;
                    }
                    Err(e) => desktop::notify("Couldn't start recording", &format!("{e:#}")),
                }
            }
            // Key auto-repeat sends more presses while held; keep recording.
            (Event::Start | Event::Retry, Some(r)) => recording = Some(r),
            (Event::Stop, None) => {}
            (Event::Retry, None) => {
                let Some(samples) = failed.take() else { continue };
                ui.set(State::Transcribing).await;
                let result = transcribe(&transcriber, transcriber.replay(&samples), &samples, false).await;
                failed = settle(&mut ui, &mut paster, result, samples).await;
                quit = drain(&mut events);
            }
            (Event::Toggle | Event::Stop, Some((r, session))) => {
                ui.set(State::Transcribing).await;
                match r.stop().await {
                    Ok(samples) => {
                        let result = transcribe(&transcriber, session, &samples, true).await;
                        failed = settle(&mut ui, &mut paster, result, samples).await;
                    }
                    // Nothing was recorded, so there's nothing to retry.
                    Err(e) => {
                        eprintln!("fishpr: {e:#}");
                        desktop::notify("Recording failed", &format!("{e:#}"));
                        ui.set(State::Idle).await;
                    }
                }
                quit = drain(&mut events);
            }
        }
    }

    ui.set(State::Idle).await;
    if let Some(s) = &shortcut {
        s.unregister().await;
    }
    Ok(())
}
