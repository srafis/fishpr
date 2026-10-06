//! The history, as the window's QML sees it: `History` lists the entries as
//! JSON in `entries`, and does what the buttons on each one ask.

use std::{
    io::Write,
    pin::Pin,
    process::{Child, Command, Stdio},
    time::SystemTime,
};

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QString, QUrl};

use crate::history;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qurl.h");
        type QUrl = cxx_qt_lib::QUrl;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(QString, entries)]
        #[qproperty(QString, playing)]
        type History = super::HistoryRust;

        /// Reads the history again if it changed, and notices playback ending.
        #[qinvokable]
        fn refresh(self: Pin<&mut History>);

        #[qinvokable]
        fn copy(self: &History, id: &QString) -> bool;

        /// Plays the entry's recording, or stops it if it's playing.
        #[qinvokable]
        fn play(self: Pin<&mut History>, id: &QString);

        #[qinvokable]
        #[cxx_name = "showInFolder"]
        fn show_in_folder(self: &History, id: &QString);

        #[qinvokable]
        #[cxx_name = "saveAudio"]
        fn save_audio(self: &History, id: &QString, to: &QUrl) -> bool;

        #[qinvokable]
        fn remove(self: Pin<&mut History>, id: &QString) -> bool;
    }
}

#[derive(Default)]
pub struct HistoryRust {
    /// JSON: `[{"id", "date", "time", "text"}]`, newest first.
    entries: QString,
    /// The ID of the entry whose recording is playing, or empty.
    playing: QString,
    player: Option<Child>,
    /// When the history folder last changed, as of the last read.
    read: Option<SystemTime>,
}

impl qobject::History {
    fn refresh(mut self: Pin<&mut Self>) {
        if let Some(player) = self.as_mut().rust_mut().player.as_mut()
            && !matches!(player.try_wait(), Ok(None))
        {
            self.as_mut().rust_mut().player = None;
            self.as_mut().set_playing(QString::default());
        }
        let changed = history::dir().ok().and_then(|dir| std::fs::metadata(dir).and_then(|m| m.modified()).ok());
        if self.read.is_some() && changed == self.read {
            return;
        }
        self.as_mut().rust_mut().read = changed;
        let entries: Vec<_> = history::list()
            .unwrap_or_default()
            .iter()
            .map(|e| serde_json::json!({ "id": e.id, "date": e.date(), "time": e.time(), "text": e.text }))
            .collect();
        self.as_mut().set_entries(QString::from(&serde_json::Value::from(entries).to_string()));
    }

    fn copy(&self, id: &QString) -> bool {
        let Some(entry) = find(id) else { return false };
        let copied = Command::new("wl-copy").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().and_then(|mut child| {
            child.stdin.take().expect("stdin is piped").write_all(entry.text.as_bytes())?;
            // wl-copy forks to keep serving the clipboard, and exits.
            child.wait()
        });
        copied.is_ok_and(|status| status.success())
    }

    fn play(mut self: Pin<&mut Self>, id: &QString) {
        let again = self.playing == *id;
        if let Some(mut player) = self.as_mut().rust_mut().player.take() {
            let _ = player.kill();
            let _ = player.wait();
        }
        if again {
            self.as_mut().set_playing(QString::default());
            return;
        }
        let Some(audio) = find(id).and_then(|e| e.audio().ok()) else { return };
        match Command::new("pw-play").arg(audio).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            Ok(player) => {
                self.as_mut().rust_mut().player = Some(player);
                self.as_mut().set_playing(id.clone());
            }
            Err(e) => eprintln!("fishpr: couldn't play the recording: {e}"),
        }
    }

    /// Opens the history folder in the file manager, with the recording selected.
    fn show_in_folder(&self, id: &QString) {
        let Some(audio) = find(id).and_then(|e| e.audio().ok()) else { return };
        let url = format!("file://{}", audio.display());
        super::runtime().spawn(async move {
            let shown = async {
                let conn = zbus::Connection::session().await?;
                conn.call_method(
                    Some("org.freedesktop.FileManager1"),
                    "/org/freedesktop/FileManager1",
                    Some("org.freedesktop.FileManager1"),
                    "ShowItems",
                    &(vec![url], ""),
                )
                .await?;
                zbus::Result::Ok(())
            };
            if let Err(e) = shown.await {
                eprintln!("fishpr: couldn't show the recording in the file manager: {e}");
                if let Ok(dir) = history::dir() {
                    let _ = Command::new("xdg-open").arg(dir).spawn().map(|mut c| std::thread::spawn(move || c.wait()));
                }
            }
        });
    }

    fn save_audio(&self, id: &QString, to: &QUrl) -> bool {
        let Some(audio) = find(id).and_then(|e| e.audio().ok()) else { return false };
        let to = to.to_local_file_or_default().to_string();
        !to.is_empty() && std::fs::copy(audio, to).inspect_err(|e| eprintln!("fishpr: couldn't save the recording: {e}")).is_ok()
    }

    fn remove(mut self: Pin<&mut Self>, id: &QString) -> bool {
        if self.playing == *id {
            self.as_mut().play(&id.clone());
        }
        let removed = history::delete(&id.to_string()).inspect_err(|e| eprintln!("fishpr: couldn't delete from the history: {e:#}")).is_ok();
        self.refresh();
        removed
    }
}

fn find(id: &QString) -> Option<history::Entry> {
    let id = id.to_string();
    history::list().ok()?.into_iter().find(|e| e.id == id)
}

impl Drop for HistoryRust {
    fn drop(&mut self) {
        if let Some(player) = &mut self.player {
            let _ = player.kill();
        }
    }
}
