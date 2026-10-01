//! Mic recording by driving `pw-record` (PipeWire) as a child process. The raw
//! audio streams through us, so the HUD can show the live input level and the
//! transcriber gets it while you're still talking.

use std::{process::Stdio, sync::OnceLock};

use anyhow::{Context, Result, bail};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::mpsc::UnboundedSender,
    task::JoinHandle,
};

/// Less than ~30 ms of audio means the mic never delivered anything.
const MIN_PCM_BYTES: usize = 1024;
/// Input levels mapped onto the meter: at or below the floor the bars sit flat
/// (a quiet room is around -75 dBFS), at the ceiling they're full height.
const FLOOR_DB: f32 = -60.0;
const CEIL_DB: f32 = -20.0;

pub struct Recording {
    child: Child,
    /// Collects the raw s16le PCM until pw-record exits.
    reader: JoinHandle<std::io::Result<Vec<u8>>>,
}

impl Recording {
    /// Starts recording 16 kHz mono s16le PCM, which goes to `audio` as it
    /// arrives; the sender is dropped when recording ends. `on_level` gets the
    /// input loudness, from 0.0 (silence or a muted mic) to 1.0 (loud speech),
    /// about every 20 ms.
    pub fn start(audio: UnboundedSender<Vec<u8>>, on_level: impl Fn(f32) + Send + 'static) -> Result<Self> {
        // stdbuf -o0: pw-record's stdout is otherwise block-buffered, which
        // delivers audio in 128 ms bursts and makes the meter stutter.
        let mut child = Command::new("stdbuf")
            .args(["-o0", "pw-record"])
            .args(raw_flag())
            .args(["--rate", "16000", "--channels", "1", "--format", "s16", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("starting pw-record (is pipewire installed?)")?;
        let mut stdout = child.stdout.take().context("pw-record has no stdout")?;
        let reader = tokio::spawn(async move {
            let mut pcm = Vec::new();
            let mut metered = 0;
            loop {
                pcm.reserve(4096);
                if stdout.read_buf(&mut pcm).await? == 0 {
                    return Ok(pcm);
                }
                // Reads can split a sample; pass on whole samples only.
                let end = pcm.len() & !1;
                on_level(level(&pcm[metered..end]));
                let _ = audio.send(pcm[metered..end].to_vec());
                metered = end;
            }
        });
        Ok(Self { child, reader })
    }

    /// Stops recording and returns the whole recording.
    pub async fn stop(self) -> Result<Vec<i16>> {
        if let Some(pid) = self.child.id() {
            // SIGINT lets pw-record flush its last samples; SIGKILL would not.
            unsafe { libc::kill(pid as i32, libc::SIGINT) };
        }
        let out = self.child.wait_with_output().await?;
        let pcm = self.reader.await?.context("reading audio from pw-record")?;
        if pcm.len() < MIN_PCM_BYTES {
            let err = String::from_utf8_lossy(&out.stderr);
            if !out.status.success() && !err.trim().is_empty() {
                bail!("pw-record failed: {}", err.trim());
            }
            bail!("recording was empty");
        }
        Ok(samples(&pcm).collect())
    }
}

/// Recent pw-record needs `--raw` to write bare PCM to stdout. Older ones
/// (PipeWire 1.0, in Ubuntu 24.04) already do that for `-` and reject the flag.
fn raw_flag() -> Option<&'static str> {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    let supported = *SUPPORTED.get_or_init(|| {
        std::process::Command::new("pw-record")
            .arg("--help")
            .output()
            .is_ok_and(|out| [out.stdout, out.stderr].iter().any(|text| String::from_utf8_lossy(text).contains("--raw")))
    });
    supported.then_some("--raw")
}

fn samples(pcm: &[u8]) -> impl Iterator<Item = i16> + '_ {
    pcm.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]))
}

/// RMS loudness of s16le `pcm` on the meter's 0.0–1.0 dB scale.
fn level(pcm: &[u8]) -> f32 {
    let n = pcm.len() / 2;
    if n == 0 {
        return 0.0;
    }
    let power = samples(pcm).map(|s| (s as f32 / 32768.0).powi(2)).sum::<f32>() / n as f32;
    // Digital silence gives -inf dB, which clamps to 0.
    let db = 10.0 * power.log10();
    ((db - FLOOR_DB) / (CEIL_DB - FLOOR_DB)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(amplitude: i16) -> Vec<u8> {
        // A square wave: RMS equals the amplitude.
        (0..320).flat_map(|i| if i % 2 == 0 { amplitude } else { -amplitude }.to_le_bytes()).collect()
    }

    #[test]
    fn silence_and_room_noise_read_as_zero() {
        assert_eq!(level(&pcm(0)), 0.0);
        assert_eq!(level(&[]), 0.0);
        assert_eq!(level(&pcm(5)), 0.0); // about -76 dBFS
    }

    #[test]
    fn level_rises_with_loudness() {
        let (quiet, normal, loud) = (level(&pcm(100)), level(&pcm(1000)), level(&pcm(3300)));
        assert!(0.0 < quiet && quiet < normal && normal < loud, "{quiet} {normal} {loud}");
        assert_eq!(level(&pcm(i16::MAX)), 1.0);
    }
}
