//! Speech-to-text: an OpenAI-compatible transcription server (Whisper on the PC), used by the
//! Telegram bot for voice notes and by the voice line for the phone's recordings.
//!
//! [`Stt`] sends the recording as a hand-built `multipart/form-data` body (`file`, `model`,
//! `language`, `response_format=text`; no auth) from the blocking pool, with a short connect timeout
//! and the configured time to answer, and reads at most 1 MB back. Anything but a transcript comes
//! back as [`SttError`]: a reason in plain words plus its [`SttFailure`] kind, so the caller can
//! tell passing trouble — which [`retry_transcribing`] tries once more — from a real failure.
//! [`spawn_health`] probes the server's own `/health` now and then, so a failure can say since when
//! the server has been down.

use crate::telegram::cut;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A voice step's result; the error is a reason in plain words for the user.
pub type VoiceFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// A transcription's result; the error carries its kind as well as its plain words.
pub type SttFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SttError>> + Send + 'a>>;

/// How long a connection may take to open.
pub const STT_CONNECT: Duration = Duration::from_secs(5);
/// How long the speech-to-text server may take to answer by default. It splits long recordings
/// into pieces and can take minutes on an hour of audio.
pub const STT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// A failed call whose failure looks passing is tried once more after this.
pub const STT_RETRY_AFTER: Duration = Duration::from_secs(3);
/// How often the speech server's own `/health` is probed.
pub const HEALTH_PROBE: Duration = Duration::from_secs(60);
/// How long one health probe may take.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
/// A failing health probe is confirmed with one more after this before it opens an outage: the
/// probe path (hub to PC) drops a single request now and then while the server is fine, and one
/// missed probe must not raise an alarm.
pub const HEALTH_RETRY_AFTER: Duration = Duration::from_secs(10);

/// What kind of failure the speech-to-text server gave, so the caller can tell one that is worth
/// trying again from one that is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SttFailure {
    /// The connection would not open (or the name would not resolve) in time.
    ConnectTimeout,
    /// Nothing answered on that address.
    Refused,
    /// The server answered, but with that HTTP status.
    Status(u16),
    /// The connection was open but no answer came in time.
    ReadTimeout,
    /// Anything else.
    Other,
}

impl SttFailure {
    /// Whether trying once more right away has a real chance: a server that is restarting or
    /// briefly refusing answers the second time; a model that is not loaded or a call that simply
    /// ran out of time does not.
    pub fn is_transient(self) -> bool {
        matches!(
            self,
            SttFailure::ConnectTimeout
                | SttFailure::Refused
                | SttFailure::Status(502)
                | SttFailure::Status(503)
        )
    }
}

/// A transcription failure: the reason in plain words, with its kind attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SttError {
    pub kind: SttFailure,
    pub why: String,
}

impl fmt::Display for SttError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.why.fmt(f)
    }
}

impl SttError {
    /// A failure whose kind is not known.
    pub fn plain(why: impl Into<String>) -> Self {
        SttError {
            kind: SttFailure::Other,
            why: why.into(),
        }
    }
}

/// The speech-to-text server's health: when it last answered `/health`, and when it last stopped
/// answering. The failed side matters too: the hub keeps the recording notes that failed while the
/// server was down, and tells the person's PC about a standing outage.
#[derive(Debug, Default)]
pub struct SttHealth {
    /// `0`: never seen answering.
    last_ok_ms: AtomicI64,
    /// `0`: not currently seen down. The first failure of a run of them.
    down_since_ms: AtomicI64,
    /// Why the current run of failures looks the way it does, in plain words.
    down_detail: std::sync::Mutex<Option<String>>,
}

impl SttHealth {
    pub fn note_ok(&self, at_ms: i64) {
        self.last_ok_ms.store(at_ms.max(1), Ordering::Relaxed);
        self.down_since_ms.store(0, Ordering::Relaxed);
        if let Ok(mut d) = self.down_detail.lock() {
            *d = None;
        }
    }

