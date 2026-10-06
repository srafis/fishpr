//! Every transcription, kept with its recording in `~/.local/share/fishpr/history`:
//! `<id>.txt` holds the text and `<id>.wav` the audio. The ID is the local time
//! it was made, like `2026-10-06_14-03-22`, so the files sort by time and read
//! well in a file manager. The window lists them.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

/// The recorder's format: 16 kHz, mono, 16-bit.
const SAMPLE_RATE: u32 = 16_000;

pub struct Entry {
    pub id: String,
    pub text: String,
}

impl Entry {
    /// `2026-10-06`.
    pub fn date(&self) -> &str {
        &self.id[..10]
    }

    /// `14:03`.
    pub fn time(&self) -> String {
        self.id[11..16].replace('-', ":")
    }

    pub fn audio(&self) -> Result<PathBuf> {
        Ok(dir()?.join(format!("{}.wav", self.id)))
    }
}

pub fn dir() -> Result<PathBuf> {
    Ok(crate::data_dir()?.join("history"))
}

/// Keeps `text` and the recording it came from.
pub fn save(text: &str, samples: &[i16]) -> Result<()> {
    let dir = dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let now = now_local();
    // Two in the same second get a suffix.
    let id = (1..)
        .map(|n| if n == 1 { now.clone() } else { format!("{now}-{n}") })
        .find(|id| !dir.join(format!("{id}.txt")).exists())
        .expect("some suffix is free");
    // The audio first: an entry is listed once its text exists.
    write_wav(&dir.join(format!("{id}.wav")), samples)?;
    let tmp = dir.join(format!(".{id}.txt"));
    fs::write(&tmp, text)?;
    fs::rename(&tmp, dir.join(format!("{id}.txt")))?;
    Ok(())
}

/// Every entry, newest first.
pub fn list() -> Result<Vec<Entry>> {
    let dir = dir()?;
    let mut entries = Vec::new();
    let Ok(files) = fs::read_dir(&dir) else { return Ok(entries) };
    for file in files.flatten() {
        let name = file.file_name();
        let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".txt")) else { continue };
        if !is_id(id) {
            continue;
        }
        if let Ok(text) = fs::read_to_string(file.path()) {
            entries.push(Entry { id: id.to_owned(), text });
        }
    }
    entries.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(entries)
}

pub fn delete(id: &str) -> Result<()> {
    anyhow::ensure!(is_id(id), "not a history entry: {id}");
    let dir = dir()?;
    fs::remove_file(dir.join(format!("{id}.txt")))?;
    let _ = fs::remove_file(dir.join(format!("{id}.wav")));
    Ok(())
}

/// `2026-10-06_14-03-22`, maybe with a `-2` after it.
fn is_id(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() >= 19
        && b.iter().take(19).enumerate().all(|(i, &c)| match i {
            4 | 7 | 13 | 16 => c == b'-',
            10 => c == b'_',
            _ => c.is_ascii_digit(),
        })
        && id[19..].strip_prefix('-').map_or(id.len() == 19, |n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
}

/// The local time as an entry ID.
fn now_local() -> String {
    let t = local_time(unsafe { libc::time(std::ptr::null_mut()) });
    format!("{:04}-{:02}-{:02}_{:02}-{:02}-{:02}", t.tm_year + 1900, t.tm_mon + 1, t.tm_mday, t.tm_hour, t.tm_min, t.tm_sec)
}

fn local_time(at: libc::time_t) -> libc::tm {
    let mut tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&at, &mut tm) };
    tm
}

fn write_wav(path: &Path, samples: &[i16]) -> Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // bytes per second
    out.extend_from_slice(&2u16.to_le_bytes()); // bytes per frame
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    fs::File::create(path)?.write_all(&out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_ids() {
        assert!(is_id("2026-10-06_14-03-22"));
        assert!(is_id("2026-10-06_14-03-22-2"));
        assert!(!is_id("2026-10-06_14-03-22-"));
        assert!(!is_id("2026-10-06 14-03-22"));
        assert!(!is_id(".2026-10-06_14-03-22"));
        assert!(!is_id("../etc/passwd"));
    }

    #[test]
    fn reads_date_and_time_from_the_id() {
        let entry = Entry { id: "2026-10-06_14-03-22-2".into(), text: String::new() };
        assert_eq!(entry.date(), "2026-10-06");
        assert_eq!(entry.time(), "14:03");
    }
}
