//! The only place that knows about the transcription backend: the streaming
//! speech service behind the microphone button on gemini.google.com
//! (unofficial, fast, accurate). Audio streams up while you talk, so the text
//! is ready about a second after you stop, however long you spoke.
//!
//! The service speaks Google's WebChannel protocol: one POST opens a session,
//! a long-lived GET (the "back channel") streams replies down, and further
//! POSTs carry audio up, one at a time, each batching whatever was recorded
//! since the last. Messages are base64-encoded protobufs.
//!
//! A local voice-activity check runs on the finished recording, so speech-less
//! audio can't come back as invented text ("you", "Thank you.").

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use reqwest::header::{HeaderMap, HeaderValue};
use tokio::{
    io::AsyncWriteExt,
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender, error::TryRecvError},
    task::JoinHandle,
};
use whisper_rs::{WhisperVadContext, WhisperVadContextParams, WhisperVadParams};

const ENDPOINT: &str = "https://speechs3proto2-pa.googleapis.com/s3web/prod/streaming/channel";
/// The public browser key that gemini.google.com sends.
const API_KEY: &str = "AIzaSyD6n9asBjvx1yBHfhFhfw_kpS9Faq0BZHM";
const ORIGIN: &str = "https://gemini.google.com";
const LANGUAGE: &[u8] = b"en-US";
/// Each upload carries at most a few seconds of audio, so this only trips on a dead connection.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// The transcript normally follows the end of the audio within about a second.
const RESULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Extension field that wraps the service's replies.
const REPLY_FIELD: u64 = 1_253_625;
/// Ends the audio stream.
const END_OF_AUDIO: [u8; 2] = [0x18, 0x01];

/// Silero voice activity detection (~1 MB, a few ms per clip).
const VAD_FILE: &str = "ggml-silero-v5.1.2.bin";
const VAD_URL: &str = "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin";

pub struct Transcriber {
    client: reqwest::Client,
    vad: Arc<Mutex<WhisperVadContext>>,
}

/// A transcription in progress. Dropping it abandons the transcription.
pub struct Session {
    task: JoinHandle<Result<String>>,
    vad: Arc<Mutex<WhisperVadContext>>,
}

impl Transcriber {
    pub fn load(vad_model: &Path) -> Result<Self> {
        whisper_rs::install_logging_hooks(); // route whisper.cpp's chatter away from stdout
        let path = vad_model.to_str().context("VAD model path isn't UTF-8")?;
        let mut params = WhisperVadContextParams::new();
        params.set_n_threads(1);
        let vad = WhisperVadContext::new(path, params).context("loading VAD model")?;
        // The key is only accepted from the page it belongs to.
        let mut headers = HeaderMap::new();
        headers.insert("origin", HeaderValue::from_static(ORIGIN));
        headers.insert("referer", HeaderValue::from_static("https://gemini.google.com/"));
        let client = reqwest::Client::builder()
            .user_agent(concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")))
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self { client, vad: Arc::new(Mutex::new(vad)) })
    }