    /// A health probe failed: the first failure of a run opens the outage at `at_ms`.
    pub fn note_down(&self, at_ms: i64, detail: &str) {
        self.down_since_ms
            .compare_exchange(0, at_ms.max(1), Ordering::Relaxed, Ordering::Relaxed)
            .ok();
        if let Ok(mut d) = self.down_detail.lock() {
            *d = Some(detail.to_owned());
        }
    }

    pub fn last_ok_ms(&self) -> Option<i64> {
        match self.last_ok_ms.load(Ordering::Relaxed) {
            0 => None,
            ms => Some(ms),
        }
    }

    pub fn is_down(&self) -> bool {
        self.down_since_ms.load(Ordering::Relaxed) != 0
    }

    /// When the current outage began, if one is running.
    pub fn down_since_ms(&self) -> Option<i64> {
        match self.down_since_ms.load(Ordering::Relaxed) {
            0 => None,
            ms => Some(ms),
        }
    }

    pub fn down_detail(&self) -> Option<String> {
        self.down_detail.lock().ok().and_then(|d| d.clone())
    }

    /// "unreachable since HH:MM" when the last good health answer is older than one probe; the
    /// server is reachable, or not heard of, when it is `None`. The clock is the person's wall
    /// clock (the container's `TZ`), the same one searches use.
    pub fn since_text(&self, now_ms: i64) -> Option<String> {
        let ok = self.last_ok_ms()?;
        if now_ms - ok < HEALTH_PROBE.as_millis() as i64 {
            return None;
        }
        Some(format!("unreachable since {}", clock_text(ok)))
    }
}

/// An instant as the person's wall clock, "HH:MM" (the container's `TZ`; only UTC and the house
/// zone are known, anything else reads as UTC).
pub fn clock_text(ms: i64) -> String {
    let off = crate::rank::utc_offset_minutes(&std::env::var("TZ").unwrap_or_default(), ms);
    let t = ms + i64::from(off) * 60_000;
    let (hh, mm) = ((t / 3_600_000).rem_euclid(24), (t / 60_000).rem_euclid(60));
    format!("{hh:02}:{mm:02}")
}

/// What a bridge does when the speech server's health says something changed: retry what it held
/// during the outage, and finish off what has waited too long to be worth holding.
pub trait SttHealed: Send + Sync {
    /// The server answers `/health` again (or the hour is up): retry the notes still under the
    /// hour and end the ones past it.
    fn stt_back(&self, at: i64);
    /// End what has been held past its hour. The watch calls it on every look.
    fn stt_expire(&self, _at: i64) {}
    /// The server looks fine but a note may still be held (a transcription failed between two
    /// probes): try again the ones that have waited a while since their last try.
    fn stt_retry_stale(&self, _at: i64) {}
}

/// Whether an HTTP answer from the health address means the server is up. A server with no
/// `/health` route answers 404 (or 405 to a GET): it was reached, which is all that is left to
/// ask of it, so it counts as up.
fn health_answer_ok(status: u16) -> bool {
    status < 400 || matches!(status, 404 | 405)
}

/// The server's own health address: the scheme and host of the transcriptions endpoint, with
/// `/health` on it.
pub fn health_url(stt_url: &str) -> Option<String> {
    let scheme = stt_url.find("://")?;
    let host = stt_url[scheme + 3..]
        .split('/')
        .next()
        .filter(|h| !h.is_empty())?;
    Some(format!("{}://{}/health", &stt_url[..scheme], host))
}

