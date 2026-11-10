//! Speech-to-text: an OpenAI-compatible transcription server (Whisper on the PC), used by the
//! Telegram bot for voice notes and by the voice line for the phone's recordings.
//!
//! [`Stt`] sends the recording as a hand-built `multipart/form-data` body (`file`, `model`,
//! `language`, `response_format=text`; no auth) from the blocking pool, with a short connect timeout
//! and the configured time to answer, and reads at most 1 MB back. Anything but a transcript comes
//! back as a reason in plain words.

use crate::telegram::cut;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// A voice step's result; the error is a reason in plain words for the user.
pub type VoiceFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// How long a connection may take to open.
pub const STT_CONNECT: Duration = Duration::from_secs(5);
/// How long the speech-to-text server may take to answer by default. It splits long recordings
/// into pieces and can take minutes on an hour of audio.
pub const STT_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// The OpenAI-compatible speech-to-text server recordings go to.
#[derive(Debug, Clone, PartialEq)]
pub struct SttConfig {
    /// The transcriptions endpoint, e.g. `http://<host>:<port>/v1/audio/transcriptions`.
    pub url: String,
    pub model: String,
    pub language: String,
    /// How long one transcription may take.
    pub timeout: Duration,
}

/// Turning a recording into text. The real one is [`Stt`]; tests use a fake.
pub trait Transcribe: Send + Sync {
    /// The transcript of `audio`, uploaded as the file `name` of type `mime`.
    fn transcribe<'a>(
        &'a self,
        audio: Vec<u8>,
        name: &'a str,
        mime: &'a str,
    ) -> VoiceFuture<'a, String>;
}

/// The speech-to-text server over HTTP.
pub struct Stt {
    cfg: SttConfig,
    /// With the server's own, longer timeout.
    agent: ureq::Agent,
}

impl Stt {
    pub fn new(cfg: &SttConfig) -> Self {
        Stt {
            cfg: cfg.clone(),
            agent: agent_with(cfg.timeout),
        }
    }
}

/// An HTTP agent that answers an error status as a response and gives up after `timeout`.
pub(crate) fn agent_with(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(STT_CONNECT))
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into()
}

/// Run a blocking HTTP exchange on the blocking pool.
pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(format!("the call did not finish: {e}")))
}

/// A `multipart/form-data` body: the file part, then the plain fields.
fn multipart(boundary: &str, file: (&str, &str, &[u8]), fields: &[(&str, &str)]) -> Vec<u8> {
    let (name, mime, bytes) = file;
    let name = name.replace(['"', '\\', '\r', '\n'], "_");
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\
         Content-Type: {mime}\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(bytes);
    for (k, v) in fields {
        body.extend_from_slice(
            format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}")
                .as_bytes(),
        );
    }
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// Why the speech-to-text server gave no answer, in plain words: out of time is said as such.
fn stt_failed(e: ureq::Error, minutes: u64) -> String {
    let unit = if minutes == 1 { "minute" } else { "minutes" };
    match e {
        ureq::Error::Timeout(t)
            if !matches!(t, ureq::Timeout::Connect | ureq::Timeout::Resolve) =>
        {
            format!("the speech-to-text server did not finish within {minutes} {unit}")
        }
        e => format!("the speech-to-text server could not be reached: {e}"),
    }
}

/// The speech-to-text server's answer as a transcript, or why it is not one. The transcript is
/// passed on as the server wrote it, with any markers it puts in (a gap it skipped, a cut).
fn stt_reply(status: u16, body: &str) -> Result<String, String> {
    let body = body.trim();
    let server = "the speech-to-text server";
    if status != 200 {
        return Err(match body {
            "" => format!("{server} answered HTTP {status}"),
            b => format!("{server} answered HTTP {status}: {}", cut(b, 300)),
        });
    }
    if body.starts_with('{') {
        if body.contains("\"error\"") {
            return Err(format!("{server} reported an error: {}", cut(body, 300)));
        }
        // A server that ignored `response_format=text` answers JSON with the text in it.
        if let Some(t) = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v.get("text")?.as_str().map(str::to_owned))
        {
            return Ok(t.trim().to_owned());
        }
    }
    match body {
        "" => Err(format!("{server} returned no text")),
        b => Ok(b.to_owned()),
    }
}

impl Transcribe for Stt {
    fn transcribe<'a>(
        &'a self,
        audio: Vec<u8>,
        name: &'a str,
        mime: &'a str,
    ) -> VoiceFuture<'a, String> {
        let boundary = format!("helm-hub-{}", uuid::Uuid::new_v4().simple());
        let body = multipart(
            &boundary,
            (name, mime, audio.as_slice()),
            &[
                ("model", self.cfg.model.as_str()),
                ("language", self.cfg.language.as_str()),
                ("response_format", "text"),
            ],
        );
        let (agent, url) = (self.agent.clone(), self.cfg.url.clone());
        let minutes = self.cfg.timeout.as_secs().div_ceil(60);
        Box::pin(blocking(move || {
            let mut resp = agent
                .post(&url)
                .header(
                    "Content-Type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .send(&body[..])
                .map_err(|e| stt_failed(e, minutes))?;
            let status = resp.status().as_u16();
            let text = resp
                .body_mut()
                .with_config()
                .limit(1024 * 1024)
                .lossy_utf8(true)
                .read_to_string()
                .map_err(|e| match e {
                    e @ ureq::Error::Timeout(_) => stt_failed(e, minutes),
                    e => format!("the speech-to-text server's answer was unreadable: {e}"),
                })?;
            stt_reply(status, &text)
        }))
    }
}
