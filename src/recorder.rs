//! Mic recording by driving `pw-record` (PipeWire) as a child process.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context, Result, bail};
use tokio::process::{Child, Command};

/// A 16-bit header-only WAV is 44 bytes; anything this small has no audio.
const MIN_WAV_BYTES: u64 = 1024;

pub struct Recording {
    child: Child,
    path: PathBuf,
}

impl Recording {
    pub fn start(path: &Path) -> Result<Self> {
        let _ = std::fs::remove_file(path);
        let child = Command::new("pw-record")
            .args(["--rate", "16000", "--channels", "1", "--format", "s16"])
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("starting pw-record (is pipewire installed?)")?;
        Ok(Self { child, path: path.to_path_buf() })
    }

    /// Stops recording and returns the path of the finished WAV file.
    pub async fn stop(self) -> Result<PathBuf> {
        if let Some(pid) = self.child.id() {
            // SIGINT lets pw-record finalize the WAV header; SIGKILL would not.
            unsafe { libc::kill(pid as i32, libc::SIGINT) };
        }
        let out = self.child.wait_with_output().await?;
        let size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if size < MIN_WAV_BYTES {
            let err = String::from_utf8_lossy(&out.stderr);
            if !out.status.success() && !err.trim().is_empty() {
                bail!("pw-record failed: {}", err.trim());
            }
            bail!("recording was empty");
        }
        Ok(self.path)
    }
}
