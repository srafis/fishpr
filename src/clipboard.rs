//! Lends the transcription to the clipboard for a single paste. Ctrl+V pastes
//! whatever is on the clipboard, so the text has to be there for a moment:
//! we save what the clipboard holds, put the text on it marked as a secret
//! (so Klipper keeps no history of it), and once the app has fetched it, put
//! back what was there. Seeing whether any app fetched it also tells us
//! whether the paste landed anywhere.
//!
//! Done through ext-data-control, the clipboard-manager protocol, which
//! doesn't need a focused window. Runs blocking; call from a blocking task.

use std::{
    collections::HashMap,
    io::{ErrorKind, Read, Write},
    os::fd::{AsFd, AsRawFd, OwnedFd},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use smithay_client_toolkit::reexports::{
    client::{
        Connection, Dispatch, EventQueue, Proxy, QueueHandle,
        backend::ObjectId,
        event_created_child,
        globals::{GlobalListContents, registry_queue_init},
        protocol::{
            wl_registry,
            wl_seat::{self, WlSeat},
        },
    },
    protocols::ext::data_control::v1::client::{
        ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
        ext_data_control_manager_v1::{self, ExtDataControlManagerV1},
        ext_data_control_offer_v1::{self, ExtDataControlOfferV1},
        ext_data_control_source_v1::{self, ExtDataControlSourceV1},
    },
};

/// Klipper skips clipboard entries that offer this type with the value "secret".
const SECRET_HINT: &str = "x-kde-passwordManagerHint";
const TEXT_TYPES: [&str; 5] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "STRING", "TEXT"];
/// How long saving the clipboard may take. A type that isn't read by then is
/// left out of what's put back.
const SAVE_WAIT: Duration = Duration::from_millis(500);
/// Clipboard managers read a new clipboard right away; give them this long
/// before pasting, so their reads aren't mistaken for the paste.
const SETTLE: Duration = Duration::from_millis(40);
/// How long after Ctrl+V an app has to fetch the text before we decide the
/// paste went nowhere.
const PASTE_WAIT: Duration = Duration::from_secs(1);
/// After the last fetch, how long to wait for more (an app may ask for the
/// text in several formats) before putting the old clipboard back.
const QUIET: Duration = Duration::from_millis(250);

/// What a clipboard holds: each MIME type it offers, with its data.
type Contents = Arc<Vec<(String, Vec<u8>)>>;

/// What fishpr last put back on the clipboard, while it's still there. The
/// next paste saves the clipboard by taking this, rather than reading it all
/// back, so pasting again right away is quick.
static RESTORED: Mutex<Option<Contents>> = Mutex::new(None);

/// The text on loan to the clipboard. Hand it back with `give_back`.
pub struct Lease {
    queue: EventQueue<State>,
    state: State,
    lent: ExtDataControlSourceV1,
    /// What to put back; None if the clipboard was empty.
    saved: Option<Contents>,
}

/// Saves the clipboard and puts `text` on it, ready for Ctrl+V. Fails if the
/// desktop lacks ext-data-control.
pub fn lend(text: &str) -> Result<Lease> {
    let conn = Connection::connect_to_env().context("connecting to Wayland")?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
    let qh = queue.handle();
    let manager: ExtDataControlManagerV1 = globals.bind(&qh, 1..=1, ()).context("the desktop doesn't support ext-data-control")?;
    let seat: WlSeat = globals.bind(&qh, 1..=1, ()).context("no seat")?;
    let device = manager.get_data_device(&seat, &qh, ());
    let mut state = State { manager, device, selection: None, sources: HashMap::new(), fetched: None, finished: false };
    // The device reports the current clipboard as soon as it's created.
    queue.roundtrip(&mut state)?;

    let saved = state.selection.take().map(|offer| {
        let types = worth_saving(offer.data::<Mutex<Vec<String>>>().map(|t| t.lock().unwrap().clone()).unwrap_or_default());
        let ours = RESTORED.lock().unwrap().clone().filter(|c| c.iter().map(|(t, _)| t).eq(types.iter()));
        let contents = ours.unwrap_or_else(|| save(&conn, &offer, types));
        offer.destroy();
        contents
    });
    // Nothing read in time: clearing the clipboard lets Klipper put its copy back.
    let saved = saved.filter(|c| !c.is_empty());
    let mut contents: Vec<(String, Vec<u8>)> = TEXT_TYPES.iter().map(|t| (t.to_string(), text.as_bytes().to_vec())).collect();
    contents.push((SECRET_HINT.into(), b"secret".to_vec()));
    let lent = state.offer(&qh, Arc::new(contents));
    dispatch_for(&mut queue, &mut state, SETTLE)?;
    state.fetched = None;
    Ok(Lease { queue, state, lent, saved })
}

