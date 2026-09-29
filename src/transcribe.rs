//! The only place that knows about the transcription backend: the streaming
//! speech service behind Gemini's web dictation (unofficial). Audio is
//! streamed while the user is still talking, so the final text arrives about
//! half a second after they stop.
//!
//! Protocol, reverse-engineered from gemini.google.com: a Google WebChannel
//! session. Handshake POST → long-poll GET for results → POSTs of protobuf
//! messages (config, audio chunks, end-of-audio), base64 in form fields.
//!
//! A local voice-activity check runs on release; recordings without speech
//! are abandoned before asking for a result, so they can't come back as
//! invented text.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use tokio::{
    io::AsyncWriteExt,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use whisper_rs::{WhisperVadContext, WhisperVadContextParams, WhisperVadParams};

const CHANNEL: &str = "https://speechs3proto2-pa.googleapis.com/s3web/prod/streaming/channel";
/// Where the API key comes from (Gemini web app's key, the `x-goog-api-key`
/// header on the first `streaming/channel` request). Kept out of the source.
const API_KEY_ENV: &str = "FISHPR_GEMINI_API_KEY";
const API_KEY_FILE: &str = "gemini-api-key"; // in ~/.config/fishpr/
const ORIGIN: &str = "https://gemini.google.com";
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

/// Recognizer config exactly as Gemini sends it. Decoded: recognizer
/// "beyond-a2a-recognizer", language en-US, 16000 Hz, encoding 11 (the
/// service sniffs the container, so WAV works), 1 channel, client
/// "bard-web-frontend".
const CONFIG: &str = "ChViZXlvbmQtYTJhLXJlY29nbml6ZXIQAMKIjwEJEgcKBWVuLVVT4o6PAQkVAAB6RhgLIAGCx48BGBIRYmFyZC13ZWItZnJvbnRlbmRCA1dlYqLmjwERCgdSBWVuLVVTKAHAAgGgAwE=";
/// `{3: 1}`: no more audio, send the final result.
const END_OF_AUDIO: &[u8] = &[0x18, 0x01];
/// Protobuf field wrapping each audio chunk, and the one results arrive in.
const AUDIO_FIELD: u64 = 293_101;
const RESULT_FIELD: u64 = 1_253_625;

/// How long to wait for the final result after end-of-audio.
const RESULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Silero voice activity detection (~1 MB, a few ms per clip).
const VAD_FILE: &str = "ggml-silero-v5.1.2.bin";
const VAD_URL: &str = "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin";

pub struct Transcriber {
    client: reqwest::Client,
    api_key: Arc<str>,
    vad: Arc<Mutex<WhisperVadContext>>,
}

fn api_key() -> Result<String> {
    if let Ok(key) = std::env::var(API_KEY_ENV) {
        return Ok(key.trim().to_string());
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .context("neither XDG_CONFIG_HOME nor HOME is set")?;
    let path = config.join("fishpr").join(API_KEY_FILE);
    let key = std::fs::read_to_string(&path)
        .with_context(|| format!("no Gemini API key: set {API_KEY_ENV} or put it in {}", path.display()))?;
    Ok(key.trim().to_string())
}

/// A live streaming session. Audio flows in from the recorder's channel;
/// `finish` decides whether to ask for the result.
pub struct Session {
    decide: oneshot::Sender<bool>,
    task: JoinHandle<Result<String>>,
}

impl Session {
    /// With `speech == false` the session is abandoned and returns "".
    pub async fn finish(self, speech: bool) -> Result<String> {
        let _ = self.decide.send(speech);
        self.task.await?
    }
}

impl Transcriber {
    pub fn load(vad_model: &Path) -> Result<Self> {
        whisper_rs::install_logging_hooks(); // route whisper.cpp's chatter away from stdout
        let path = vad_model.to_str().context("VAD model path isn't UTF-8")?;
        let mut params = WhisperVadContextParams::new();
        params.set_n_threads(1);
        let vad = WhisperVadContext::new(path, params).context("loading VAD model")?;
        let client = reqwest::Client::builder().user_agent(USER_AGENT).build()?;
        Ok(Self { client, api_key: api_key()?.into(), vad: Arc::new(Mutex::new(vad)) })
    }

    /// Starts streaming `audio` (16 kHz mono s16le PCM chunks) right away.
    pub fn begin(&self, audio: mpsc::UnboundedReceiver<Vec<u8>>) -> Session {
        let (decide, decided) = oneshot::channel();
        let task = tokio::spawn(stream(self.client.clone(), self.api_key.clone(), audio, decided));
        Session { decide, task }
    }

    /// Whether the recording (16 kHz mono s16le PCM) contains any speech.
    pub async fn has_speech(&self, pcm: Vec<u8>) -> Result<bool> {
        let samples: Vec<f32> = pcm.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
        let vad = self.vad.clone();
        tokio::task::spawn_blocking(move || {
            let segments = vad.lock().unwrap().segments_from_samples(WhisperVadParams::new(), &samples)?;
            Ok(segments.count() > 0)
        })
        .await?
    }
}

async fn stream(
    client: reqwest::Client,
    api_key: Arc<str>,
    mut audio: mpsc::UnboundedReceiver<Vec<u8>>,
    decided: oneshot::Receiver<bool>,
) -> Result<String> {
    let mut channel = Channel::open(client, &api_key).await?;
    let mut results = channel.listen();

    channel.send(&[B64.decode(CONFIG)?]).await?;
    // The service needs a container; a WAV header with "unknown length"
    // sizes lets us stream raw PCM behind it.
    channel.send(&[audio_message(&streaming_wav_header())]).await?;
    while let Some(first) = audio.recv().await {
        // The server accepts audio at about real-time pace, so chunks that
        // queue up while a POST is in flight go out together in the next one.
        let mut batch = vec![audio_message(&first)];
        while let Ok(more) = audio.try_recv() {
            batch.push(audio_message(&more));
        }
        channel.send(&batch).await?;
    }

    if !decided.await.unwrap_or(false) {
        return Ok(String::new());
    }
    channel.send(&[END_OF_AUDIO.to_vec()]).await?;

    let mut text = Vec::new();
    let deadline = tokio::time::sleep(RESULT_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            reply = results.recv() => match reply {
                Some(Reply::Final(t)) => text.push(t),
                Some(Reply::Done) | None => break,
            },
            _ = &mut deadline => {
                if text.is_empty() {
                    bail!("no result from the speech service within {}s", RESULT_TIMEOUT.as_secs());
                }
                break;
            }
        }
    }
    Ok(text.join(" ").trim().to_string())
}

