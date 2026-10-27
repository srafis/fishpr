//! Global push-to-talk shortcut via KDE's KGlobalAccel (built into KWin).
//!
//! KGlobalAccel reports both press and release, which is what hold-to-talk
//! needs. Ctrl+Space, Ctrl+Alt+V to paste the last transcription again, and
//! Esc to cancel or dismiss the HUD are registered as *defaults*, so users can
//! rebind them in System Settings → Shortcuts → fishpr and the choice sticks.
//!
//! Ctrl+Space can be configured as hold-to-talk or press-to-toggle. The choice
//! comes from the tray menu and is read on every press, so it applies without
//! a restart. In toggle mode KGlobalAccel's repeated presses while a key is
//! held must not toggle again; only the first press counts.
//!
//! All component signals are read from one ordered D-Bus stream. Reading
//! presses and releases from separate streams lets a quick tap's release
//! overtake its press, which corrupts the held-key state.
//!
//! Esc is grabbed only while the HUD is on screen. KWin then keeps it from
//! the focused app; the rest of the time, apps get it as usual.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use tokio::sync::{mpsc::UnboundedSender, watch};
use zbus::zvariant::OwnedObjectPath;

use crate::Event;

const COMPONENT: &str = "fishpr";
const PUSH_TO_TALK: &str = "push-to-talk";
const PASTE_LAST: &str = "paste-last";
const DISMISS: &str = "dismiss";

// Qt key codes: Qt::ControlModifier | Qt::Key_Space,
// Qt::ControlModifier | Qt::AltModifier | Qt::Key_V, and Qt::Key_Escape.
const CTRL_SPACE: i32 = 0x0400_0000 | 0x20;
const CTRL_ALT_V: i32 = 0x0400_0000 | 0x0800_0000 | 0x56;
const ESC: i32 = 0x0100_0000;

const DISMISS_ID: [&str; 4] = [COMPONENT, DISMISS, "fishpr", "Cancel or dismiss (while showing)"];

/// Each action's ID (component, action, and their display names) and default keys.
const ACTIONS: [([&str; 4], i32); 2] = [
    ([COMPONENT, PUSH_TO_TALK, "fishpr", "Push to talk (hold)"], CTRL_SPACE),
    ([COMPONENT, PASTE_LAST, "fishpr", "Paste last transcription"], CTRL_ALT_V),
];

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

    /// The user rebound one of the app's shortcuts in System Settings.
    #[zbus(signal, name = "yourShortcutsChanged")]
    fn your_shortcuts_changed(&self, action_id: Vec<String>, new_keys: Vec<(Vec<i32>,)>) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "org.kde.kglobalaccel.Component", default_service = "org.kde.kglobalaccel")]
trait Component {
    #[zbus(signal, name = "globalShortcutPressed")]
    fn global_shortcut_pressed(&self, component_unique: String, shortcut_unique: String, timestamp: i64) -> zbus::Result<()>;

    #[zbus(signal, name = "globalShortcutReleased")]
    fn global_shortcut_released(&self, component_unique: String, shortcut_unique: String, timestamp: i64) -> zbus::Result<()>;
}

/// The keys in effect for the shortcuts the tray menu shows, as Qt key codes:
/// the first key sequence of each, or empty if it has none.
#[derive(Clone, Default, PartialEq)]
pub struct Bindings {
    pub push_to_talk: Vec<i32>,
    pub paste_last: Vec<i32>,
}

