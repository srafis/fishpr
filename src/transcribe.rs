//! The only place that knows about the transcription backend. Swapping to the
//! official OpenAI API later means changing this file only.

use std::{path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use reqwest::multipart::{Form, Part};
use serde::Deserialize;

const ENDPOINT: &str = "https://chatgpt.com/backend-anon/transcribe";

#[derive(Deserialize)]
struct Response {
    text: String,
}

pub struct Transcriber {
    client: reqwest::Client,
}

impl Transcriber {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            // Cloudflare rejects requests without a User-Agent.
            .user_agent(concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(120))
            .build()?;
        Ok(Self { client })
    }

    pub async fn transcribe(&self, wav: &Path) -> Result<String> {
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
        if !status.is_success() {
            let snippet: String = body.chars().take(200).collect();
            bail!("HTTP {status}: {snippet}");
        }
        let parsed: Response = serde_json::from_str(&body)
            .with_context(|| format!("unexpected response: {}", body.chars().take(200).collect::<String>()))?;
        Ok(parsed.text.trim().to_string())
    }
}