impl Lease {
    /// Call right after pressing Ctrl+V. Waits for an app to fetch the text,
    /// then puts the old clipboard back. Says whether any app fetched it,
    /// meaning the paste landed.
    pub fn give_back(mut self) -> bool {
        let start = Instant::now();
        loop {
            // Someone else put something on the clipboard; leave it be.
            if self.state.sources[&self.lent.id()].cancelled || self.state.finished {
                return self.state.fetched.is_some();
            }
            let waited = match self.state.fetched {
                Some(last) => last.elapsed() >= QUIET,
                None => start.elapsed() >= PASTE_WAIT,
            };
            if waited || dispatch_for(&mut self.queue, &mut self.state, Duration::from_millis(20)).is_err() {
                break;
            }
        }
        let pasted = self.state.fetched.is_some();
        let qh = self.queue.handle();
        *RESTORED.lock().unwrap() = self.saved.clone();
        match self.saved.take() {
            Some(contents) => {
                self.state.offer(&qh, contents);
            }
            None => self.state.device.set_selection(None),
        }
        // Keep serving the old clipboard, until something replaces it.
        std::thread::spawn(move || {
            let Self { mut queue, mut state, .. } = self;
            while state.sources.values().any(|s| !s.cancelled) && !state.finished {
                if queue.blocking_dispatch(&mut state).is_err() {
                    break;
                }
            }
        });
        pasted
    }
}

struct Source {
    contents: Contents,
    cancelled: bool,
}

struct State {
    manager: ExtDataControlManagerV1,
    device: ExtDataControlDeviceV1,
    selection: Option<ExtDataControlOfferV1>,
    sources: HashMap<ObjectId, Source>,
    /// When an app last fetched text from the lent clipboard.
    fetched: Option<Instant>,
    /// The compositor took the data device away.
    finished: bool,
}

impl State {
    /// Puts `contents` on the clipboard.
    fn offer(&mut self, qh: &QueueHandle<Self>, contents: Contents) -> ExtDataControlSourceV1 {
        let source = self.manager.create_data_source(qh, ());
        for (mime, _) in contents.iter() {
            source.offer(mime.clone());
        }
        self.device.set_selection(Some(&source));
        self.sources.insert(source.id(), Source { contents, cancelled: false });
        source
    }
}

/// The types to save of those a clipboard offers. Qt apps (Klipper,
/// Spectacle, Dolphin) offer an image in dozens of formats, each converted
/// when asked for, one after another, which takes seconds for a screenshot.
/// Every app takes PNG, so with PNG on offer, that's the only image format
/// kept. Some apps list a type twice, and answer only one request for it.
fn worth_saving(mut types: Vec<String>) -> Vec<String> {
    let has_png = types.iter().any(|t| t == "image/png");
    let mut seen = std::collections::HashSet::new();
    types.retain(|t| (!has_png || t == "image/png" || !t.starts_with("image/")) && seen.insert(t.clone()));
    types
}

/// Reads each of `types` from `offer`, all at once, so a slow one doesn't hold up the rest.
fn save(conn: &Connection, offer: &ExtDataControlOfferV1, types: Vec<String>) -> Contents {
    let mut pending = Vec::new();
    for mime in types {
        let Ok((reader, writer)) = std::io::pipe() else { continue };
        offer.receive(mime.clone(), writer.as_fd());
        drop(writer);
        // Nonblocking, so one poll can wait on all of them.
        let fd = reader.as_raw_fd();
        unsafe { libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK) };
        pending.push((mime, reader, Vec::new()));
    }
    let _ = conn.flush();

    let deadline = Instant::now() + SAVE_WAIT;
    let mut saved = Vec::new();
    while !pending.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let late: Vec<&str> = pending.iter().map(|(mime, _, _)| mime.as_str()).collect();
            eprintln!("fishpr: couldn't save the clipboard's {} in time", late.join(", "));
            break;
        }
        let mut fds: Vec<libc::pollfd> = pending.iter().map(|(_, r, _)| libc::pollfd { fd: r.as_raw_fd(), events: libc::POLLIN, revents: 0 }).collect();
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, left.as_millis() as i32) } < 0 {
            break;
        }
        let mut i = 0;
        for fd in fds {
            let (_, reader, data) = &mut pending[i];
            let mut done = false;
            if fd.revents != 0 {
                let mut chunk = [0; 65536];
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) => break done = true,
                        Ok(n) => data.extend_from_slice(&chunk[..n]),
                        Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == ErrorKind::Interrupted => {}
                        Err(_) => break done = true,
                    }
                }
            }
            if done {
                let (mime, _, data) = pending.remove(i);
                saved.push((mime, data));
            } else {
                i += 1;
            }
        }
    }
    Arc::new(saved)
}

