//! Clipboard and notifications via the standard desktop CLI tools.

use std::process::Stdio;

use anyhow::{Context, Result, bail};
use tokio::{io::AsyncWriteExt, process::Command};

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
