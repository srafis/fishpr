mod clipboard;
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
/// retry button transcribes the last failed recording again. The paste-last
/// shortcut pastes the last transcription again, and the HUD's copy button
/// copies it. Esc, while the HUD shows, cancels or dismisses what it shows.
#[derive(Clone, Copy, PartialEq)]
pub enum Event {
    Toggle,
    Start,
    Stop,
    Retry,
    PasteLast,
    CopyLast,
    Dismiss,
    Quit,
}

#[derive(Clone, Copy)]
enum State {
    Loading,
    Idle,
    Recording,
    Transcribing,
    /// Idle, with the HUD offering to retry for a few seconds.
    Failed { why: hud::Failure },
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

/// The app icon, cropped to its opaque pixels and centered on a transparent
/// square (so it isn't stretched), `size` pixels across.
pub fn icon(size: u32) -> RgbaImage {
    let img = image::load_from_memory_with_format(include_bytes!("../assets/icon.png"), image::ImageFormat::Png)
        .expect("valid icon png")
        .to_rgba8();
    let (x, y, w, h) = opaque_bounds(&img).unwrap_or((0, 0, img.width(), img.height()));
    let cropped = image::imageops::crop_imm(&img, x, y, w, h).to_image();
    let side = w.max(h);
    let mut canvas = RgbaImage::new(side, side);
    image::imageops::overlay(&mut canvas, &cropped, ((side - w) / 2).into(), ((side - h) / 2).into());
    image::imageops::resize(&canvas, size, size, FilterType::Lanczos3)
}

fn tray_icon() -> ksni::Icon {
    let mut data = icon(ICON_SIZE).into_vec();
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
            State::Failed { why } => self.hud.fail(why),
            State::Loading | State::Idle => self.hud.hide(),
        }
    }

    /// Shows a transcription that nothing took, with a button to copy it.
    /// Without the HUD, copies it right away.
    async fn offer(&mut self, text: &str) {
        self.tray.update(|t| t.state = State::Idle).await;
        if self.hud.is_available() {
            self.hud.offer(text);
        } else {
            self.hud.hide();
            let copied = desktop::copy_to_clipboard(text).await;
            let summary = if copied.is_ok() { "Nowhere to paste — copied to your clipboard" } else { "Nowhere to paste" };
            desktop::notify(summary, text);
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

/// Waits for the transcript of `samples`. None means nobody spoke. A retry
/// skips the voice check, in case it was wrong.
async fn transcribe(transcriber: &Transcriber, session: Session, samples: &[i16], check_speech: bool) -> Result<Option<String>> {
    if check_speech && !transcriber.has_speech(samples).await? {
        return Ok(None);
    }
    let text = session.finish().await?;
    Ok(Some(text).filter(|t| !t.is_empty()))
}

/// Pastes a finished transcription, keeping it as the last one, or offers a
/// retry in the HUD when it failed or heard no speech. Returns the recording
/// to keep for that retry.
async fn settle(
    ui: &mut Ui,
    paster: &mut paste::Paster,
    last: &mut Option<String>,
    result: Result<Option<String>>,
    samples: Vec<i16>,
) -> Option<Vec<i16>> {
    let (why, problem) = match result {
        Ok(Some(text)) => {
            deliver(ui, paster, &text).await;
            *last = Some(text);
            return None;
        }
        Ok(None) => (hud::Failure::NoSpeech, "no speech detected".to_string()),
        Err(e) => (hud::Failure::Error, format!("{e:#}")),
    };
    eprintln!("fishpr: {problem}");
    if !ui.hud.is_available() {
        desktop::notify("Transcription failed", &problem);
    }
    ui.set(State::Failed { why }).await;
    Some(samples)
}

/// Offers to transcribe `samples` after all, when the user cancelled with Esc.
async fn cancel(ui: &mut Ui, samples: Vec<i16>) -> Option<Vec<i16>> {
    eprintln!("fishpr: cancelled");
    ui.set(State::Failed { why: hud::Failure::Cancelled }).await;
    Some(samples)
}

/// Runs `work` unless the user presses Esc first. Input meanwhile is
/// dropped, as `drain` does, except Quit, which takes effect once `work` is done.
async fn unless_dismissed<T>(work: impl Future<Output = T>, events: &mut mpsc::UnboundedReceiver<Event>, quit: &mut bool) -> Option<T> {
    tokio::pin!(work);
    loop {
        tokio::select! {
            done = &mut work => return Some(done),
            event = events.recv() => match event {
                Some(Event::Dismiss) => return None,
                Some(Event::Quit) => *quit = true,
                Some(_) => {}
                None => return Some(work.await),
            },
        }
    }
}

/// Pastes the transcription into the focused window. When nothing takes it
/// (no text field has focus), the HUD shows it with a copy button instead.
async fn deliver(ui: &mut Ui, paster: &mut paste::Paster, text: &str) {
    match paster.paste(text).await {
        Ok(true) => ui.set(State::Idle).await,
        Ok(false) => ui.offer(text).await,
        Err(e) => {
            eprintln!("fishpr: paste failed: {e:#}");
            ui.offer(text).await;
        }
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

    let tray = FishTray { state: State::Loading, icon: tray_icon(), events: events_tx.clone() };
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
    let shortcut = match shortcut::Shortcut::register(events_tx.clone(), ui.hud.visibility()).await {
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
    // The last transcription, for the paste-last shortcut and the HUD's copy button.
    let mut last: Option<String> = None;

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
            (Event::Start | Event::Retry | Event::PasteLast, Some(r)) => recording = Some(r),
            (Event::Stop, None) => {}
            // Hides the retry button or the card.
            (Event::Dismiss, None) => {
                failed = None;
                ui.set(State::Idle).await;
            }
            // Stops recording without transcribing; the retry button transcribes it after all.
            (Event::Dismiss, Some((r, _session))) => match r.stop().await {
                Ok(samples) => failed = cancel(&mut ui, samples).await,
                Err(e) => {
                    eprintln!("fishpr: {e:#}");
                    ui.set(State::Idle).await;
                }
            },
            (Event::PasteLast, None) => {
                let Some(text) = last.clone() else { continue };
                deliver(&mut ui, &mut paster, &text).await;
                quit |= drain(&mut events);
            }
            (Event::CopyLast, r) => {
                recording = r;
                if let Some(text) = &last
                    && let Err(e) = desktop::copy_to_clipboard(text).await
                {
                    desktop::notify("Couldn't copy", &format!("{e:#}"));
                }
            }
            (Event::Retry, None) => {
                let Some(samples) = failed.take() else { continue };
                ui.set(State::Transcribing).await;
                let work = transcribe(&transcriber, transcriber.replay(&samples), &samples, false);
                failed = match unless_dismissed(work, &mut events, &mut quit).await {
                    Some(result) => settle(&mut ui, &mut paster, &mut last, result, samples).await,
                    None => cancel(&mut ui, samples).await,
                };
                quit |= drain(&mut events);
            }
            (Event::Toggle | Event::Stop, Some((r, session))) => {
                ui.set(State::Transcribing).await;
                match r.stop().await {
                    Ok(samples) => {
                        let work = transcribe(&transcriber, session, &samples, true);
                        failed = match unless_dismissed(work, &mut events, &mut quit).await {
                            Some(result) => settle(&mut ui, &mut paster, &mut last, result, samples).await,
                            None => cancel(&mut ui, samples).await,
                        };
                    }
                    // Nothing was recorded, so there's nothing to retry.
                    Err(e) => {
                        eprintln!("fishpr: {e:#}");
                        desktop::notify("Recording failed", &format!("{e:#}"));
                        ui.set(State::Idle).await;
                    }
                }
                quit |= drain(&mut events);
            }
        }
    }

    ui.set(State::Idle).await;
    if let Some(s) = &shortcut {
        s.unregister().await;
    }
    Ok(())
}