/// Handles Wayland events for up to `timeout`.
pub fn dispatch_for<S>(queue: &mut EventQueue<S>, state: &mut S, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        queue.dispatch_pending(state)?;
        queue.flush()?;
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        let Some(guard) = queue.prepare_read() else { continue };
        let mut fd = libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 };
        if unsafe { libc::poll(&mut fd, 1, left.as_millis().max(1) as i32) } > 0 {
            guard.read()?;
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlSeat, ()> for State {
    fn event(_: &mut Self, _: &WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ExtDataControlManagerV1, ()> for State {
    fn event(_: &mut Self, _: &ExtDataControlManagerV1, _: ext_data_control_manager_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ExtDataControlDeviceV1, ()> for State {
    fn event(state: &mut Self, _: &ExtDataControlDeviceV1, event: ext_data_control_device_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            ext_data_control_device_v1::Event::Selection { id } => {
                if let Some(old) = std::mem::replace(&mut state.selection, id) {
                    old.destroy();
                }
            }
            ext_data_control_device_v1::Event::PrimarySelection { id: Some(offer) } => offer.destroy(),
            ext_data_control_device_v1::Event::Finished => state.finished = true,
            _ => {}
        }
    }

    event_created_child!(State, ExtDataControlDeviceV1, [
        ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, Mutex::new(Vec::new())),
    ]);
}

impl Dispatch<ExtDataControlOfferV1, Mutex<Vec<String>>> for State {
    fn event(_: &mut Self, _: &ExtDataControlOfferV1, event: ext_data_control_offer_v1::Event, types: &Mutex<Vec<String>>, _: &Connection, _: &QueueHandle<Self>) {
        if let ext_data_control_offer_v1::Event::Offer { mime_type } = event {
            types.lock().unwrap().push(mime_type);
        }
    }
}

impl Dispatch<ExtDataControlSourceV1, ()> for State {
    fn event(state: &mut Self, source: &ExtDataControlSourceV1, event: ext_data_control_source_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let Some(entry) = state.sources.get_mut(&source.id()) else { return };
        match event {
            ext_data_control_source_v1::Event::Send { mime_type, fd } => {
                if mime_type != SECRET_HINT {
                    state.fetched = Some(Instant::now());
                }
                let data = entry.contents.clone();
                // Write from a thread: a pipe holds only 64 KiB until the app reads it.
                std::thread::spawn(move || write_type(fd, &data, &mime_type));
            }
            ext_data_control_source_v1::Event::Cancelled => {
                entry.cancelled = true;
                source.destroy();
                let mut restored = RESTORED.lock().unwrap();
                if restored.as_ref().is_some_and(|c| Arc::ptr_eq(c, &entry.contents)) {
                    *restored = None;
                }
            }
            _ => {}
        }
    }
}

fn write_type(fd: OwnedFd, contents: &[(String, Vec<u8>)], mime: &str) {
    if let Some((_, data)) = contents.iter().find(|(m, _)| m == mime) {
        let _ = std::fs::File::from(fd).write_all(data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(list: &[&str]) -> Vec<String> {
        list.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn keeps_only_png_of_an_image() {
        let offered = types(&["application/x-kde-suggestedfilename", "image/png", "application/x-qt-image", "image/bmp", "image/heic", "image/jpeg"]);
        assert_eq!(worth_saving(offered), types(&["application/x-kde-suggestedfilename", "image/png", "application/x-qt-image"]));
    }

    #[test]
    fn keeps_other_image_formats_without_png() {
        assert_eq!(worth_saving(types(&["image/jpeg", "image/webp"])), types(&["image/jpeg", "image/webp"]));
    }

    #[test]
    fn asks_for_each_type_once() {
        let offered = types(&["text/plain", "text/plain", "text/plain;charset=utf-8", "TEXT", "STRING", "UTF8_STRING"]);
        assert_eq!(worth_saving(offered), types(&["text/plain", "text/plain;charset=utf-8", "TEXT", "STRING", "UTF8_STRING"]));
    }
}