/// Probe the speech server's `/health` every [`HEALTH_PROBE`] and keep the last answer in
/// `health`. Call it from inside the tokio runtime; the bridges hold the same handle.
pub fn spawn_health(stt_url: &str, health: Arc<SttHealth>) {
    let Some(url) = health_url(stt_url) else {
        eprintln!("stt: the speech server's health address is not known from {stt_url}");
        return;
    };
    let (h, agent) = (health, agent_with(HEALTH_TIMEOUT));
    tokio::spawn(async move {
        let mut was_ok = true;
        loop {
            let probe = || {
                let agent = agent.clone();
                let url = url.clone();
                blocking_typed(
                    move || match agent.get(&url).call() {
                        Ok(r) => Ok(r.status().as_u16()),
                        Err(e) => Err(stt_failed(e, 1)),
                    },
                    SttError::plain,
                )
            };
            let (answer, missed_once) = probe_health(probe, HEALTH_RETRY_AFTER).await;
            let ok = matches!(answer, Ok(s) if health_answer_ok(s));
            if ok {
                h.note_ok(crate::telegram::now_ms());
                if !was_ok {
                    eprintln!("stt: the speech server answers /health again");
                } else if missed_once {
                    eprintln!("stt: one /health probe missed but the confirming one answered");
                }
            } else {
                let detail = match answer {
                    Ok(s) => format!("its health address answered HTTP {s}"),
                    Err(e) => match e.kind {
                        SttFailure::ReadTimeout => {
                            "its health address does not answer in time".into()
                        }
                        _ => "its health address does not answer".to_owned(),
                    },
                };
                h.note_down(crate::telegram::now_ms(), &detail);
                if was_ok {
                    eprintln!("stt: the speech server's /health did not answer");
                }
            }
            was_ok = ok;
            tokio::time::sleep(HEALTH_PROBE).await;
        }
    });
}

/// One health probe run, confirmed if it fails: a second probe goes out after `retry_after` and
/// only the second answer counts as the server's state. A lone miss on the probe path is a blip,
/// not an outage. `Ok` answer, and whether the first probe missed (the blip, worth a log line).
pub(crate) async fn probe_health<F, Fut>(mut probe: F, retry_after: Duration) -> (Fut::Output, bool)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<u16, SttError>>,
{
    let first = probe().await;
    if matches!(first, Ok(s) if health_answer_ok(s)) {
        return (first, false);
    }
    tokio::time::sleep(retry_after).await;
    (probe().await, true)
}

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
    ) -> SttFuture<'a, String>;
    /// Where the calls go, for the log line. `""` when there is no real server.
    fn endpoint(&self) -> &str {
        ""
    }
}

/// One transcription, tried once more after `retry_after` (normally [`STT_RETRY_AFTER`]) when the
/// first try failed the way a restarting or briefly busy server fails. Each attempt is logged with
/// its address, kind and how long it took.
pub async fn retry_transcribing<Fut>(
    endpoint: &str,
    retry_after: Duration,
    mut attempt: impl FnMut() -> Fut,
) -> Result<String, SttError>
where
    Fut: Future<Output = Result<String, SttError>>,
{
    let first = log_attempt(endpoint, 1, &mut attempt).await;
    if let Err(e) = &first {
        if e.kind.is_transient() {
            tokio::time::sleep(retry_after).await;
            return log_attempt(endpoint, 2, &mut attempt).await;
        }
    }
    first
}

async fn log_attempt<Fut>(
    endpoint: &str,
    n: u8,
    attempt: &mut impl FnMut() -> Fut,
) -> Result<String, SttError>
where
    Fut: Future<Output = Result<String, SttError>>,
{
    let started = Instant::now();
    let r = attempt().await;
    let took = started.elapsed().as_millis();
    // The moment the attempt ended, as a wall clock; the container logs are otherwise bare.
    let ms = crate::telegram::now_ms();
    let at = format!(
        "{:02}:{:02}:{:02}",
        ms / 3_600_000 % 24,
        ms / 60_000 % 60,
        ms / 1000 % 60
    );
    // The address may carry a key in its query or a login before the host: log the host only.
    let endpoint = log_host(endpoint);
    match &r {
        Ok(_) => eprintln!("stt {at}: {endpoint} answered in {took} ms (try {n})"),
        Err(e) => eprintln!(
            "stt {at}: {endpoint} failed in {took} ms (try {n}): {:?} — {}",
            e.kind, e.why
        ),
    }
    r
}

/// `host:port` of an address, with no scheme, login, path or query: what is safe to log.
fn log_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
        .to_owned()
}

/// The speech-to-text server over HTTP.
pub struct Stt {
    cfg: SttConfig,
    /// With the server's own, longer timeout.
    agent: ureq::Agent,
    /// Sent on every request, e.g. `X-Priority: interactive` so a spoken word is taken ahead of
    /// queued bulk work at a server that understands it.
    headers: Vec<(&'static str, String)>,
}

impl Stt {
    pub fn new(cfg: &SttConfig) -> Self {
        Stt {
            cfg: cfg.clone(),
            agent: agent_with(cfg.timeout),
            headers: Vec::new(),
        }
    }

