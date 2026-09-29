//! The only place that knows about the transcription backend: ChatGPT's
//! anonymous dictation endpoint (unofficial, fast, accurate). Swapping to the
//! official OpenAI API later means changing this file only.
//!
//! A local voice-activity check runs first, so silent or accidental recordings
//! never leave the machine, and speech-less audio can't come back as invented
//! text ("you", "Thank you.").

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use reqwest::multipart::{Form, Part};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use whisper_rs::{WhisperVadContext, WhisperVadContextParams, WhisperVadParams};

const ENDPOINT: &str = "https://chatgpt.com/backend-anon/transcribe";

/// Silero voice activity detection (~1 MB, a few ms per clip).
const VAD_FILE: &str = "ggml-silero-v5.1.2.bin";
const VAD_URL: &str = "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin";

#[derive(Deserialize)]
struct Response {
    text: String,
}

pub struct Transcriber {
    client: reqwest::Client,
    vad: Arc<Mutex<WhisperVadContext>>,
}

impl Transcriber {
    pub fn load(vad_model: &Path) -> Result<Self> {
        whisper_rs::install_logging_hooks(); // route whisper.cpp's chatter away from stdout
        let path = vad_model.to_str().context("VAD model path isn't UTF-8")?;
        let mut params = WhisperVadContextParams::new();
        params.set_n_threads(1);
        let vad = WhisperVadContext::new(path, params).context("loading VAD model")?;
        let client = reqwest::Client::builder()
            // Cloudflare rejects requests without a User-Agent.
            .user_agent(concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(120))
            .build()?;
        Ok(Self { client, vad: Arc::new(Mutex::new(vad)) })
    }

    pub async fn transcribe(&self, wav: &Path) -> Result<String> {
        if !self.has_speech(wav).await? {
            return Ok(String::new());
        }

        let bytes = tokio::fs::read(wav).await.context("reading recording")?;
        let file = Part::bytes(bytes).file_name("audio.wav").mime_str("audio/wav")?;
        let form = Form::new()
            .part("file", file)
            .text("duration_ms", 1_000_000_000.to_string());
        let res = self
            .client
            .post(ENDPOINT)
            .header("oai-device-id", uuid::Uuid::new_v4().to_string())
            .multipart(form)
            .send()
            .await
            .context("sending audio")?;

        let status = res.status();
        let body = res.text().await.context("reading response")?;
        let snippet = || body.chars().take(200).collect::<String>();
        if !status.is_success() {
            bail!("HTTP {status}: {}", snippet());
        }
        let parsed: Response = serde_json::from_str(&body).with_context(|| format!("unexpected response: {}", snippet()))?;
        Ok(parsed.text.trim().to_string())
    }

    async fn has_speech(&self, wav: &Path) -> Result<bool> {
        let samples = read_wav(wav)?;
        let vad = self.vad.clone();
        tokio::task::spawn_blocking(move || {
            let segments = vad.lock().unwrap().segments_from_samples(WhisperVadParams::new(), &samples)?;
            Ok(segments.count() > 0)
        })
        .await?
    }
}

/// Reads the 16 kHz mono s16 WAV that `pw-record` writes into f32 samples.
fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let reader = hound::WavReader::open(path).context("opening recording")?;
    let spec = reader.spec();
    if spec.sample_rate != 16_000 || spec.channels != 1 {
        bail!("expected 16 kHz mono audio, got {} Hz / {} ch", spec.sample_rate, spec.channels);
    }
    let pcm: Vec<i16> = reader.into_samples::<i16>().collect::<Result<_, _>>()?;
    let mut samples = vec![0.0; pcm.len()];
    whisper_rs::convert_integer_to_float_audio(&pcm, &mut samples)?;
    Ok(samples)
}

/// Path to the VAD model, downloading it on first run.
pub async fn ensure_vad_model() -> Result<PathBuf> {
    let data_dir = crate::data_dir()?;
    let path = data_dir.join(VAD_FILE);
    if path.exists() {
        return Ok(path);
    }
    tokio::fs::create_dir_all(&data_dir).await?;
    let part = path.with_extension("part");
    let mut res = reqwest::get(VAD_URL).await?.error_for_status().context("downloading VAD model")?;
    let mut file = tokio::fs::File::create(&part).await?;
    while let Some(chunk) = res.chunk().await? {
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    // Rename only after a complete download so a crash never leaves a broken model.
    tokio::fs::rename(&part, &path).await?;
    Ok(path)
}