enum Reply {
    Final(String),
    Done,
}

/// One Google WebChannel (BrowserChannel v8) session.
struct Channel {
    client: reqwest::Client,
    sid: String,
    gsessionid: String,
    rid: u64,
    ofs: usize,
}

impl Channel {
    async fn open(client: reqwest::Client, api_key: &str) -> Result<Self> {
        let rid = u64::from(uuid::Uuid::new_v4().as_u128() as u16) + 10_000;
        let res = client
            .post(CHANNEL)
            .query(&[("VER", "8"), ("RID", &rid.to_string()), ("CVER", "22"), ("X-HTTP-Session-Id", "gsessionid"), ("zx", &zx()), ("t", "1")])
            .header("Origin", ORIGIN)
            .header("Referer", format!("{ORIGIN}/"))
            .header("x-goog-api-key", api_key)
            .header("X-WebChannel-Content-Type", "application/x-protobuf")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body("count=0")
            .send()
            .await
            .context("connecting to the speech service")?;
        let status = res.status();
        let gsessionid = res.headers().get("X-HTTP-Session-Id").and_then(|v| v.to_str().ok()).map(str::to_owned);
        let body = res.text().await?;
        if !status.is_success() {
            bail!("speech service handshake failed: HTTP {status}: {}", body.chars().take(200).collect::<String>());
        }
        // Body: `51\n[[0,["c","<SID>","",8,15,30000]]]\n`
        let sid = body.split("\"c\",\"").nth(1).and_then(|s| s.split('"').next()).map(str::to_owned);
        let (Some(sid), Some(gsessionid)) = (sid, gsessionid) else {
            bail!("unexpected handshake response: {}", body.chars().take(200).collect::<String>());
        };
        Ok(Self { client, sid, gsessionid, rid, ofs: 0 })
    }

    /// Opens the long-poll GET that delivers results, parsed in the background.
    fn listen(&self) -> mpsc::UnboundedReceiver<Reply> {
        let (tx, rx) = mpsc::unbounded_channel();
        let req = self
            .client
            .get(CHANNEL)
            .query(&[
                ("gsessionid", self.gsessionid.as_str()),
                ("VER", "8"),
                ("RID", "rpc"),
                ("SID", &self.sid),
                ("AID", "0"),
                ("CI", "0"),
                ("TYPE", "xmlhttp"),
                ("zx", &zx()),
                ("t", "1"),
            ])
            .header("Origin", ORIGIN)
            .header("Referer", format!("{ORIGIN}/"));
        tokio::spawn(async move {
            let Ok(mut res) = req.send().await else { return };
            let mut buf = Vec::new();
            while let Ok(Some(chunk)) = res.chunk().await {
                buf.extend_from_slice(&chunk);
                for frame in take_frames(&mut buf) {
                    for reply in parse_frame(&frame) {
                        let done = matches!(reply, Reply::Done);
                        if tx.send(reply).is_err() || done {
                            return;
                        }
                    }
                }
            }
        });
        rx
    }

