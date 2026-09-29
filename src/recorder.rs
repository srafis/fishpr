//! Mic recording by driving `pw-record` (PipeWire) as a child process. Raw
//! 16 kHz mono s16le PCM is streamed out in ~100 ms chunks as it's captured,
//! and also kept whole for the speech check on release.

use std::process::Stdio;

use anyhow::{Context, Result, bail};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::mpsc::UnboundedSender,
    task::JoinHandle,
};

/// 100 ms of 16 kHz mono s16le.
const CHUNK_BYTES: usize = 3200;

pub struct Recording {
    child: Child,
    reader: JoinHandle<Vec<u8>>,
}

impl Recording {
    pub fn start(chunks: UnboundedSender<Vec<u8>>) -> Result<Self> {
        let mut child = Command::new("pw-record")
            .args(["--rate", "16000", "--channels", "1", "--format", "s16", "--raw", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("starting pw-record (is pipewire installed?)")?;
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let reader = tokio::spawn(async move {
            let mut all = Vec::new();
            let mut buf = vec![0; CHUNK_BYTES];
            while let Ok(n) = stdout.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                // The session may have failed already; keep recording anyway.
                let _ = chunks.send(buf[..n].to_vec());
                all.extend_from_slice(&buf[..n]);
            }
            all // dropping `chunks` here tells the session the audio is complete
        });
        Ok(Self { child, reader })
    }

    /// Stops recording and returns everything that was captured.
    pub async fn stop(mut self) -> Result<Vec<u8>> {
        if let Some(pid) = self.child.id() {
            // SIGINT lets pw-record flush and exit cleanly.
            unsafe { libc::kill(pid as i32, libc::SIGINT) };
        }
        let status = self.child.wait().await?;
        let pcm = self.reader.await?;
        if pcm.is_empty() {
            let mut err = String::new();
            if let Some(mut stderr) = self.child.stderr.take() {
                let _ = stderr.read_to_string(&mut err).await;
            }
            if !status.success() && !err.trim().is_empty() {
                bail!("pw-record failed: {}", err.trim());
            }
            bail!("recording was empty");
        }
        Ok(pcm)
    }
}