    /// Starts transcribing. Send 16 kHz mono s16le PCM down the returned
    /// sender as it's recorded; the audio ends when the sender is dropped.
    pub fn begin(&self) -> (Session, UnboundedSender<Vec<u8>>) {
        let (audio, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(stream(self.client.clone(), rx));
        (Session { task, vad: self.vad.clone() }, audio)
    }
}

impl Session {
    /// Waits for the transcript, once the audio has ended. `samples` is the
    /// whole recording, for the voice check; without speech, returns "".
    pub async fn finish(mut self, samples: &[i16]) -> Result<String> {
        if !has_speech(self.vad.clone(), samples).await? {
            return Ok(String::new());
        }
        (&mut self.task).await.context("transcription task failed")?
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn has_speech(vad: Arc<Mutex<WhisperVadContext>>, samples: &[i16]) -> Result<bool> {
    let mut audio = vec![0.0; samples.len()];
    whisper_rs::convert_integer_to_float_audio(samples, &mut audio)?;
    tokio::task::spawn_blocking(move || {
        let segments = vad.lock().unwrap().segments_from_samples(WhisperVadParams::new(), &audio)?;
        Ok(segments.count() > 0)
    })
    .await?
}

/// One WebChannel session.
struct Channel {
    client: reqwest::Client,
    gsessionid: String,
    sid: String,
    /// Request counter, required by the protocol.
    rid: AtomicU64,
    /// The last reply received, acknowledged with every request.
    aid: AtomicU64,
}

/// Runs a whole session: streams `audio` up and returns the transcript.
async fn stream(client: reqwest::Client, audio: UnboundedReceiver<Vec<u8>>) -> Result<String> {
    let res = client
        .post(ENDPOINT)
        .query(&[("VER", "8"), ("RID", "10000"), ("CVER", "22"), ("X-HTTP-Session-Id", "gsessionid"), ("t", "1")])
        .header("x-goog-api-key", API_KEY)
        .header("x-webchannel-content-type", "application/x-protobuf")
        .form(&[("count", "0")])
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .context("connecting to the speech service")?;
    let res = check(res).await?;
    let gsessionid = res.headers().get("x-http-session-id").and_then(|v| v.to_str().ok()).context("no session id in reply")?.to_string();
    let body = res.text().await?;
    // The reply is [[0, ["c", SID, ...]]].
    let mut frames = Frames::default();
    frames.push(body.as_bytes());
    let opened = frames.next()?.context("empty session reply")?;
    let sid = opened[0][1][1].as_str().with_context(|| format!("unexpected session reply: {opened}"))?.to_string();
    let channel = Channel { client, gsessionid, sid, rid: AtomicU64::new(10_001), aid: AtomicU64::new(0) };

    let replies = channel.receive();
    tokio::pin!(replies);
    tokio::select! {
        sent = channel.send_audio(audio) => sent?,
        // Replies end early only when the service gives up on the session.
        text = &mut replies => return text,
    }
    tokio::time::timeout(RESULT_TIMEOUT, replies).await.context("timed out waiting for the transcript")?
}

impl Channel {
    /// Uploads audio as it arrives, then the end-of-audio marker.
    async fn send_audio(&self, mut audio: UnboundedReceiver<Vec<u8>>) -> Result<()> {
        let mut ofs = 0;
        let mut messages = vec![config()];
        let mut ended = false;
        while !ended {
            // Wait for audio, then take everything recorded during the last upload too.
            let mut pcm = Vec::new();
            if messages.is_empty() {
                match audio.recv().await {
                    Some(chunk) => pcm = chunk,
                    None => ended = true,
                }
            }
            loop {
                match audio.try_recv() {
                    Ok(chunk) => pcm.extend(chunk),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        ended = true;
                        break;
                    }
                }
            }
            if !pcm.is_empty() {
                messages.push(field(293_101, &field(1, &pcm)));
            }
            if ended {
                messages.push(END_OF_AUDIO.to_vec());
            }
            self.post(ofs, &messages).await?;
            ofs += messages.len();
            messages.clear();
        }
        Ok(())
    }

    async fn post(&self, ofs: usize, messages: &[Vec<u8>]) -> Result<()> {
        let mut form = vec![("count".to_string(), messages.len().to_string()), ("ofs".to_string(), ofs.to_string())];
        for (i, message) in messages.iter().enumerate() {
            form.push((format!("req{i}___data__"), BASE64.encode(message)));
        }
        let rid = self.rid.fetch_add(1, Ordering::Relaxed).to_string();
        let aid = self.aid.load(Ordering::Relaxed).to_string();
        let res = self
            .client
            .post(ENDPOINT)
            .query(&[("VER", "8"), ("gsessionid", self.gsessionid.as_str()), ("SID", self.sid.as_str()), ("RID", rid.as_str()), ("AID", aid.as_str()), ("t", "1")])
            .form(&form)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .context("sending audio")?;
        check(res).await?;
        Ok(())
    }

    /// Reads replies until the service closes the session, and returns the transcript.
    async fn receive(&self) -> Result<String> {
        let mut text = Vec::new();
        // The service ends each back channel after about a minute; reopen it until the session closes.
        loop {
            let aid = self.aid.load(Ordering::Relaxed).to_string();
            let res = self
                .client
                .get(ENDPOINT)
                .query(&[("gsessionid", self.gsessionid.as_str()), ("VER", "8"), ("RID", "rpc"), ("SID", self.sid.as_str()), ("AID", aid.as_str()), ("CI", "0"), ("TYPE", "xmlhttp"), ("t", "1")])
                .send()
                .await
                .context("receiving the transcript")?;
            let mut res = check(res).await?;
            let mut frames = Frames::default();
            while let Some(chunk) = res.chunk().await.context("receiving the transcript")? {
                frames.push(&chunk);
                while let Some(frame) = frames.next()? {
                    // Each frame is a list of [id, [payload]].
                    for entry in frame.as_array().into_iter().flatten() {
                        if let Some(id) = entry[0].as_u64() {
                            self.aid.store(id, Ordering::Relaxed);
                        }
                        match entry[1][0].as_str() {
                            Some("close") => return Ok(text.join(" ")),
                            Some("noop") | None => {}
                            Some(payload) => text.extend(reply(&BASE64.decode(payload)?)?),
                        }
                    }
                }
            }
        }
    }
}

async fn check(res: reqwest::Response) -> Result<reqwest::Response> {
    let status = res.status();
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        bail!("HTTP {status}: {}", body.chars().take(200).collect::<String>());
    }
    Ok(res)
}

/// The session settings the Gemini web app sends, except that the audio is
/// raw PCM (encoding 0) rather than WebM/Opus (11).
fn config() -> Vec<u8> {
    // Sample rate, encoding, channels.
    let audio = [&[0x15][..], &16_000f32.to_le_bytes(), &[0x18, 0x00, 0x20, 0x01]].concat();
    [
        field(1, b"beyond-a2a-recognizer"),
        vec![0x10, 0x00],
        field(293_000, &field(2, &field(1, LANGUAGE))),
        field(293_100, &audio),
        field(294_000, &[field(2, b"bard-web-frontend"), field(8, b"Web")].concat()),
        field(294_500, &[field(1, &field(10, LANGUAGE)), vec![0x28, 0x01, 0xc0, 0x02, 0x01, 0xa0, 0x03, 0x01]].concat()),
    ]
    .concat()
}

/// The text in one reply from the service, if it carries any. Most replies
/// are progress reports; the transcript arrives in one piece at the end.
fn reply(msg: &[u8]) -> Result<Option<String>> {
    if int(msg, 1) == Some(2) {
        bail!("speech service error {}", int(msg, 2).unwrap_or_default());
    }
    let Some(result) = bytes(msg, REPLY_FIELD).next().and_then(|r| bytes(r, 1).next()) else {
        return Ok(None);
    };
    let mut text = String::new();
    for hypothesis in bytes(result, 3) {
        for words in bytes(hypothesis, 3).flat_map(|t| bytes(t, 1)) {
            text.push_str(std::str::from_utf8(words).context("transcript isn't UTF-8")?);
        }
    }
    Ok(Some(text.trim().to_string()).filter(|t| !t.is_empty()))
}

/// Splits a WebChannel response into its frames: `<length>\n<json>`, repeated.
#[derive(Default)]
struct Frames(Vec<u8>);

impl Frames {
    fn push(&mut self, data: &[u8]) {
        self.0.extend_from_slice(data);
    }