    async fn send(&mut self, messages: &[Vec<u8>]) -> Result<()> {
        self.rid += 1;
        let mut form = vec![("count".to_string(), messages.len().to_string()), ("ofs".to_string(), self.ofs.to_string())];
        for (i, m) in messages.iter().enumerate() {
            form.push((format!("req{i}___data__"), B64.encode(m)));
        }
        self.ofs += messages.len();
        let res = self
            .client
            .post(CHANNEL)
            .query(&[
                ("VER", "8"),
                ("gsessionid", self.gsessionid.as_str()),
                ("SID", &self.sid),
                ("RID", &self.rid.to_string()),
                ("AID", "0"),
                ("zx", &zx()),
                ("t", "1"),
            ])
            .header("Origin", ORIGIN)
            .header("Referer", format!("{ORIGIN}/"))
            .form(&form)
            .send()
            .await
            .context("streaming audio")?;
        if !res.status().is_success() {
            bail!("speech service rejected audio: HTTP {}", res.status());
        }
        Ok(())
    }
}

/// Cache-busting token the web client adds to every request.
fn zx() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

/// Splits complete `<length>\n<json>` frames off the front of `buf`.
fn take_frames(buf: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    loop {
        // Some frames end with a newline that isn't counted in their length.
        let skip = buf.iter().take_while(|b| b.is_ascii_whitespace()).count();
        buf.drain(..skip);
        let Some(nl) = buf.iter().position(|&b| b == b'\n') else { break };
        let Some(len) = std::str::from_utf8(&buf[..nl]).ok().and_then(|s| s.trim().parse::<usize>().ok()) else {
            buf.clear(); // out of sync; drop and wait for the next frame
            break;
        };
        if buf.len() < nl + 1 + len {
            break;
        }
        frames.push(buf[nl + 1..nl + 1 + len].to_vec());
        buf.drain(..nl + 1 + len);
    }
    frames
}

/// A frame is `[[id, [payload]], ...]`; payloads are "noop", "close", or a
/// base64 protobuf message.
fn parse_frame(frame: &[u8]) -> Vec<Reply> {
    let Ok(entries) = serde_json::from_slice::<Vec<(u64, Vec<serde_json::Value>)>>(frame) else { return vec![] };
    let mut replies = Vec::new();
    for (_, payload) in entries {
        match payload.first().and_then(|v| v.as_str()) {
            Some("close") => replies.push(Reply::Done),
            Some("noop") | None => {}
            Some(b64) => {
                if let Ok(msg) = B64.decode(b64) {
                    replies.extend(parse_reply(&msg));
                }
            }
        }
    }
    replies
}

/// Final results live at `1253625.1` with `.1 == 1` (final) and the text at
/// `.3.3.1`. The last message of a session is `{1: 1, 5: n}`.
fn parse_reply(msg: &[u8]) -> Option<Reply> {
    let top = proto::fields(msg)?;
    for (field, value) in &top {
        if let (RESULT_FIELD, proto::Value::Bytes(body)) = (*field, value) {
            for (f, v) in proto::fields(body)? {
                let (1, proto::Value::Bytes(result)) = (f, v) else { continue };
                let result = proto::fields(result)?;
                if proto::varint(&result, 1) != Some(1) {
                    continue; // not final
                }
                let hyp = proto::fields(proto::bytes(&result, 3)?)?;
                let text = proto::fields(proto::bytes(&hyp, 3)?)?;
                return Some(Reply::Final(String::from_utf8_lossy(proto::bytes(&text, 1)?).into_owned()));
            }
        }
    }
    (proto::varint(&top, 1) == Some(1)).then_some(Reply::Done)
}

fn audio_message(chunk: &[u8]) -> Vec<u8> {
    let mut inner = Vec::with_capacity(chunk.len() + 4);
    proto::put_bytes(&mut inner, 1, chunk);
    let mut outer = Vec::with_capacity(inner.len() + 8);
    proto::put_bytes(&mut outer, AUDIO_FIELD, &inner);
    outer
}

fn streaming_wav_header() -> Vec<u8> {
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&u32::MAX.to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    h.extend_from_slice(&1u16.to_le_bytes()); // PCM
    h.extend_from_slice(&1u16.to_le_bytes()); // mono
    h.extend_from_slice(&16_000u32.to_le_bytes()); // sample rate
    h.extend_from_slice(&32_000u32.to_le_bytes()); // byte rate
    h.extend_from_slice(&2u16.to_le_bytes()); // block align
    h.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    h.extend_from_slice(b"data");
    h.extend_from_slice(&u32::MAX.to_le_bytes());
    h
}

