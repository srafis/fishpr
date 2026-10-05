//! Pastes into the focused window by pressing Ctrl+V (KWin doesn't allow
//! wtype-style fake input, so we go below or around the compositor). The text
//! is on the clipboard only for that paste; see clipboard.rs.
//!
//! Preferred: a virtual keyboard on /dev/uinput. Silent, but needs write
//! access to the device (KDE Connect's udev rule grants it to the logged-in
//! user). Fallback: the XDG RemoteDesktop portal, which works everywhere but
//! asks permission once and shows a "remote control" notification per paste.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ashpd::desktop::{
    PersistMode,
    remote_desktop::{DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions},
};
use evdev::{AttributeSet, EventType, InputEvent, KeyCode, uinput::VirtualDevice};
use smithay_client_toolkit::reexports::client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::wl_registry,
};
use wayland_protocols_plasma::keystate::client::org_kde_kwin_keystate::{self, OrgKdeKwinKeystate};

use crate::{clipboard, desktop};

const XK_CONTROL_L: i32 = 0xffe3;
const XK_V: i32 = 0x0076;
/// How long to wait for the user to let go of Ctrl, Alt, Shift and Meta.
const MODIFIERS_WAIT: Duration = Duration::from_secs(10);

pub struct Paster {
    keyboard: Option<VirtualDevice>,
}

impl Paster {
    /// Create this at startup: the compositor needs a moment to pick up a
    /// new input device before it will route its keys.
    pub fn new() -> Self {
        let keyboard = virtual_keyboard()
            .inspect_err(|e| eprintln!("fishpr: no uinput access ({e}), pasting via the desktop portal"))
            .ok();
        Self { keyboard }
    }

    /// Pastes `text` into the focused window, leaving the clipboard as it
    /// was. Says whether an app took the text; false means nothing that
    /// accepts text had focus, or the user kept holding modifier keys.
    pub async fn paste(&mut self, text: &str) -> Result<bool> {
        // Held from a shortcut, they'd turn Ctrl+V into, say, Ctrl+Alt+V,
        // so the paste waits for the user to let go.
        match tokio::task::spawn_blocking(|| modifiers_released(MODIFIERS_WAIT)).await? {
            Ok(true) => {}
            Ok(false) => {
                eprintln!("fishpr: modifier keys still held, not pasting");
                return Ok(false);
            }
            Err(e) => eprintln!("fishpr: can't see the modifier keys ({e:#}), pasting anyway"),
        }
        let lease = tokio::task::spawn_blocking({
            let text = text.to_owned();
            move || clipboard::lend(&text)
        })
        .await?;
        let lease = match lease {
            Ok(lease) => lease,
            // Older Plasma lacks ext-data-control; the text stays on the clipboard there.
            Err(e) => {
                eprintln!("fishpr: can't lend the clipboard ({e:#}), leaving the text on it");
                desktop::copy_to_clipboard(text).await?;
                // Give wl-copy's background process a moment to take clipboard ownership.
                tokio::time::sleep(Duration::from_millis(100)).await;
                self.press().await?;
                return Ok(true);
            }
        };
        let pressed = self.press().await;
        let landed = tokio::task::spawn_blocking(move || lease.give_back()).await?;
        pressed.map(|()| landed)
    }

    async fn press(&mut self) -> Result<()> {
        match &mut self.keyboard {
            Some(keyboard) => uinput_paste(keyboard).await,
            None => portal_paste().await,
        }
    }
}

/// Waits up to `timeout` until Ctrl, Alt, Shift, AltGr and Meta are all up,
/// as KWin's keystate protocol reports them. False if some are still down.
fn modifiers_released(timeout: Duration) -> Result<bool> {
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<Modifiers>(&conn)?;
    // Version 5 is the first to report modifiers.
    let keystate: OrgKdeKwinKeystate = globals.bind(&queue.handle(), 5..=5, ()).context("KWin's keystate protocol is missing")?;
    let mut held = Modifiers::default();
    keystate.fetchStates();
    queue.roundtrip(&mut held)?;
    let deadline = Instant::now() + timeout;
    while held.any() && Instant::now() < deadline {
        clipboard::dispatch_for(&mut queue, &mut held, Duration::from_millis(20))?;
    }
    keystate.destroy();
    Ok(!held.any())
}

/// Whether each modifier is down, by keystate's key number.
#[derive(Default)]
struct Modifiers([bool; 8]);

impl Modifiers {
    fn any(&self) -> bool {
        // 0–2 are Caps, Num and Scroll Lock, which don't matter.
        self.0[3..].iter().any(|&down| down)
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Modifiers {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<OrgKdeKwinKeystate, ()> for Modifiers {
    fn event(held: &mut Self, _: &OrgKdeKwinKeystate, event: org_kde_kwin_keystate::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let org_kde_kwin_keystate::Event::StateChanged { key, state } = event {
            // Anything but "unlocked": pressed, or latched by sticky keys.
            if let Some(down) = held.0.get_mut(key as usize) {
                *down = state != 0;
            }
        }
    }
}

fn virtual_keyboard() -> std::io::Result<VirtualDevice> {
    let keys = AttributeSet::from_iter([KeyCode::KEY_LEFTCTRL, KeyCode::KEY_V]);
    VirtualDevice::builder()?.name("fishpr virtual keyboard").with_keys(&keys)?.build()
}

async fn uinput_paste(keyboard: &mut VirtualDevice) -> Result<()> {
    for (key, value) in [
        (KeyCode::KEY_LEFTCTRL, 1),
        (KeyCode::KEY_V, 1),
        (KeyCode::KEY_V, 0),
        (KeyCode::KEY_LEFTCTRL, 0),
    ] {
        keyboard
            .emit(&[InputEvent::new(EventType::KEY.0, key.0, value)])
            .context("sending key via uinput")?;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

/// The first call shows the desktop's "allow input control" dialog. The portal hands
/// back a single-use restore token that we save, so later calls skip it.
async fn portal_paste() -> Result<()> {
    let token_path = crate::data_dir()?.join("portal-token");
    let token = tokio::fs::read_to_string(&token_path).await.ok();

    let proxy = RemoteDesktop::new().await.context("connecting to the desktop portal")?;
    let session = proxy.create_session(Default::default()).await?;
    proxy
        .select_devices(
            &session,
            SelectDevicesOptions::default()
                .set_devices(ashpd::enumflags2::BitFlags::from(DeviceType::Keyboard))
                .set_persist_mode(PersistMode::ExplicitlyRevoked)
                .set_restore_token(token.as_deref()),
        )
        .await?
        .response()?;
    let started = proxy
        .start(&session, None, Default::default())
        .await?
        .response()
        .context("input control permission was not granted")?;
    if let Some(token) = started.restore_token() {
        tokio::fs::create_dir_all(token_path.parent().unwrap()).await?;
        tokio::fs::write(&token_path, token).await?;
    }

    let result = async {
        for (keysym, state) in [
            (XK_CONTROL_L, KeyState::Pressed),
            (XK_V, KeyState::Pressed),
            (XK_V, KeyState::Released),
            (XK_CONTROL_L, KeyState::Released),
        ] {
            proxy.notify_keyboard_keysym(&session, keysym, state, Default::default()).await?;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        anyhow::Ok(())
    }
    .await;
    let _ = session.close().await;
    result
}
