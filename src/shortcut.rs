//! Global push-to-talk shortcut via KDE's KGlobalAccel (built into KWin).
//!
//! KGlobalAccel reports both press and release, which is what hold-to-talk
//! needs. Ctrl+Space is registered as the *default*, so users can rebind it in
//! System Settings → Shortcuts → fishpr and the choice sticks.

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tokio::sync::mpsc::UnboundedSender;
use zbus::zvariant::OwnedObjectPath;

use crate::Event;

const COMPONENT: &str = "fishpr";
const ACTION: &str = "push-to-talk";

// Qt key codes: Qt::ControlModifier | Qt::Key_Space.
const CTRL_SPACE: i32 = 0x0400_0000 | 0x20;

// kglobalacceld's SetShortcutFlag values.
const SET_PRESENT: u32 = 2;
const IS_DEFAULT: u32 = 8;

#[zbus::proxy(interface = "org.kde.KGlobalAccel", default_service = "org.kde.kglobalaccel", default_path = "/kglobalaccel")]
trait KGlobalAccel {
    #[zbus(name = "doRegister")]
    fn do_register(&self, action_id: &[&str]) -> zbus::Result<()>;

    /// Returns the keys actually in effect (a saved user choice wins over ours).
    #[zbus(name = "setShortcutKeys")]
    fn set_shortcut_keys(&self, action_id: &[&str], keys: &[(Vec<i32>,)], flags: u32) -> zbus::Result<Vec<(Vec<i32>,)>>;

    #[zbus(name = "setInactive")]
    fn set_inactive(&self, action_id: &[&str]) -> zbus::Result<()>;

    #[zbus(name = "getComponent")]
    fn get_component(&self, component_unique: &str) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(interface = "org.kde.kglobalaccel.Component", default_service = "org.kde.kglobalaccel")]
trait Component {
    #[zbus(signal, name = "globalShortcutPressed")]
    fn global_shortcut_pressed(&self, component_unique: String, shortcut_unique: String, timestamp: i64) -> zbus::Result<()>;

    #[zbus(signal, name = "globalShortcutReleased")]
    fn global_shortcut_released(&self, component_unique: String, shortcut_unique: String, timestamp: i64) -> zbus::Result<()>;
}

/// Keeps the shortcut registered; call `unregister` before exiting so KWin
/// stops grabbing the keys while fishpr isn't running.
pub struct Shortcut {
    accel: KGlobalAccelProxy<'static>,
}

fn action_id() -> [&'static str; 4] {
    [COMPONENT, ACTION, "fishpr", "Push to talk (hold)"]
}

impl Shortcut {
    pub async fn register(events: UnboundedSender<Event>) -> Result<Self> {
        let conn = zbus::Connection::session().await?;
        let accel = KGlobalAccelProxy::new(&conn).await.context("connecting to KGlobalAccel")?;
        let id = action_id();
        accel.do_register(&id).await?;
        accel.set_shortcut_keys(&id, &[(vec![CTRL_SPACE],)], IS_DEFAULT).await?;
        accel.set_shortcut_keys(&id, &[(vec![CTRL_SPACE],)], SET_PRESENT).await?;

        let path = accel.get_component(COMPONENT).await?;
        let component = ComponentProxy::builder(&conn).path(path)?.build().await?;
        let mut pressed = component.receive_global_shortcut_pressed().await?;
        let mut released = component.receive_global_shortcut_released().await?;
        tokio::spawn(async move {
            loop {
                let (signal, event) = tokio::select! {
                    Some(s) = pressed.next() => (s.args().map(|a| a.shortcut_unique == ACTION), Event::Start),
                    Some(s) = released.next() => (s.args().map(|a| a.shortcut_unique == ACTION), Event::Stop),
                    else => break,
                };
                if matches!(signal, Ok(true)) && events.send(event).is_err() {
                    break;
                }
            }
        });
        Ok(Self { accel })
    }

    pub async fn unregister(&self) {
        let _ = self.accel.set_inactive(&action_id()).await;
    }
}
