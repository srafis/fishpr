//! Pastes into the focused window by pressing Ctrl+V (KWin doesn't allow
//! wtype-style fake input, so we go below or around the compositor).
//!
//! Preferred: a virtual keyboard on /dev/uinput. Silent, but needs write
//! access to the device (KDE Connect's udev rule grants it to the logged-in
//! user). Fallback: the XDG RemoteDesktop portal, which works everywhere but
//! asks permission once and shows a "remote control" notification per paste.

use std::time::Duration;

use anyhow::{Context, Result};
use ashpd::desktop::{
    PersistMode,
    remote_desktop::{DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions},
};
use evdev::{AttributeSet, EventType, InputEvent, KeyCode, uinput::VirtualDevice};

const XK_CONTROL_L: i32 = 0xffe3;
const XK_V: i32 = 0x0076;

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

    pub async fn paste(&mut self) -> Result<()> {
        match &mut self.keyboard {
            Some(keyboard) => uinput_paste(keyboard).await,
            None => portal_paste().await,
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
