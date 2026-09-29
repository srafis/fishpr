//! The only place that knows about the transcription backend: local Whisper
//! via whisper.cpp. The model is loaded once and kept in memory.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use tokio::io::AsyncWriteExt;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const MODEL_FILE: &str = "ggml-base.en.bin";
/// Segments Whisper itself thinks are probably not speech are dropped; on
/// silence it otherwise invents words like "you" (measured: speech ≈ 0.01,
/// hallucinated "you" on silence ≈ 0.94). 0.6 is openai/whisper's default.
const NO_SPEECH_THRESHOLD: f32 = 0.6;
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin";

pub struct Transcriber {
    ctx: Arc<WhisperContext>,
}

impl Transcriber {
    pub fn load(model: &Path) -> Result<Self> {
        whisper_rs::install_logging_hooks(); // route whisper.cpp's chatter away from stdout
        let ctx = WhisperContext::new_with_params(model, WhisperContextParameters::default())
            .with_context(|| format!("loading Whisper model {}", model.display()))?;
        Ok(Self { ctx: Arc::new(ctx) })
    }

    pub async fn transcribe(&self, wav: &Path) -> Result<String> {
        let samples = read_wav(wav)?;
        let ctx = self.ctx.clone();
        tokio::task::spawn_blocking(move || {
            let mut state = ctx.create_state()?;
            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            params.set_language(Some("en"));
            params.set_n_threads(std::thread::available_parallelism().map_or(4, |n| n.get()) as i32);
            params.set_no_context(true);
            params.set_suppress_blank(true);
            params.set_suppress_nst(true);
            params.set_print_special(false);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            state.full(params, &samples)?;
            let text: Vec<String> = state
                .as_iter()
                .filter(|s| s.no_speech_probability() < NO_SPEECH_THRESHOLD)
                .map(|s| s.to_string().trim().to_string())
                .filter(|s| !s.is_empty() && !is_non_speech_tag(s))
                .collect();
            Ok(text.join(" "))
        })
        .await?
    }
}

/// Whisper marks silence and noise with tags like `[BLANK_AUDIO]` or `(wind blowing)`.
fn is_non_speech_tag(segment: &str) -> bool {
    (segment.starts_with('[') && segment.ends_with(']')) || (segment.starts_with('(') && segment.ends_with(')'))
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

/// Path to the model, downloading it on first run. `FISHPR_MODEL` overrides it.
pub async fn ensure_model() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("FISHPR_MODEL") {
        return Ok(PathBuf::from(path));
    }
    let data_dir = crate::data_dir()?;
    let model = data_dir.join(MODEL_FILE);
    if model.exists() {
        return Ok(model);
    }

    tokio::fs::create_dir_all(&data_dir).await?;
    let part = model.with_extension("bin.part");
    let mut res = reqwest::get(MODEL_URL).await?.error_for_status().context("downloading Whisper model")?;
    let mut file = tokio::fs::File::create(&part).await?;
    while let Some(chunk) = res.chunk().await? {
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    // Rename only after a complete download so a crash never leaves a broken model.
    tokio::fs::rename(&part, &model).await?;
    Ok(model)
}