    /// A copy that sends these extra headers on every request.
    pub fn with_headers(mut self, headers: &[(&'static str, &str)]) -> Self {
        self.headers = headers.iter().map(|(k, v)| (*k, (*v).to_owned())).collect();
        self
    }

    pub fn url(&self) -> &str {
        &self.cfg.url
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

/// [`blocking`] for a typed error: a join failure becomes one too.
async fn blocking_typed<T: Send + 'static, E: Send + 'static>(
    f: impl FnOnce() -> Result<T, E> + Send + 'static,
    on_join: impl FnOnce(String) -> E + Send + 'static,
) -> Result<T, E> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(on_join(format!("the call did not finish: {e}"))))
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

/// Why the speech-to-text call went wrong, with its kind: out of time is said as such, and a
/// refused or unreachable address gets the kind that is worth trying again.
fn stt_failed(e: ureq::Error, minutes: u64) -> SttError {
    let unit = if minutes == 1 { "minute" } else { "minutes" };
    match e {
        ureq::Error::Timeout(ureq::Timeout::Connect | ureq::Timeout::Resolve) => SttError {
            kind: SttFailure::ConnectTimeout,
            why: format!("the speech-to-text server could not be reached: {e}"),
        },
        ureq::Error::Timeout(_) => SttError {
            kind: SttFailure::ReadTimeout,
            why: format!("the speech-to-text server did not finish within {minutes} {unit}"),
        },
        ureq::Error::Io(ref io) if io.kind() == std::io::ErrorKind::ConnectionRefused => SttError {
            kind: SttFailure::Refused,
            why: format!("the speech-to-text server could not be reached: {e}"),
        },
        ureq::Error::ConnectionFailed => SttError {
            kind: SttFailure::Refused,
            why: format!("the speech-to-text server could not be reached: {e}"),
        },
        e => SttError {
            kind: SttFailure::Other,
            why: format!("the speech-to-text server could not be reached: {e}"),
        },
    }
}

/// The speech-to-text server's answer as a transcript, or why it is not one. The transcript is
/// passed on as the server wrote it, with any markers it puts in (a gap it skipped, a cut).
fn stt_reply(status: u16, body: &str) -> Result<String, SttError> {
    let body = body.trim();
    let server = "the speech-to-text server";
    if status != 200 {
        return Err(SttError {
            kind: SttFailure::Status(status),
            why: match body {
                "" => format!("{server} answered HTTP {status}"),
                b => format!("{server} answered HTTP {status}: {}", cut(b, 300)),
            },
        });
    }
    if body.starts_with('{') {
        if body.contains("\"error\"") {
            return Err(SttError::plain(format!(
                "{server} reported an error: {}",
                cut(body, 300)
            )));
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
        "" => Err(SttError::plain(format!("{server} returned no text"))),
        b => Ok(b.to_owned()),
    }
}

impl Transcribe for Stt {
    fn transcribe<'a>(
        &'a self,
        audio: Vec<u8>,
        name: &'a str,
        mime: &'a str,
    ) -> SttFuture<'a, String> {
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
        let (agent, url, headers) = (
            self.agent.clone(),
            self.cfg.url.clone(),
            self.headers.clone(),
        );
        let minutes = self.cfg.timeout.as_secs().div_ceil(60);
        Box::pin(blocking_typed(
            move || {
                let mut req = agent.post(&url).header(
                    "Content-Type",
                    format!("multipart/form-data; boundary={boundary}"),
                );
                for (k, v) in &headers {
                    req = req.header(*k, v.as_str());
                }
                let mut resp = req.send(&body[..]).map_err(|e| stt_failed(e, minutes))?;
                let status = resp.status().as_u16();
                let text = resp
                    .body_mut()
                    .with_config()
                    .limit(1024 * 1024)
                    .lossy_utf8(true)
                    .read_to_string()
                    .map_err(|e| match e {
                        e @ ureq::Error::Timeout(_) => stt_failed(e, minutes),
                        e => SttError::plain(format!(
                            "the speech-to-text server's answer was unreadable: {e}"
                        )),
                    })?;
                stt_reply(status, &text)
            },
            SttError::plain,
        ))
    }

    fn endpoint(&self) -> &str {
        &self.cfg.url
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Counting {
        tries: AtomicUsize,
        fail: Option<SttFailure>,
    }

    impl Transcribe for Counting {
        fn transcribe<'a>(
            &'a self,
            _audio: Vec<u8>,
            _name: &'a str,
            _mime: &'a str,
        ) -> SttFuture<'a, String> {
            let n = self.tries.fetch_add(1, Ordering::SeqCst);
            match self.fail {
                Some(kind) => Box::pin(async move {
                    Err(SttError {
                        kind,
                        why: format!("try {}", n + 1),
                    })
                }),
                None => Box::pin(async move { Ok("words".to_owned()) }),
            }
        }
    }

    #[tokio::test]
    async fn passing_trouble_is_retried_once_other_failures_are_not() {
        // (kind of the first failure, attempts a caller should see)
        for (kind, tries) in [
            (SttFailure::ConnectTimeout, 2),
            (SttFailure::Refused, 2),
            (SttFailure::Status(502), 2),
            (SttFailure::Status(503), 2),
            (SttFailure::Status(500), 1),
            (SttFailure::ReadTimeout, 1),
            (SttFailure::Other, 1),
        ] {
            let c = Counting {
                tries: AtomicUsize::new(0),
                fail: Some(kind),
            };
            let r = retry_transcribing(
                "http://speech/v1/audio/transcriptions",
                Duration::from_millis(1),
                || c.transcribe(vec![1], "a.ogg", "audio/ogg"),
            )
            .await;
            assert_eq!(r.unwrap_err().why, format!("try {tries}"), "{kind:?}");
            assert_eq!(
                c.tries.load(Ordering::SeqCst),
                tries,
                "{kind:?} retried wrong"
            );
        }
        // A first try that answers never costs a second one.
        let c = Counting {
            tries: AtomicUsize::new(0),
            fail: None,
        };
        let r = retry_transcribing("http://speech", Duration::from_millis(1), || {
            c.transcribe(vec![1], "a.ogg", "audio/ogg")
        })
        .await;
        assert_eq!(r.as_deref(), Ok("words"));
        assert_eq!(c.tries.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn ureq_failures_get_the_kind_that_fits() {
        let minutes = 10;
        let refused = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "nope");
        let cases = [
            (
                ureq::Error::Timeout(ureq::Timeout::Connect),
                SttFailure::ConnectTimeout,
            ),
            (
                ureq::Error::Timeout(ureq::Timeout::Resolve),
                SttFailure::ConnectTimeout,
            ),
            (
                ureq::Error::Timeout(ureq::Timeout::RecvResponse),
                SttFailure::ReadTimeout,
            ),
            (
                ureq::Error::Timeout(ureq::Timeout::Global),
                SttFailure::ReadTimeout,
            ),
            (ureq::Error::Io(refused), SttFailure::Refused),
            (ureq::Error::ConnectionFailed, SttFailure::Refused),
            (ureq::Error::HostNotFound, SttFailure::Other),
        ];
        for (e, kind) in cases {
            let e = stt_failed(e, minutes);
            assert_eq!(e.kind, kind);
            assert!(e.why.starts_with("the speech-to-text server"));
        }
        // The plain words for the two time-outs stay the ones the user reads.
        let t = stt_failed(ureq::Error::Timeout(ureq::Timeout::RecvResponse), 10);
        assert_eq!(
            t.why,
            "the speech-to-text server did not finish within 10 minutes"
        );
    }

    #[tokio::test]
    async fn a_lone_failed_probe_is_confirmed_before_it_counts() {
        // First probe misses, the confirming one answers: a blip on the probe path, not an outage.
        let n = AtomicUsize::new(0);
        let (answer, missed_once) = probe_health(
            || {
                let ok = n.fetch_add(1, Ordering::SeqCst) > 0;
                async move {
                    if ok {
                        Ok(200)
                    } else {
                        Err(SttError {
                            kind: SttFailure::ReadTimeout,
                            why: "slow".into(),
                        })
                    }
                }
            },
            Duration::from_millis(1),
        )
        .await;
        assert_eq!(answer, Ok(200), "the confirming probe decides");
        assert!(missed_once, "the blip is worth a log line");
        // Both miss: the outage stands, and it carries the second failure.
        let (answer, missed_once) = probe_health(
            || async { Err(SttError::plain("still nothing")) },
            Duration::from_millis(1),
        )
        .await;
        assert!(missed_once);
        assert_eq!(answer.unwrap_err().why, "still nothing");
        // A first probe that answers costs no confirming one.
        let n2 = AtomicUsize::new(0);
        let (answer, missed_once) = probe_health(
            || {
                n2.fetch_add(1, Ordering::SeqCst);
                async { Ok(404) }
            },
            Duration::from_millis(1),
        )
        .await;
        assert_eq!(answer, Ok(404));
        assert!(!missed_once);
        assert_eq!(n2.load(Ordering::SeqCst), 1, "a good probe is not repeated");
    }

    #[test]
    fn a_running_outage_keeps_its_start_and_its_reason() {
        let h = SttHealth::default();
        assert!(!h.is_down(), "an unprobed server is not a known outage");
        let base = 1_763_000_000_000;
        h.note_down(base, "its health address does not answer");
        h.note_down(base + 60_000, "its health address answered HTTP 503");
        assert!(h.is_down());
        // The outage keeps the moment it opened; the reason keeps the newest answer.
        assert_eq!(h.down_since_ms(), Some(base));
        assert_eq!(
            h.down_detail().as_deref(),
            Some("its health address answered HTTP 503")
        );
        h.note_ok(base + 120_000);
        assert!(!h.is_down());
        assert_eq!(h.down_since_ms(), None);
        assert_eq!(h.down_detail(), None);
    }

    #[test]
    fn an_error_answer_carries_its_status_as_kind() {
        let e = stt_reply(503, "model not loaded").unwrap_err();
        assert_eq!(e.kind, SttFailure::Status(503));
        assert_eq!(
            e.why,
            "the speech-to-text server answered HTTP 503: model not loaded"
        );
        assert!(stt_reply(200, " hello ").unwrap() == "hello");
    }

    #[test]
    fn the_health_address_is_the_speech_servers_own() {
        assert_eq!(
            health_url("http://192.168.1.9:1079/v1/audio/transcriptions").as_deref(),
            Some("http://192.168.1.9:1079/health")
        );
        assert_eq!(
            health_url("https://speech.example/v1/audio/transcriptions").as_deref(),
            Some("https://speech.example/health")
        );
        assert_eq!(health_url("not a url"), None);
        // A server with no health route is reached, so it is up; a refusal or an error is not.
        assert!(
            health_answer_ok(200) && health_answer_ok(404) && health_answer_ok(405),
            "an answer, even \"no such route\", shows the server is there"
        );
        assert!(!health_answer_ok(500) && !health_answer_ok(503) && !health_answer_ok(401));
        // What is logged of an address never carries a login, path or query (it may hold a key).
        for (url, host) in [
            (
                "http://u:k3y@192.168.1.9:1079/v1/audio?key=abc",
                "192.168.1.9:1079",
            ),
            (
                "https://speech.example/v1/audio/transcriptions",
                "speech.example",
            ),
            ("", ""),
        ] {
            assert_eq!(log_host(url), host, "{url}");
        }
    }

    #[test]
    fn a_long_standing_outage_names_its_start() {
        let h = SttHealth::default();
        // 15:40 UTC on a fixed day; a probe that passed 40 minutes later.
        let base = 1_763_000_000_000;
        assert_eq!(
            h.since_text(base),
            None,
            "never seen answering says nothing"
        );
        h.note_ok(base);
        assert_eq!(
            h.since_text(base + 30_000),
            None,
            "a fresh outage is not yet a standing one"
        );
        let text = h.since_text(base + 300_000).expect("five minutes on");
        assert!(
            text.starts_with("unreachable since ")
                && text[18..].chars().all(|c| c.is_ascii_digit() || c == ':'),
            "{text}"
        );
    }
}
