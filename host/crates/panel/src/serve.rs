//! Ranged serving of previewed files: PDFs, audio, video and fonts.
//!
//! The viewer never loads such a file whole. [`MediaServer::open`] checks a path (the same gate,
//! link and private-folder checks as every other read) and hands out a random, short-lived token;
//! the renderer then loads `<scheme>/<token>` as a media, frame or font source and the app's
//! custom-scheme handler answers each request through [`MediaServer::respond`], reading only the
//! requested byte range from disk.
//!
//! Safety rules, all enforced here:
//! - Only `GET` and `HEAD`; the path never comes from the request, only from a token this server
//!   issued, and at most [`MAX_SESSIONS`] tokens are live (the oldest is dropped first).
//! - Every request re-runs the path checks, opens the file without following links and compares its
//!   size and modified time with what the token was issued for, so a swapped file is refused.
//! - A single byte range per request, at most [`CHUNK`] bytes; a request with no range gets the
//!   whole file only when it is at most [`WHOLE_LIMIT`] (fonts: [`FONT_LIMIT`]), else its first chunk.
//! - Responses carry the content type from the extension, `nosniff`, no caching, and (except for
//!   PDFs, whose built-in viewer refuses a sandboxed response) a `sandbox` content-security policy,
//!   so nothing a file holds can run as a page of its own.

use crate::private::PrivateRoots;
use crate::viewer::{self, ReadError, ViewKind};
use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

/// The custom scheme the app registers for [`MediaServer`].
pub const SCHEME: &str = "helmfile";

/// The address the renderer loads a token from (WebView2 maps a custom scheme to a `.localhost` host).
pub fn url_for(token: &str) -> String {
    if cfg!(windows) {
        format!("http://{SCHEME}.localhost/{token}")
    } else {
        format!("{SCHEME}://localhost/{token}")
    }
}

pub const MAX_SESSIONS: usize = 32;
/// The most bytes one response carries.
pub const CHUNK: u64 = 8 * 1024 * 1024;
/// The largest file answered whole when the request names no range.
pub const WHOLE_LIMIT: u64 = 32 * 1024 * 1024;
pub const FONT_LIMIT: u64 = 16 * 1024 * 1024;
const PDF_LIMIT: u64 = 1024 * 1024 * 1024;
const STREAM_LIMIT: u64 = 16 * 1024 * 1024 * 1024;

/// The largest file of a kind the viewer will preview by serving or listing it.
pub fn size_limit(kind: ViewKind) -> u64 {
    match kind {
        ViewKind::Font => FONT_LIMIT,
        ViewKind::Pdf => PDF_LIMIT,
        _ => STREAM_LIMIT,
    }
}

#[derive(Debug, Clone)]
struct Session {
    token: String,
    path: PathBuf,
    mime: &'static str,
    kind: ViewKind,
    size: u64,
    modified: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub token: String,
    pub mime: &'static str,
    pub size: u64,
}

/// One answer for the custom-scheme handler to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

pub struct MediaServer {
    roots: PrivateRoots,
    sessions: Mutex<VecDeque<Session>>,
}

#[derive(Debug, PartialEq, Eq)]
enum RangeError {
    Unsatisfiable,
}

/// The byte range a `Range` header asks for within a file of `size` bytes: `None` when there is no
/// header, otherwise the inclusive `(start, end)` already clamped to the file and to [`CHUNK`].
fn parse_range(header: Option<&str>, size: u64) -> Result<Option<(u64, u64)>, RangeError> {
    let Some(h) = header else { return Ok(None) };
    let spec = h
        .trim()
        .strip_prefix("bytes=")
        .ok_or(RangeError::Unsatisfiable)?;
    if spec.contains(',') || size == 0 {
        return Err(RangeError::Unsatisfiable);
    }
    let (a, b) = spec.split_once('-').ok_or(RangeError::Unsatisfiable)?;
    let (a, b) = (a.trim(), b.trim());
    let num = |s: &str| s.parse::<u64>().map_err(|_| RangeError::Unsatisfiable);
    let (start, end) = if a.is_empty() {
        // A suffix range: the last `n` bytes.
        let n = num(b)?;
        if n == 0 {
            return Err(RangeError::Unsatisfiable);
        }
        (size.saturating_sub(n), size - 1)
    } else {
        let start = num(a)?;
        let end = if b.is_empty() {
            size - 1
        } else {
            num(b)?.min(size - 1)
        };
        if start >= size || end < start {
            return Err(RangeError::Unsatisfiable);
        }
        (start, end)
    };
    Ok(Some((start, end.min(start + CHUNK - 1))))
}

