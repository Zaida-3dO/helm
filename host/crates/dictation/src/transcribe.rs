//! The call to the speech-to-text server: one multipart POST of a WAV, the text read back.
//!
//! The server speaks the OpenAI transcription route (`/v1/audio/transcriptions`), which Whisper
//! servers commonly offer. The request goes through no proxy, follows no redirect and has its
//! address checked again first, so a recording never travels further than the user allowed.
//! **Nothing here logs the audio or the text**, and the errors carry neither.

use crate::settings::{check_stt_url, Settings, AUTO_LANGUAGE};
use serde_json::Value;
use std::time::Duration;

/// The largest recording accepted: about five minutes of 16 kHz mono 16-bit sound.
pub const MAX_WAV_BYTES: usize = 10 * 1024 * 1024;
/// How long the server gets to answer.
pub const TIMEOUT: Duration = Duration::from_secs(60);
const MAX_REPLY: u64 = 1024 * 1024;
const ROUTE: &str = "/v1/audio/transcriptions";

/// Why a transcription did not happen. Each one has fixed words in [`Failure::say`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// No address is saved.
    NotSetUp,
    /// The saved address no longer passes the rule; the text says why.
    Address(String),
    /// The recording is over [`MAX_WAV_BYTES`].
    TooBig,
    /// The bytes are not a WAV file.
    NotWav,
    /// Nothing answered in time, or the connection failed.
    Unreachable,
    /// The server answered with this status.
    Server(u16),
    /// The server answered with something that holds no text.
    Unreadable,
}

impl Failure {
    /// The code the page keys on.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotSetUp => "not_set_up",
            Self::Address(_) => "bad_address",
            Self::TooBig => "too_big",
            Self::NotWav => "bad_audio",
            Self::Unreachable => "unreachable",
            Self::Server(_) => "server",
            Self::Unreadable => "unreadable",
        }
    }

    /// A sentence for the page, with no audio and no text in it.
    pub fn say(&self) -> String {
        match self {
            Self::NotSetUp => "Dictation isn't set up yet.".to_owned(),
            Self::Address(why) => why.clone(),
            Self::TooBig => {
                "That recording is too long. Dictation takes up to about five minutes.".to_owned()
            }
            Self::NotWav => "That recording could not be read.".to_owned(),
            Self::Unreachable => "Speech-to-text didn't answer.".to_owned(),
            Self::Server(status) => {
                format!("Speech-to-text answered with an error (HTTP {status}).")
            }
            Self::Unreadable => {
                "Speech-to-text answered with something Helm can't read.".to_owned()
            }
        }
    }
}

/// A quiet recording of `seconds` at 16 kHz, mono, 16-bit: a WAV header and zeros.
pub fn silent_wav(seconds: u32) -> Vec<u8> {
    let data = 16_000 * 2 * seconds;
    let mut w = Vec::with_capacity(44 + data as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&16_000u32.to_le_bytes()); // sample rate
    w.extend_from_slice(&32_000u32.to_le_bytes()); // bytes per second
    w.extend_from_slice(&2u16.to_le_bytes()); // block align
    w.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    w.resize(44 + data as usize, 0);
    w
}

/// Whether `wav` starts like a WAV file.
fn looks_like_wav(wav: &[u8]) -> bool {
    wav.len() > 44 && &wav[..4] == b"RIFF" && &wav[8..12] == b"WAVE"
}

/// The body of the multipart request, and the boundary it uses.
fn multipart(settings: &Settings, wav: &[u8]) -> (String, Vec<u8>) {
    let boundary = format!("helm-{}", uuid::Uuid::new_v4().simple());
    let mut body = Vec::with_capacity(wav.len() + 1024);
    let mut field = |name: &str, value: &str| {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    };
    field("model", &settings.model);
    if settings.language != AUTO_LANGUAGE {
        field("language", &settings.language);
    }
    field("response_format", "text");
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"turn.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(wav);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// The text in a reply: the plain text asked for, or `{"text": ...}` from a server that answers
/// in JSON anyway.
pub fn read_reply(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('"') {
        match serde_json::from_str::<Value>(trimmed) {
            Ok(Value::String(s)) => return s.trim().to_owned(),
            Ok(v) => {
                if let Some(t) = v.get("text").and_then(Value::as_str) {
                    return t.trim().to_owned();
                }
            }
            Err(_) => {}
        }
    }
    trimmed.to_owned()
}

/// Send `wav` to the saved server and return what was said (empty when nothing was). The
/// recording and the address are checked first; `before_send` then runs once, just before the
/// request (it is where the caller makes graphics memory room).
pub fn transcribe(
    settings: &Settings,
    wav: &[u8],
    before_send: impl FnOnce(),
) -> Result<String, Failure> {
    transcribe_with(settings, wav, TIMEOUT, before_send)
}

pub(crate) fn transcribe_with(
    settings: &Settings,
    wav: &[u8],
    timeout: Duration,
    before_send: impl FnOnce(),
) -> Result<String, Failure> {
    let Some(raw) = settings.stt_url.as_deref() else {
        return Err(Failure::NotSetUp);
    };
    let base = check_stt_url(raw, settings.allow_private_network).map_err(Failure::Address)?;
    if wav.len() > MAX_WAV_BYTES {
        return Err(Failure::TooBig);
    }
    if !looks_like_wav(wav) {
        return Err(Failure::NotWav);
    }
    let (content_type, body) = multipart(settings, wav);
    before_send();
    let agent = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .user_agent("helm-home")
        .build()
        .new_agent();
    let mut res = agent
        .post(format!("{base}{ROUTE}"))
        .header("Content-Type", &content_type)
        .header("Accept", "text/plain, application/json")
        .send(body)
        .map_err(|_| Failure::Unreachable)?;
    let status = res.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(Failure::Server(status));
    }
    let reply = res
        .body_mut()
        .with_config()
        .limit(MAX_REPLY)
        .read_to_string()
        .map_err(|e| match e {
            ureq::Error::Timeout(_) => Failure::Unreachable,
            _ => Failure::Unreadable,
        })?;
    Ok(read_reply(&reply))
}
