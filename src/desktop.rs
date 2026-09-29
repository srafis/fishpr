//! Clipboard and notifications via the standard desktop CLI tools, plus a
//! recording notice over D-Bus (notify-send can't close what it shows).

use std::{collections::HashMap, process::Stdio};

use anyhow::{Context, Result, bail};
use tokio::{io::AsyncWriteExt, process::Command, sync::OnceCell};
use zbus::zvariant::Value;

pub async fn copy_to_clipboard(text: &str) -> Result<()> {
    let mut cmd = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        Command::new("wl-copy")
    } else {
        let mut c = Command::new("xclip");
        c.args(["-selection", "clipboard"]);
        c
    };
    // wl-copy forks a daemon that keeps serving the clipboard; don't let it
    // hold our pipes open.
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("starting clipboard tool (wl-copy / xclip)")?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin.write_all(text.as_bytes()).await?;
    drop(stdin);
    let status = child.wait().await?;
    if !status.success() {
        bail!("clipboard tool exited with {status}");
    }
    Ok(())
}

pub fn notify(summary: &str, body: &str) {
    // tokio reaps the child in the background, so no zombies pile up.
    let _ = Command::new("notify-send")
        .args(["--app-name", "fishpr", "--icon", "audio-input-microphone", "--expire-time", "3000"])
        .arg(summary)
        .arg(body)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[zbus::proxy(interface = "org.freedesktop.Notifications", default_service = "org.freedesktop.Notifications", default_path = "/org/freedesktop/Notifications")]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(&self, app_name: &str, replaces_id: u32, app_icon: &str, summary: &str, body: &str, actions: &[&str], hints: HashMap<&str, Value<'_>>, expire_timeout: i32) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;
}

async fn notifications() -> zbus::Result<NotificationsProxy<'static>> {
    static CONN: OnceCell<zbus::Connection> = OnceCell::const_new();
    NotificationsProxy::new(CONN.get_or_try_init(zbus::Connection::session).await?).await
}

/// Shows a notification that stays until `close_notice`, and returns its ID.
/// Transient, so it doesn't pile up in the notification history.
pub async fn show_notice(summary: &str) -> Option<u32> {
    let hints = HashMap::from([("transient", Value::from(true)), ("urgency", Value::from(1u8))]);
    let result = async { notifications().await?.notify("fishpr", 0, "audio-input-microphone", summary, "", &[], hints, 0).await }.await;
    result.inspect_err(|e| eprintln!("fishpr: couldn't show notification: {e}")).ok()
}

pub async fn close_notice(id: u32) {
    if let Ok(proxy) = notifications().await {
        let _ = proxy.close_notification(id).await;
    }
}