impl Bindings {
    fn set(&mut self, action: &str, keys: &[(Vec<i32>,)]) {
        let first = keys.iter().map(|(seq,)| seq.iter().copied().filter(|&k| k != 0).collect::<Vec<_>>()).find(|seq| !seq.is_empty());
        match action {
            PUSH_TO_TALK => self.push_to_talk = first.unwrap_or_default(),
            PASTE_LAST => self.paste_last = first.unwrap_or_default(),
            _ => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShortcutSignal {
    Pressed,
    Released,
}

#[derive(Default)]
struct ShortcutState {
    esc_down: bool,
    /// None while the keys are up; otherwise whether that press began in
    /// toggle mode, so its release doesn't stop a toggle recording.
    push_to_talk_down: Option<bool>,
}

/// Keeps the shortcuts registered; call `unregister` before exiting so KWin
/// stops grabbing the keys while fishpr isn't running.
pub struct Shortcut {
    accel: KGlobalAccelProxy<'static>,
}

impl Shortcut {
    /// Registers the shortcuts. Esc is grabbed while `hud_visible` is true.
    /// `bindings` follows the keys the user has bound, and `toggle_mode` says
    /// whether Ctrl+Space acts on press and release or toggles on each press.
    pub async fn register(
        events: UnboundedSender<Event>,
        mut hud_visible: watch::Receiver<bool>,
        bindings: watch::Sender<Bindings>,
        toggle_mode: Arc<AtomicBool>,
    ) -> Result<Self> {
        let conn = zbus::Connection::session().await?;
        if !is_kde(&conn).await {
            bail!("this isn't KDE Plasma, which fishpr's shortcut needs");
        }
        let accel = KGlobalAccelProxy::new(&conn).await.context("connecting to KGlobalAccel")?;
        let mut bound = Bindings::default();
        for (id, key) in ACTIONS {
            accel.do_register(&id).await?;
            accel.set_shortcut_keys(&id, &[(vec![key],)], IS_DEFAULT).await?;
            bound.set(id[1], &accel.set_shortcut_keys(&id, &[(vec![key],)], SET_PRESENT).await?);
        }
        bindings.send_replace(bound);
        let mut rebound = accel.receive_your_shortcuts_changed().await?;
        tokio::spawn(async move {
            while let Some(signal) = rebound.next().await {
                if let Ok(args) = signal.args()
                    && args.action_id.first().is_some_and(|c| c == COMPONENT)
                    && let Some(action) = args.action_id.get(1)
                {
                    bindings.send_modify(|b| b.set(action, &args.new_keys));
                }
            }
        });
        accel.do_register(&DISMISS_ID).await?;
        accel.set_shortcut_keys(&DISMISS_ID, &[(vec![ESC],)], IS_DEFAULT).await?;
        // Released, in case a fishpr that crashed left it grabbed.
        accel.set_inactive(&DISMISS_ID).await?;
        tokio::spawn({
            let accel = accel.clone();
            async move {
                while hud_visible.changed().await.is_ok() {
                    let shown = *hud_visible.borrow_and_update();
                    let grabbed = if shown {
                        accel.set_shortcut_keys(&DISMISS_ID, &[(vec![ESC],)], SET_PRESENT).await.map(drop)
                    } else {
                        accel.set_inactive(&DISMISS_ID).await
                    };
                    if let Err(e) = grabbed {
                        eprintln!("fishpr: couldn't {} Esc: {e}", if shown { "grab" } else { "release" });
                    }
                }
            }
        });

        let path = accel.get_component(COMPONENT).await?;
        let component = ComponentProxy::builder(&conn).path(path)?.build().await?;
        // Keep press and release in one D-Bus-ordered stream. Reading them
        // from separate streams lets tokio::select! process a quick tap's
        // release before its press, leaving the shortcut state one press behind.
        let mut signals = component.inner().receive_all_signals().await?;
        tokio::spawn(async move {
            // KGlobalAccel sends globalShortcutRepeated while a key is held;
            // it's ignored here. Paste-last acts on release, Esc only on its
            // first press, and push-to-talk only on the first press of each
            // physical key press.
            let mut state = ShortcutState::default();
            while let Some(message) = signals.next().await {
                let signal = match message.header().member().map(|member| member.as_str()) {
                    Some("globalShortcutPressed") => ShortcutSignal::Pressed,
                    Some("globalShortcutReleased") => ShortcutSignal::Released,
                    _ => continue,
                };
                let body = message.body();
                let Ok((_, shortcut, _)) = body.deserialize::<(String, String, i64)>() else {
                    continue;
                };
                let event = handle_signal(signal, &shortcut, toggle_mode.load(Ordering::Relaxed), &mut state);
                if let Some(event) = event
                    && events.send(event).is_err()
                {
                    break;
                }
            }
        });
        Ok(Self { accel })
    }

    pub async fn unregister(&self) {
        for id in ACTIONS.map(|(id, _)| id).into_iter().chain([DISMISS_ID]) {
            let _ = self.accel.set_inactive(&id).await;
        }
    }
}

/// The event for a push-to-talk press. Repeated presses while the keys are
/// held are ignored. In toggle mode the first press toggles recording and the
/// release does nothing; in hold mode the press starts and the release stops.
fn push_to_talk_pressed(toggle: bool, down: &mut Option<bool>) -> Option<Event> {
    if down.is_some() {
        return None;
    }
    *down = Some(toggle);
    Some(if toggle { Event::Toggle } else { Event::Start })
}

fn push_to_talk_released(toggle_now: bool, down: &mut Option<bool>) -> Option<Event> {
    match down.take() {
        // The press began in hold mode.
        Some(false) => Some(Event::Stop),
        // The mode changed to hold while a toggle-mode press was still held.
        Some(true) if !toggle_now => Some(Event::Stop),
        // A toggle-mode release does nothing.
        _ => None,
    }
}

/// Applies one KGlobalAccel signal in D-Bus order. Keeping this pure makes the
/// press/release ordering and held-key rules testable without a session bus.
fn handle_signal(signal: ShortcutSignal, shortcut: &str, toggle: bool, state: &mut ShortcutState) -> Option<Event> {
    match (signal, shortcut) {
        (ShortcutSignal::Pressed, PUSH_TO_TALK) => push_to_talk_pressed(toggle, &mut state.push_to_talk_down),
        (ShortcutSignal::Released, PUSH_TO_TALK) => push_to_talk_released(toggle, &mut state.push_to_talk_down),
        (ShortcutSignal::Pressed, DISMISS) if !state.esc_down => {
            state.esc_down = true;
            Some(Event::Dismiss)
        }
        (ShortcutSignal::Released, DISMISS) => {
            state.esc_down = false;
            None
        }
        (ShortcutSignal::Released, PASTE_LAST) => Some(Event::PasteLast),
        _ => None,
    }
}

/// Decides by the desktop, not by whether KGlobalAccel answers: D-Bus would
/// happily start kglobalacceld on other desktops too, and it would never see a key.
async fn is_kde(conn: &zbus::Connection) -> bool {
    match std::env::var("XDG_CURRENT_DESKTOP") {
        Ok(desktops) => desktops.split(':').any(|d| d.eq_ignore_ascii_case("KDE")),
        // Some launchers drop the variable; a running kglobalacceld means Plasma.
        Err(_) => match zbus::fdo::DBusProxy::new(conn).await {
            Ok(dbus) => dbus.name_has_owner("org.kde.kglobalaccel".try_into().unwrap()).await.unwrap_or(false),
            Err(_) => false,
        },
    }
}


/// `keys` (Qt key codes) as a D-Bus menu shortcut, like `[["Control", "space"]]`,
/// or empty if a key has no name there.
pub fn menu_shortcut(keys: &[i32]) -> Vec<Vec<String>> {
    let named: Option<Vec<Vec<String>>> = keys
        .iter()
        .map(|&key| {
            let mut tokens: Vec<String> = [(0x0400_0000, "Control"), (0x0800_0000, "Alt"), (0x0200_0000, "Shift"), (0x1000_0000, "Super")]
                .into_iter()
                .filter(|(bit, _)| key & bit != 0)
                .map(|(_, name)| name.to_owned())
                .collect();
            tokens.push(key_name(key & 0x01ff_ffff)?);
            Some(tokens)
        })
        .collect();
    named.unwrap_or_default()
}

/// `keys` as people read them, like "Ctrl+Space".
pub fn label(keys: &[i32]) -> Option<String> {
    let shortcut = menu_shortcut(keys);
    let presses: Vec<String> = shortcut
        .iter()
        .map(|press| {
            press
                .iter()
                .map(|t| match t.as_str() {
                    "Control" => "Ctrl".to_owned(),
                    "Super" => "Meta".to_owned(),
                    "space" => "Space".to_owned(),
                    other => other.to_owned(),
                })
                .collect::<Vec<_>>()
                .join("+")
        })
        .collect();
    (!presses.is_empty()).then(|| presses.join(", "))
}

/// The name the menu shortcut format gives a Qt key code, without modifiers.
fn key_name(key: i32) -> Option<String> {
    let name = match key {
        0x20 => "space".to_owned(),
        0x2b => "plus".to_owned(),
        0x2d => "minus".to_owned(),
        0x21..=0x7e => char::from(key as u8).to_ascii_uppercase().to_string(),
        0x0100_0030..=0x0100_0052 => format!("F{}", key - 0x0100_0030 + 1),
        _ => [
            (0x0100_0000, "Esc"),
            (0x0100_0001, "Tab"),
            (0x0100_0003, "Backspace"),
            (0x0100_0004, "Return"),
            (0x0100_0005, "Enter"),
            (0x0100_0006, "Ins"),
            (0x0100_0007, "Del"),
            (0x0100_0008, "Pause"),
            (0x0100_0009, "Print"),
            (0x0100_0010, "Home"),
            (0x0100_0011, "End"),
            (0x0100_0012, "Left"),
            (0x0100_0013, "Up"),
            (0x0100_0014, "Right"),
            (0x0100_0015, "Down"),
            (0x0100_0016, "PgUp"),
            (0x0100_0017, "PgDown"),
        ]
        .into_iter()
        .find(|&(code, _)| code == key)?
        .1
        .to_owned(),
    };
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_default_shortcuts() {
        assert_eq!(menu_shortcut(&[CTRL_SPACE]), [["Control", "space"]]);
        assert_eq!(menu_shortcut(&[CTRL_ALT_V]), [["Control", "Alt", "V"]]);
        assert_eq!(label(&[CTRL_SPACE]).as_deref(), Some("Ctrl+Space"));
        assert_eq!(label(&[CTRL_ALT_V]).as_deref(), Some("Ctrl+Alt+V"));
    }

    #[test]
    fn hold_mode_starts_on_press_and_stops_on_release() {
        let mut down = None;
        assert_eq!(push_to_talk_pressed(false, &mut down), Some(Event::Start));
        assert_eq!(push_to_talk_pressed(false, &mut down), None);
        assert_eq!(push_to_talk_released(false, &mut down), Some(Event::Stop));
        assert_eq!(push_to_talk_released(false, &mut down), None);
    }

    #[test]
    fn toggle_mode_toggles_once_per_press() {
        let mut down = None;
        assert_eq!(push_to_talk_pressed(true, &mut down), Some(Event::Toggle));
        // Key auto-repeat while the shortcut is held.
        assert_eq!(push_to_talk_pressed(true, &mut down), None);
        assert_eq!(push_to_talk_released(true, &mut down), None);
        assert_eq!(push_to_talk_pressed(true, &mut down), Some(Event::Toggle));
        assert_eq!(push_to_talk_released(true, &mut down), None);
    }

    #[test]
    fn changing_the_mode_mid_hold_finishes_the_old_gesture() {
        let mut down = None;
        assert_eq!(push_to_talk_pressed(false, &mut down), Some(Event::Start));
        // The setting changed to toggle while the key was still down.
        assert_eq!(push_to_talk_released(true, &mut down), Some(Event::Stop));
        assert_eq!(push_to_talk_pressed(true, &mut down), Some(Event::Toggle));
    }

    #[test]
    fn changing_the_mode_mid_toggle_finishes_the_old_gesture() {
        let mut down = None;
        assert_eq!(push_to_talk_pressed(true, &mut down), Some(Event::Toggle));
        // The setting changed to hold while the key was still down.
        assert_eq!(push_to_talk_released(false, &mut down), Some(Event::Stop));
        assert_eq!(push_to_talk_pressed(false, &mut down), Some(Event::Start));
    }

    #[test]
    fn quick_toggle_taps_each_start_or_stop_once() {
        let mut state = ShortcutState::default();
        assert_eq!(handle_signal(ShortcutSignal::Pressed, PUSH_TO_TALK, true, &mut state), Some(Event::Toggle));
        assert_eq!(handle_signal(ShortcutSignal::Released, PUSH_TO_TALK, true, &mut state), None);
        assert_eq!(handle_signal(ShortcutSignal::Pressed, PUSH_TO_TALK, true, &mut state), Some(Event::Toggle));
        assert_eq!(handle_signal(ShortcutSignal::Released, PUSH_TO_TALK, true, &mut state), None);
    }

    #[test]
    fn hold_quick_tap_starts_and_stops_in_order() {
        let mut state = ShortcutState::default();
        assert_eq!(handle_signal(ShortcutSignal::Pressed, PUSH_TO_TALK, false, &mut state), Some(Event::Start));
        assert_eq!(handle_signal(ShortcutSignal::Released, PUSH_TO_TALK, false, &mut state), Some(Event::Stop));
    }

    #[test]
    fn escape_and_paste_last_keep_their_release_semantics() {
        let mut state = ShortcutState::default();
        assert_eq!(handle_signal(ShortcutSignal::Pressed, DISMISS, false, &mut state), Some(Event::Dismiss));
        assert_eq!(handle_signal(ShortcutSignal::Pressed, DISMISS, false, &mut state), None);
        assert_eq!(handle_signal(ShortcutSignal::Released, DISMISS, false, &mut state), None);
        assert_eq!(handle_signal(ShortcutSignal::Pressed, DISMISS, false, &mut state), Some(Event::Dismiss));

        assert_eq!(handle_signal(ShortcutSignal::Pressed, PASTE_LAST, false, &mut state), None);
        assert_eq!(handle_signal(ShortcutSignal::Released, PASTE_LAST, false, &mut state), Some(Event::PasteLast));
    }

    #[test]
    fn unmatched_releases_are_harmless() {
        let mut state = ShortcutState::default();
        assert_eq!(handle_signal(ShortcutSignal::Released, PUSH_TO_TALK, false, &mut state), None);
        assert_eq!(handle_signal(ShortcutSignal::Released, PUSH_TO_TALK, true, &mut state), None);
    }

    #[test]
    fn names_function_keys_and_sequences() {
        let meta_f9 = 0x1000_0000 | 0x0100_0038;
        assert_eq!(label(&[meta_f9, 0x0400_0000 | 0x51]).as_deref(), Some("Meta+F9, Ctrl+Q"));
    }

    #[test]
    fn leaves_out_keys_it_cant_name() {
        assert!(menu_shortcut(&[0x0100_1234]).is_empty());
        assert_eq!(label(&[]), None);
    }
}
