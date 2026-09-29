mod desktop;
mod recorder;
mod transcribe;

use std::path::PathBuf;

use anyhow::Result;
use image::{GenericImageView, imageops::FilterType};
use ksni::TrayMethods;
use tokio::sync::mpsc;

use recorder::Recording;
use transcribe::Transcriber;

const ICON_SIZE: u32 = 64;

#[derive(Clone, Copy)]
enum State {
    Idle,
    Recording,
    Transcribing,
}

struct Icons {
    idle: ksni::Icon,
    active: ksni::Icon,
    busy: ksni::Icon,
}

struct FishTray {
    state: State,
    icons: Icons,
    clicks: mpsc::UnboundedSender<()>,
}

impl ksni::Tray for FishTray {
    fn id(&self) -> String {
        env!("CARGO_PKG_NAME").into()
    }

    fn title(&self) -> String {
        "fishpr".into()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.clicks.send(());
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let icon = match self.state {
            State::Idle => &self.icons.idle,
            State::Recording => &self.icons.active,
            State::Transcribing => &self.icons.busy,
        };
        vec![icon.clone()]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let description = match self.state {
            State::Idle => "Click to start recording",
            State::Recording => "Recording… click to stop and transcribe",
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
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Decodes a PNG, trims transparent padding, and scales it to a square tray icon.
fn load_icon(png: &[u8], opacity: f32) -> ksni::Icon {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).expect("valid icon png");
    let rgba = img.to_rgba8();
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, p) in rgba.enumerate_pixels() {
        if p[3] > 8 {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
    }
    let img = if x0 <= x1 {
        // Keep it square so the tray doesn't stretch it.
        let side = (x1 - x0 + 1).max(y1 - y0 + 1);
        let cx = (x0 + x1) / 2;
        let cy = (y0 + y1) / 2;
        let left = cx.saturating_sub(side / 2).min(img.width().saturating_sub(side));
        let top = cy.saturating_sub(side / 2).min(img.height().saturating_sub(side));
        img.crop_imm(left, top, side.min(img.width()), side.min(img.height()))
    } else {
        img
    };
    let img = img.resize_exact(ICON_SIZE, ICON_SIZE, FilterType::Lanczos3);
    let (width, height) = img.dimensions();
    let mut data = img.into_rgba8().into_vec();
    for px in data.chunks_exact_mut(4) {
        px[3] = (px[3] as f32 * opacity) as u8;
        px.rotate_right(1); // RGBA -> ARGB
    }
    ksni::Icon { width: width as i32, height: height as i32, data }
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

#[tokio::main]
async fn main() -> Result<()> {
    let idle_png = include_bytes!("../assets/icon-idle.png");
    let active_png = include_bytes!("../assets/icon-active.png");
    let icons = Icons {
        idle: load_icon(idle_png, 1.0),
        active: load_icon(active_png, 1.0),
        busy: load_icon(active_png, 0.45),
    };

    let (clicks_tx, mut clicks) = mpsc::unbounded_channel();
    let tray = FishTray { state: State::Idle, icons, clicks: clicks_tx };
    let handle = tray.spawn().await?;
    let set_state = |state: State| {
        let handle = handle.clone();
        async move { handle.update(|t| t.state = state).await }
    };

    let transcriber = Transcriber::new()?;
    let path = recording_path();
    let mut recording: Option<Recording> = None;

    while clicks.recv().await.is_some() {
        match recording.take() {
            None => match Recording::start(&path) {
                Ok(r) => {
                    recording = Some(r);
                    set_state(State::Recording).await;
                }
                Err(e) => desktop::notify("Couldn't start recording", &format!("{e:#}")),
            },
            Some(r) => {
                set_state(State::Transcribing).await;
                match finish(r, &transcriber).await {
                    Ok(text) => desktop::notify("Copied to clipboard", &text),
                    Err(e) => {
                        eprintln!("fishpr: {e:#}");
                        desktop::notify("Transcription failed", &format!("{e:#}"));
                    }
                }
                // Clicks made while transcribing shouldn't start a new recording.
                while clicks.try_recv().is_ok() {}
                set_state(State::Idle).await;
            }
        }
    }
    Ok(())
}