impl MediaServer {
    pub fn new(roots: PrivateRoots) -> Self {
        Self {
            roots,
            sessions: Mutex::new(VecDeque::new()),
        }
    }

    /// Checks `raw` and issues a token for it. Only the kinds this server answers are admitted.
    pub fn open(&self, raw: &str) -> Result<Opened, ReadError> {
        let path = viewer::admit(raw, &self.roots)?;
        let meta = std::fs::symlink_metadata(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ReadError::NotFound,
            _ => ReadError::Io(e.to_string()),
        })?;
        if helm_platform::fs::is_link_like(&meta) || !meta.is_file() {
            return Err(ReadError::Refused(
                "Only regular files are previewed.".into(),
            ));
        }
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let (kind, mime) = match viewer::stream_kind(&ext) {
            Some((k, m)) if !matches!(k, ViewKind::Archive) => (k, m),
            _ => {
                return Err(ReadError::Refused(
                    "This kind of file is not previewed.".into(),
                ))
            }
        };
        let size = meta.len();
        if size > size_limit(kind) {
            return Err(ReadError::TooLarge(
                "The file is too large to preview here. Open it in its default app instead.",
            ));
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        let session = Session {
            token: token.clone(),
            path,
            mime,
            kind,
            size,
            modified: meta.modified().ok(),
        };
        let mut live = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        live.retain(|s| s.path != session.path);
        while live.len() >= MAX_SESSIONS {
            live.pop_front();
        }
        live.push_back(session);
        Ok(Opened { token, mime, size })
    }

    fn lookup(&self, token: &str) -> Option<Session> {
        let live = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        live.iter().find(|s| s.token == token).cloned()
    }

    /// Answers one request for `token`.
    pub fn respond(&self, method: &str, token: &str, range: Option<&str>) -> Reply {
        let head = method.eq_ignore_ascii_case("HEAD");
        if !head && !method.eq_ignore_ascii_case("GET") {
            return plain(405, "Only GET and HEAD are answered.");
        }
        let Some(s) = self.lookup(token) else {
            return plain(404, "Not found.");
        };
        match self.read(&s, range, head) {
            Ok(r) => r,
            Err((status, msg)) => {
                let mut r = plain(status, msg);
                if status == 416 {
                    r.headers
                        .push(("content-range", format!("bytes */{}", s.size)));
                }
                r
            }
        }
    }

    fn read(
        &self,
        s: &Session,
        range: Option<&str>,
        head: bool,
    ) -> Result<Reply, (u16, &'static str)> {
        // The same checks as on open, again: the file may have been swapped or linked since.
        let path = viewer::admit(&s.path.to_string_lossy(), &self.roots)
            .map_err(|_| (403, "This file is not available."))?;
        let mut file =
            helm_platform::fs::open_regular_no_follow(&path).map_err(|_| (404, "Not found."))?;
        let meta = file.metadata().map_err(|_| (404, "Not found."))?;
        if meta.len() != s.size || meta.modified().ok() != s.modified {
            return Err((410, "The file changed. Reload the preview."));
        }
        let size = s.size;
        let whole = if matches!(s.kind, ViewKind::Font) {
            FONT_LIMIT
        } else {
            WHOLE_LIMIT
        };
        let parsed = parse_range(range, size).map_err(|_| (416, "That range cannot be served."))?;
        let (start, end, partial) = match parsed {
            Some((a, b)) => (a, b, true),
            None if size <= whole => (0, size.saturating_sub(1), false),
            None => (0, CHUNK - 1, true),
        };
        let mut body = Vec::new();
        if size > 0 && !head {
            file.seek(SeekFrom::Start(start))
                .map_err(|_| (500, "Reading failed."))?;
            file.take(end - start + 1)
                .read_to_end(&mut body)
                .map_err(|_| (500, "Reading failed."))?;
        }
        let mut headers = vec![
            ("content-type", s.mime.to_string()),
            ("accept-ranges", "bytes".to_string()),
            ("cache-control", "no-store".to_string()),
            ("x-content-type-options", "nosniff".to_string()),
            (
                "content-security-policy",
                if matches!(s.kind, ViewKind::Pdf) {
                    "default-src 'none'; script-src 'none'; object-src 'none'".to_string()
                } else {
                    "default-src 'none'; sandbox".to_string()
                },
            ),
        ];
        if matches!(s.kind, ViewKind::Font) {
            // A font loads in CORS mode; the unguessable token is what protects it.
            headers.push(("access-control-allow-origin", "*".to_string()));
        }
        if partial {
            headers.push(("content-range", format!("bytes {start}-{end}/{size}")));
        }
        Ok(Reply {
            status: if partial { 206 } else { 200 },
            headers,
            body,
        })
    }
}