    fn next(&mut self) -> Result<Option<serde_json::Value>> {
        let Some(newline) = self.0.iter().position(|&b| b == b'\n') else { return Ok(None) };
        let len: usize = std::str::from_utf8(&self.0[..newline])?.trim().parse().context("bad frame length")?;
        let end = newline + 1 + len;
        if self.0.len() < end {
            return Ok(None);
        }
        let frame = serde_json::from_slice(&self.0[newline + 1..end]).context("bad frame")?;
        self.0.drain(..end);
        Ok(Some(frame))
    }
}

// Just enough protobuf for the handful of messages above.

/// A length-delimited protobuf field.
fn field(number: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    put_varint(&mut out, number << 3 | 2);
    put_varint(&mut out, payload.len() as u64);
    out.extend_from_slice(payload);
    out
}

fn put_varint(out: &mut Vec<u8>, mut n: u64) {
    while n >= 0x80 {
        out.push(n as u8 | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

enum Value<'a> {
    Int(u64),
    Bytes(&'a [u8]),
    Fixed,
}

/// The fields of a protobuf message, stopping at anything malformed.
fn fields(mut buf: &[u8]) -> impl Iterator<Item = (u64, Value<'_>)> {
    fn varint(buf: &mut &[u8]) -> Option<u64> {
        let mut n = 0;
        for shift in (0..64).step_by(7) {
            let (&b, rest) = buf.split_first()?;
            *buf = rest;
            n |= u64::from(b & 0x7f) << shift;
            if b < 0x80 {
                return Some(n);
            }
        }
        None
    }
    fn take<'a>(buf: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
        let (taken, rest) = buf.split_at_checked(n)?;
        *buf = rest;
        Some(taken)
    }
    std::iter::from_fn(move || {
        let key = varint(&mut buf)?;
        let value = match key & 7 {
            0 => Value::Int(varint(&mut buf)?),
            1 => take(&mut buf, 8).map(|_| Value::Fixed)?,
            2 => {
                let len = varint(&mut buf)?;
                Value::Bytes(take(&mut buf, usize::try_from(len).ok()?)?)
            }
            5 => take(&mut buf, 4).map(|_| Value::Fixed)?,
            _ => return None,
        };
        Some((key >> 3, value))
    })
}

fn int(msg: &[u8], number: u64) -> Option<u64> {
    fields(msg).find_map(|(n, v)| match v {
        Value::Int(i) if n == number => Some(i),
        _ => None,
    })
}

fn bytes(msg: &[u8], number: u64) -> impl Iterator<Item = &[u8]> {
    fields(msg).filter_map(move |(n, v)| match v {
        Value::Bytes(b) if n == number => Some(b),
        _ => None,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn config_is_the_browsers_but_with_pcm() {
        let mut browser = BASE64
            .decode("ChViZXlvbmQtYTJhLXJlY29nbml6ZXIQAMKIjwEJEgcKBWVuLVVT4o6PAQkVAAB6RhgLIAGCx48BGBIRYmFyZC13ZWItZnJvbnRlbmRCA1dlYqLmjwERCgdSBWVuLVVTKAHAAgGgAwE=")
            .unwrap();
        let encoding = browser.windows(2).position(|w| w == [0x18, 11]).unwrap();
        browser[encoding + 1] = 0;
        assert_eq!(config(), browser);
    }

    #[test]
    fn replies() {
        // Progress, the end of the session, and the result for silence, as the service sent them.
        assert_eq!(reply(&hex("2800ca8fe4040e120c080010c0a3860118c0a38601")).unwrap(), None);
        assert_eq!(reply(&hex("08012803")).unwrap(), None);
        assert_eq!(reply(&hex("2801ca8fe404180a1608011a08080010001a020a002a08080010001a020a00")).unwrap(), None);
        assert!(reply(&hex("080210022800320708022203525043")).is_err());

        let text = |s: &str| field(3, &[vec![0x08, 0x00], field(3, &field(1, s.as_bytes()))].concat());
        let result = [vec![0x08, 0x01], text("Hello there."), field(5, b"ignored")].concat();
        let msg = [vec![0x28, 0x02], field(REPLY_FIELD, &field(1, &result))].concat();
        assert_eq!(reply(&msg).unwrap().as_deref(), Some("Hello there."));
    }

    #[test]
    fn frames_split_across_chunks() {
        let mut frames = Frames::default();
        frames.push(b"14\n[[1,[\"noop\"]]]15\n[[2,");
        assert_eq!(frames.next().unwrap().unwrap()[0][1][0], "noop");
        assert!(frames.next().unwrap().is_none());
        frames.push(b"[\"close\"]]]");
        assert_eq!(frames.next().unwrap().unwrap()[0][1][0], "close");
        assert!(frames.next().unwrap().is_none());
    }
}