/// Just enough protobuf wire format for the messages above.
mod proto {
    pub enum Value<'a> {
        Varint(u64),
        Bytes(&'a [u8]),
    }

    pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    pub fn put_bytes(out: &mut Vec<u8>, field: u64, data: &[u8]) {
        put_varint(out, field << 3 | 2);
        put_varint(out, data.len() as u64);
        out.extend_from_slice(data);
    }

    fn get_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *buf.get(*pos)?;
            *pos += 1;
            v |= u64::from(b & 0x7f) << shift;
            if b < 0x80 {
                return Some(v);
            }
        }
        None
    }

    /// Top-level fields of a message, or None if it isn't valid protobuf.
    pub fn fields(buf: &[u8]) -> Option<Vec<(u64, Value<'_>)>> {
        let mut out = Vec::new();
        let mut pos = 0;
        while pos < buf.len() {
            let key = get_varint(buf, &mut pos)?;
            let value = match key & 7 {
                0 => Value::Varint(get_varint(buf, &mut pos)?),
                1 => {
                    pos += 8;
                    continue;
                }
                2 => {
                    let len = get_varint(buf, &mut pos)? as usize;
                    let data = buf.get(pos..pos.checked_add(len)?)?;
                    pos += len;
                    Value::Bytes(data)
                }
                5 => {
                    pos += 4;
                    continue;
                }
                _ => return None,
            };
            out.push((key >> 3, value));
        }
        (pos == buf.len()).then_some(out)
    }

    pub fn varint(fields: &[(u64, Value)], field: u64) -> Option<u64> {
        fields.iter().find_map(|(f, v)| match v {
            Value::Varint(x) if *f == field => Some(*x),
            _ => None,
        })
    }

    pub fn bytes<'a>(fields: &[(u64, Value<'a>)], field: u64) -> Option<&'a [u8]> {
        fields.iter().find_map(|(f, v)| match v {
            Value::Bytes(b) if *f == field => Some(*b),
            _ => None,
        })
    }
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

    // Frames captured from a live session transcribing the JFK sample.
    const FRAMES: &str = concat!(
        "14\n[[1,[\"noop\"]]]\n",
        "38\n[[2,[\"KADKj+QEDBIKCAAQwO0aGMDtGg==\"]]]\n",
        "38\n[[3,[\"KAHKj+QEDhIMCAMQwLGfBRjAsZ8F\"]]]\n",
        "350\n[[4,[\"KALKj+QE9wEK9AEIARp3CAAQwLGfBRpuCmxBbmQgc28sIG15IGZlbGxvdyBBbWVyaWNhbnMsIGFzayBub3Qgd2hhdCB5b3VyIGNvdW50cnkgY2FuIGRvIGZvciB5b3UsIGFzayB3aGF0IHlvdSBjYW4gZG8gZm9yIHlvdXIgY291bnRyeS4qdwgAEMCxnwUabgpsQW5kIHNvLCBteSBmZWxsb3cgQW1lcmljYW5zLCBhc2sgbm90IHdoYXQgeW91ciBjb3VudHJ5IGNhbiBkbyBmb3IgeW91LCBhc2sgd2hhdCB5b3UgY2FuIGRvIGZvciB5b3VyIGNvdW50cnku\"]]]",
        "18\n[[5,[\"CAEoAw==\"]]]",
        "15\n[[6,[\"close\"]]]",
    );

    fn replies(stream: &[u8], split_at: usize) -> Vec<String> {
        // Feed in two pieces to exercise frames split across network chunks.
        let mut buf = Vec::new();
        let mut out = Vec::new();
        for piece in [&stream[..split_at], &stream[split_at..]] {
            buf.extend_from_slice(piece);
            for frame in take_frames(&mut buf) {
                for r in parse_frame(&frame) {
                    out.push(match r {
                        Reply::Final(t) => t,
                        Reply::Done => "<done>".into(),
                    });
                }
            }
        }
        out
    }

    #[test]
    fn parses_final_result_and_end() {
        let stream = FRAMES.as_bytes();
        for split in [0, 1, 60, 200, stream.len() - 3, stream.len()] {
            assert_eq!(
                replies(stream, split),
                [
                    "And so, my fellow Americans, ask not what your country can do for you, ask what you can do for your country.",
                    "<done>",
                    "<done>",
                ],
                "split at {split}"
            );
        }
    }

    #[test]
    fn audio_message_round_trips() {
        let msg = audio_message(b"hello");
        let top = proto::fields(&msg).unwrap();
        let inner = proto::fields(proto::bytes(&top, AUDIO_FIELD).unwrap()).unwrap();
        assert_eq!(proto::bytes(&inner, 1).unwrap(), b"hello");
    }
}