fn plain(status: u16, msg: &str) -> Reply {
    Reply {
        status,
        headers: vec![
            ("content-type", "text/plain; charset=utf-8".to_string()),
            ("cache-control", "no-store".to_string()),
            ("x-content-type-options", "nosniff".to_string()),
            (
                "content-security-policy",
                "default-src 'none'; sandbox".to_string(),
            ),
        ],
        body: msg.as_bytes().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type RangeCase = (
        Option<&'static str>,
        u64,
        Result<Option<(u64, u64)>, RangeError>,
    );

    #[test]
    fn range_headers_follow_the_single_range_rules() {
        let bad = || Err(RangeError::Unsatisfiable);
        let rows: Vec<RangeCase> = vec![
            (None, 100, Ok(None)),
            (Some("bytes=0-9"), 100, Ok(Some((0, 9)))),
            (Some("bytes=90-"), 100, Ok(Some((90, 99)))),
            (Some("bytes=90-500"), 100, Ok(Some((90, 99)))),
            (Some("bytes=-10"), 100, Ok(Some((90, 99)))),
            (Some("bytes=-500"), 100, Ok(Some((0, 99)))),
            (Some("bytes=100-"), 100, bad()),
            (Some("bytes=5-2"), 100, bad()),
            (Some("bytes=0-1,5-6"), 100, bad()),
            (Some("bytes=-0"), 100, bad()),
            (Some("items=0-1"), 100, bad()),
            (Some("bytes=0-"), 0, bad()),
            (Some("bytes=0-"), 3 * CHUNK, Ok(Some((0, CHUNK - 1)))),
        ];
        for (h, size, want) in rows {
            assert_eq!(parse_range(h, size), want, "{h:?} of {size}");
        }
    }

    #[test]
    fn tokens_serve_ranges_of_regular_media_only_and_refuse_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        std::fs::write(&wav, (0u8..100).collect::<Vec<_>>()).unwrap();
        let s = MediaServer::new(PrivateRoots::default());
        let o = s.open(&wav.to_string_lossy()).unwrap();
        assert_eq!((o.mime, o.size), ("audio/wav", 100));

        let all = s.respond("GET", &o.token, None);
        assert_eq!((all.status, all.body.len()), (200, 100));
        let part = s.respond("GET", &o.token, Some("bytes=10-19"));
        assert_eq!(part.status, 206);
        assert_eq!(part.body, (10u8..20).collect::<Vec<_>>());
        assert!(part
            .headers
            .contains(&("content-range", "bytes 10-19/100".to_string())));
        let head = s.respond("HEAD", &o.token, None);
        assert_eq!((head.status, head.body.len()), (200, 0));
        assert_eq!(s.respond("GET", &o.token, Some("bytes=500-")).status, 416);
        assert_eq!(s.respond("POST", &o.token, None).status, 405);
        assert_eq!(s.respond("GET", "nope", None).status, 404);
        let csp = all
            .headers
            .iter()
            .find(|h| h.0 == "content-security-policy")
            .unwrap();
        assert!(csp.1.contains("sandbox"));

        // A changed file is refused rather than served.
        std::fs::write(&wav, [1u8; 7]).unwrap();
        assert_eq!(s.respond("GET", &o.token, None).status, 410);

        // Other kinds and missing files never get a token.
        let txt = dir.path().join("a.txt");
        std::fs::write(&txt, "x").unwrap();
        assert!(s.open(&txt.to_string_lossy()).is_err());
        assert!(s
            .open(&dir.path().join("none.mp4").to_string_lossy())
            .is_err());

        // Only the newest sessions stay live.
        let first = s.open(&wav.to_string_lossy()).unwrap().token;
        for i in 0..MAX_SESSIONS {
            let p = dir.path().join(format!("{i}.mp3"));
            std::fs::write(&p, "x").unwrap();
            s.open(&p.to_string_lossy()).unwrap();
        }
        assert_eq!(s.respond("GET", &first, None).status, 404);
    }
}
