//! Page sessions and the `helmpage` scheme.
//!
//! Opening a page reads its file once (the host reads it, never the page), keeps the text as a
//! session under a random id and hands the renderer an address on this scheme. The scheme answers
//! three kinds of request and nothing else:
//!
//! - `/<session>/index.html`: a small prelude (charset, base style, the channel id, the runtime
//!   script) followed by the page's own HTML, with the page policy from [`csp`];
//! - `/<session>/helm-runtime.js`: the runtime that gives the page its `helm` object;
//! - `/<session>/<relative path>`: an image, style sheet, script, font or media file from the
//!   page's own folder, so a page made of several files keeps working. The path may not climb,
//!   name a drive or a stream, pass a link, enter one of Helm's own folders or carry an extension
//!   outside [`asset_mime`]'s list.
//!
//! Only `GET` and `HEAD` are answered. Every response is `no-store` and `nosniff`; an unknown
//! session, an unlisted path and a wrong method all get a plain answer under a deny-all policy.
//!
//! Budgets: at most [`MAX_SESSIONS`] sessions and [`MAX_RETAINED`] bytes of retained HTML. Over
//! either, an open is refused and the user is asked to close a page; a session nobody has touched
//! for [`IDLE_BEFORE_DROP`] is dropped first to make room.

use helm_panel::private::PrivateRoots;
use helm_panel::serve::Reply;
use helm_panel::viewer::{self, ReadError};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// The custom scheme the app registers for [`PageServer`].
pub const SCHEME: &str = "helmpage";
pub const DOCUMENT_FILE: &str = "index.html";
pub const RUNTIME_FILE: &str = "helm-runtime.js";
/// The runtime script every page loads.
pub const RUNTIME: &str = include_str!("runtime.js");

/// The largest page file.
pub const MAX_PAGE_BYTES: u64 = 10 * 1024 * 1024;
/// The largest file served beside a page.
pub const MAX_ASSET_BYTES: u64 = 25 * 1024 * 1024;
pub const MAX_SESSIONS: usize = 24;
/// Retained page text across all sessions.
pub const MAX_RETAINED: usize = 64 * 1024 * 1024;
pub const IDLE_BEFORE_DROP: Duration = Duration::from_secs(15 * 60);

/// The address the renderer loads a session from (WebView2 maps a custom scheme to a `.localhost`
/// host).
pub fn url_for(session: &str) -> String {
    if cfg!(windows) {
        format!("http://{SCHEME}.localhost/{session}/{DOCUMENT_FILE}")
    } else {
        format!("{SCHEME}://localhost/{session}/{DOCUMENT_FILE}")
    }
}

/// Why an open, a reload or a check was refused. The messages are sentences for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageError {
    Refused(String),
    NotFound(String),
    Io(String),
}

impl PageError {
    pub fn message(&self) -> &str {
        match self {
            Self::Refused(m) | Self::NotFound(m) | Self::Io(m) => m,
        }
    }
}

fn from_read(e: ReadError) -> PageError {
    match e {
        ReadError::NotFound => PageError::NotFound("The page was not found.".into()),
        ReadError::Io(m) => PageError::Io(m),
        other => PageError::Refused(other.message()),
    }
}

fn from_io(e: std::io::Error) -> PageError {
    match e.kind() {
        std::io::ErrorKind::NotFound => PageError::NotFound("The page was not found.".into()),
        std::io::ErrorKind::PermissionDenied => PageError::Refused(e.to_string()),
        _ => PageError::Io(e.to_string()),
    }
}

/// What the policy sent with a page's document allows.
///
/// Offline (the default) the page may load scripts, styles, images, fonts and media from its own
/// session and from inline, `data:` and `blob:` sources, and open connections to `blob:` and
/// `data:` only. That last rule is also what keeps the page away from the app's own IPC address.
/// With `network` the same, plus `https:` for loading and `https:` and `wss:` for connections.
/// Forms, plugins and `<base>` are always off, and the `sandbox` directive makes the document
/// itself script-only and origin-less even if something loaded it as a top-level page.
pub fn csp(network: bool) -> String {
    const SELF: &str = "'self' http://helmpage.localhost helmpage:";
    let net = if network { " https:" } else { "" };
    let connect = if network {
        "blob: data: https: wss:"
    } else {
        "blob: data:"
    };
    format!(
        "default-src 'none'; \
         script-src {SELF} 'unsafe-inline' 'unsafe-eval' blob: data:{net}; \
         style-src {SELF} 'unsafe-inline' blob: data:{net}; \
         img-src {SELF} blob: data:{net}; \
         font-src {SELF} blob: data:{net}; \
         media-src {SELF} blob: data:{net}; \
         connect-src {connect}; \
         worker-src blob:; \
         frame-src blob: data:{net}; \
         object-src 'none'; base-uri 'none'; form-action 'none'; \
         sandbox allow-scripts allow-downloads"
    )
}

/// The content type of a file served beside a page, by extension; `None` for anything else.
pub fn asset_mime(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => return None,
    })
}

const BASE_STYLE: &str = "\
:root{color-scheme:light dark}\
html{background:var(--helm-bg,Canvas);color:var(--helm-text,CanvasText);font-family:var(--helm-font,system-ui,sans-serif)}\
code,pre,kbd,samp{font-family:var(--helm-mono,ui-monospace,monospace)}\
html[data-helm-loading] body{visibility:hidden}\
.helm-card{background:var(--helm-surface,Canvas);border:1px solid var(--helm-border,#8886);border-radius:10px;padding:12px}\
.helm-row{display:flex;gap:8px;align-items:center}\
.helm-muted{color:var(--helm-muted,GrayText)}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
        }
    }
}

struct Session {
    id: String,
    channel: String,
    path: PathBuf,
    key: String,
    dir: PathBuf,
    html: String,
    network: bool,
    stamp: Stamp,
    revision: u64,
    touched: Instant,
}

/// What a read of a page file produced.
struct Loaded {
    path: PathBuf,
    key: String,
    html: String,
    stamp: Stamp,
}

/// A page file that passed every check, without its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub path: PathBuf,
    pub key: String,
    pub title: String,
}

/// An open session, as the renderer needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub session: String,
    pub channel: String,
    pub url: String,
    pub path: String,
    pub key: String,
    pub title: String,
    pub network: bool,
    pub revision: u64,
    pub size: usize,
}

impl Opened {
    pub fn to_json(&self) -> Value {
        json!({
            "session": self.session,
            "channel": self.channel,
            "url": self.url,
            "path": self.path,
            "key": self.key,
            "title": self.title,
            "network": self.network,
            "revision": self.revision,
            "size": self.size,
        })
    }
}

/// The file watch's answer for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub revision: u64,
    /// The file was re-read since the last status.
    pub changed: bool,
    /// The file is gone (the page keeps showing what it had).
    pub missing: bool,
    /// The file changed but could not be read yet (still being written, too big...).
    pub error: Option<String>,
}

pub struct PageServer {
    roots: PrivateRoots,
    sessions: Mutex<Vec<Session>>,
}

/// Slash-normalised, lower-cased on Windows, without the verbatim prefix: the key a page's saved
/// state and pin are filed under.
pub fn path_key(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\");
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.to_owned()
    }
}

fn plain(p: PathBuf) -> PathBuf {
    let rest = p
        .to_string_lossy()
        .strip_prefix(r"\\?\")
        .filter(|r| !r.starts_with("UNC\\"))
        .map(str::to_owned);
    match rest {
        Some(r) => PathBuf::from(r),
        None => p,
    }
}

fn title_of(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Page".to_owned())
}

fn extension(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The file a relative address names inside `dir`, or `None` when the address is not a plain
/// relative path: no empty or `.`/`..` segments, no backslash, drive colon, control character or
/// trailing dot or space (Windows reads those as another name).
fn asset_path(dir: &Path, rel: &str) -> Option<PathBuf> {
    let decoded = percent_decode(rel)?;
    if decoded.is_empty() || decoded.len() > 1024 {
        return None;
    }
    let mut out = dir.to_path_buf();
    for seg in decoded.split('/') {
        if seg.is_empty()
            || seg == "."
            || seg == ".."
            || seg.contains(['\\', ':'])
            || seg.chars().any(char::is_control)
            || seg.ends_with('.')
            || seg.ends_with(' ')
        {
            return None;
        }
        out.push(seg);
    }
    Some(out)
}

impl PageServer {
    pub fn new(roots: PrivateRoots) -> Self {
        Self {
            roots,
            sessions: Mutex::new(Vec::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Session>> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn session_count(&self) -> usize {
        self.lock().len()
    }

    /// Checks a path and returns its canonical form and key. Reads no content.
    pub fn check(&self, raw: &str) -> Result<Checked, PageError> {
        let (path, _) = self.admit_file(raw)?;
        Ok(Checked {
            key: path_key(&path),
            title: title_of(&path),
            path,
        })
    }

    /// The gate for a page file: the path gate, no links, not in a private folder, an `.html` or
    /// `.htm` regular file of 1 byte to [`MAX_PAGE_BYTES`]. Returns the canonical path and the
    /// size and time seen.
    fn admit_file(&self, raw: &str) -> Result<(PathBuf, Stamp), PageError> {
        let path = viewer::admit(raw, &self.roots).map_err(from_read)?;
        if !matches!(extension(&path).as_str(), "html" | "htm") {
            return Err(PageError::Refused(
                "Only .html and .htm files run as pages.".into(),
            ));
        }
        let meta = std::fs::symlink_metadata(&path).map_err(from_io)?;
        if helm_platform::fs::is_link_like(&meta) || !meta.is_file() {
            return Err(PageError::Refused(
                "Only regular files run as pages.".into(),
            ));
        }
        if meta.len() == 0 {
            return Err(PageError::Refused("The page file is empty.".into()));
        }
        if meta.len() > MAX_PAGE_BYTES {
            return Err(PageError::Refused(
                "The page is larger than 10 MiB, so it does not run here.".into(),
            ));
        }
        // The canonical spelling decides the key and the private-folder check, so another casing
        // or a short name cannot reach a different row or a folder that is off limits.
        let canon = plain(std::fs::canonicalize(&path).map_err(from_io)?);
        if self.roots.contains(&canon) {
            return Err(PageError::Refused(viewer::PRIVATE_MESSAGE.into()));
        }
        Ok((canon, Stamp::of(&meta)))
    }

    /// Reads a page file whole: opened without following a link, size and time compared before
    /// and after, valid UTF-8 with no NUL bytes.
    fn load(&self, raw: &str) -> Result<Loaded, PageError> {
        let (path, before) = self.admit_file(raw)?;
        let file = helm_platform::fs::open_regular_no_follow(&path).map_err(from_io)?;
        let mut bytes = Vec::new();
        (&file)
            .take(MAX_PAGE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(from_io)?;
        let after = Stamp::of(&file.metadata().map_err(from_io)?);
        if after != before {
            return Err(PageError::Refused(
                "The file changed while it was being read. Try again.".into(),
            ));
        }
        if bytes.len() as u64 > MAX_PAGE_BYTES {
            return Err(PageError::Refused(
                "The page is larger than 10 MiB, so it does not run here.".into(),
            ));
        }
        if bytes.contains(&0) {
            return Err(PageError::Refused("This is not a text file.".into()));
        }
        let html = String::from_utf8(bytes)
            .map_err(|_| PageError::Refused("The page must be UTF-8 text.".into()))?;
        Ok(Loaded {
            key: path_key(&path),
            path,
            html,
            stamp: after,
        })
    }

    fn opened(s: &Session) -> Opened {
        Opened {
            session: s.id.clone(),
            channel: s.channel.clone(),
            url: url_for(&s.id),
            path: s.path.to_string_lossy().into_owned(),
            key: s.key.clone(),
            title: title_of(&s.path),
            network: s.network,
            revision: s.revision,
            size: s.html.len(),
        }
    }

    /// Reads a page and starts a session for it. `network` is fixed for the session's life.
    pub fn open(&self, raw: &str, network: bool) -> Result<Opened, PageError> {
        let loaded = self.load(raw)?;
        let mut live = self.lock();
        if live.len() >= MAX_SESSIONS
            || live.iter().map(|s| s.html.len()).sum::<usize>() + loaded.html.len() > MAX_RETAINED
        {
            live.retain(|s| s.touched.elapsed() < IDLE_BEFORE_DROP);
        }
        if live.len() >= MAX_SESSIONS
            || live.iter().map(|s| s.html.len()).sum::<usize>() + loaded.html.len() > MAX_RETAINED
        {
            return Err(PageError::Refused(
                "Too many pages are open. Close one first.".into(),
            ));
        }
        let dir = loaded
            .path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| PageError::Refused("The page has no folder.".into()))?;
        let session = Session {
            id: uuid::Uuid::new_v4().simple().to_string(),
            channel: uuid::Uuid::new_v4().simple().to_string(),
            path: loaded.path,
            key: loaded.key,
            dir,
            html: loaded.html,
            network,
            stamp: loaded.stamp,
            revision: 1,
            touched: Instant::now(),
        };
        let out = Self::opened(&session);
        live.push(session);
        Ok(out)
    }

    /// Ends a session; true when there was one.
    pub fn close(&self, id: &str) -> bool {
        let mut live = self.lock();
        let before = live.len();
        live.retain(|s| s.id != id);
        live.len() != before
    }

    /// The page key a session was opened for. The only way an op learns what a session means.
    pub fn key_of(&self, id: &str) -> Option<String> {
        self.lock()
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.key.clone())
    }

    fn not_open() -> PageError {
        PageError::NotFound("That page is no longer open.".into())
    }

    /// Reads the file again now (the user's Reload) and bumps the revision.
    pub fn reload(&self, id: &str) -> Result<Opened, PageError> {
        let path = {
            let live = self.lock();
            let s = live
                .iter()
                .find(|s| s.id == id)
                .ok_or_else(Self::not_open)?;
            s.path.to_string_lossy().into_owned()
        };
        let loaded = self.load(&path)?;
        let mut live = self.lock();
        let others: usize = live
            .iter()
            .filter(|s| s.id != id)
            .map(|s| s.html.len())
            .sum();
        if others + loaded.html.len() > MAX_RETAINED {
            return Err(PageError::Refused(
                "Too many pages are open. Close one first.".into(),
            ));
        }
        let s = live
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(Self::not_open)?;
        s.html = loaded.html;
        s.stamp = loaded.stamp;
        s.revision += 1;
        s.touched = Instant::now();
        Ok(Self::opened(s))
    }

    /// Looks at the file: when its size or time moved since the session last read it, reads it
    /// again and bumps the revision (the renderer then reloads the frame in place). Cheap when
    /// nothing changed: one stat.
    pub fn status(&self, id: &str) -> Result<Status, PageError> {
        let (path, stamp, revision) = {
            let mut live = self.lock();
            let s = live
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or_else(Self::not_open)?;
            s.touched = Instant::now();
            (s.path.to_string_lossy().into_owned(), s.stamp, s.revision)
        };
        let quiet = |error: Option<String>, missing: bool| Status {
            revision,
            changed: false,
            missing,
            error,
        };
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(quiet(None, true)),
            Err(e) => return Ok(quiet(Some(e.to_string()), false)),
        };
        if Stamp::of(&meta) == stamp {
            return Ok(quiet(None, false));
        }
        match self.load(&path) {
            Err(e) => Ok(quiet(Some(e.message().to_owned()), false)),
            Ok(loaded) => {
                let mut live = self.lock();
                let others: usize = live
                    .iter()
                    .filter(|s| s.id != id)
                    .map(|s| s.html.len())
                    .sum();
                if others + loaded.html.len() > MAX_RETAINED {
                    return Ok(quiet(Some("Too many pages are open.".into()), false));
                }
                let s = live
                    .iter_mut()
                    .find(|s| s.id == id)
                    .ok_or_else(Self::not_open)?;
                s.html = loaded.html;
                s.stamp = loaded.stamp;
                s.revision += 1;
                Ok(Status {
                    revision: s.revision,
                    changed: true,
                    missing: false,
                    error: None,
                })
            }
        }
    }

    /// Answers one request. `path` is the request's path, without the query.
    pub fn respond(&self, method: &str, path: &str) -> Reply {
        let head = method.eq_ignore_ascii_case("HEAD");
        if !head && !method.eq_ignore_ascii_case("GET") {
            return deny(405, "Only GET and HEAD are answered.");
        }
        let trimmed = path.trim_start_matches('/');
        let (id, rest) = trimmed.split_once('/').unwrap_or((trimmed, ""));
        let dir = {
            let mut live = self.lock();
            let Some(s) = live.iter_mut().find(|s| s.id == id) else {
                return deny(404, "Not found.");
            };
            s.touched = Instant::now();
            match rest {
                "" | DOCUMENT_FILE => {
                    let body = document(s);
                    return page_reply(
                        "text/html; charset=utf-8",
                        Some(csp(s.network)),
                        body.into_bytes(),
                        head,
                    );
                }
                RUNTIME_FILE => {
                    return page_reply(
                        "text/javascript; charset=utf-8",
                        None,
                        RUNTIME.as_bytes().to_vec(),
                        head,
                    );
                }
                _ => s.dir.clone(),
            }
        };
        self.asset(&dir, rest, head)
    }

    fn asset(&self, dir: &Path, rel: &str, head: bool) -> Reply {
        let Some(path) = asset_path(dir, rel) else {
            return deny(404, "Not found.");
        };
        let Some(mime) = asset_mime(&extension(&path)) else {
            return deny(404, "Not found.");
        };
        // The same gate as the page itself: no links along the way, no private folder.
        let Ok(path) = viewer::admit(&path.to_string_lossy(), &self.roots) else {
            return deny(404, "Not found.");
        };
        let Ok(file) = helm_platform::fs::open_regular_no_follow(&path) else {
            return deny(404, "Not found.");
        };
        let Ok(meta) = file.metadata() else {
            return deny(404, "Not found.");
        };
        if meta.len() > MAX_ASSET_BYTES {
            return deny(404, "Not found.");
        }
        let mut body = Vec::new();
        if !head
            && (&file)
                .take(MAX_ASSET_BYTES)
                .read_to_end(&mut body)
                .is_err()
        {
            return deny(404, "Not found.");
        }
        let mut r = page_reply(mime, Some("default-src 'none'; sandbox".into()), body, head);
        if mime.starts_with("font/") {
            // A font loads in CORS mode. The unguessable session address is what protects it.
            r.headers
                .push(("access-control-allow-origin", "*".to_string()));
        }
        r
    }
}

/// The served document: the host prelude, then the page's own HTML verbatim. The prelude opens no
/// `<html>` or `<head>` of its own, so the page's tags merge into the ones the parser makes.
fn document(s: &Session) -> String {
    let mut out = String::with_capacity(s.html.len() + 1024);
    out.push_str("<!doctype html><meta charset=\"utf-8\">");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    out.push_str("<style>");
    out.push_str(BASE_STYLE);
    out.push_str("</style><script>document.documentElement.setAttribute('data-helm-loading','');window.__helmBootstrap={\"channel\":\"");
    out.push_str(&s.channel);
    out.push_str("\"};</script><script src=\"");
    out.push_str(RUNTIME_FILE);
    out.push_str("\"></script>");
    out.push_str(&s.html);
    out
}

fn page_reply(mime: &str, policy: Option<String>, body: Vec<u8>, head: bool) -> Reply {
    let mut headers = vec![
        ("content-type", mime.to_string()),
        ("cache-control", "no-store".to_string()),
        ("x-content-type-options", "nosniff".to_string()),
        ("referrer-policy", "no-referrer".to_string()),
    ];
    if let Some(p) = policy {
        headers.push(("content-security-policy", p));
    }
    Reply {
        status: 200,
        headers,
        body: if head { Vec::new() } else { body },
    }
}

fn deny(status: u16, msg: &str) -> Reply {
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
    use std::fs;

    fn header<'a>(r: &'a Reply, name: &str) -> Option<&'a str> {
        r.headers.iter().find(|h| h.0 == name).map(|h| h.1.as_str())
    }

    fn server() -> PageServer {
        PageServer::new(PrivateRoots::default())
    }

    /// The page document, the runtime and the assets beside a page: what is served, with which
    /// policy, and every address that must not reach a file.
    #[test]
    fn a_session_serves_its_page_the_runtime_and_plain_assets_only() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        fs::write(dir.join("p.html"), "<h1 id=x>Hi</h1><script>1</script>").unwrap();
        fs::write(dir.join("pic.png"), [1u8, 2, 3]).unwrap();
        fs::write(dir.join("style.css"), "a{}").unwrap();
        fs::write(dir.join("f.woff2"), [9u8]).unwrap();
        fs::write(dir.join("data.json"), "{}").unwrap();
        fs::write(dir.join("other.html"), "<p>other</p>").unwrap();
        fs::create_dir(dir.join("sub")).unwrap();
        fs::write(dir.join("sub").join("deep.png"), [7u8]).unwrap();
        let secret = tempfile::tempdir().unwrap();
        fs::write(secret.path().join("s.png"), [5u8]).unwrap();

        let s = server();
        let o = s
            .open(&dir.join("p.html").to_string_lossy(), false)
            .unwrap();
        assert_eq!(o.title, "p");
        assert!(o.url.contains(&o.session) && o.url.ends_with("/index.html"));
        assert_ne!(o.session, o.channel);

        // The document: prelude with the channel, the page verbatim, and the offline policy.
        let doc = s.respond("GET", &format!("/{}/index.html", o.session));
        assert_eq!(doc.status, 200);
        let body = String::from_utf8(doc.body.clone()).unwrap();
        assert!(body.starts_with("<!doctype html>"));
        assert!(body.contains(&format!("\"channel\":\"{}\"", o.channel)));
        assert!(body.ends_with("<h1 id=x>Hi</h1><script>1</script>"));
        assert!(
            !body.contains(&o.session),
            "the address never goes into the page"
        );
        let policy = header(&doc, "content-security-policy").unwrap();
        assert!(policy.contains("connect-src blob: data:;"));
        assert!(!policy.contains("https:") && !policy.contains("wss:"));
        assert!(policy.contains("sandbox allow-scripts allow-downloads"));
        assert!(policy.contains("form-action 'none'") && policy.contains("base-uri 'none'"));
        assert_eq!(header(&doc, "cache-control"), Some("no-store"));
        assert_eq!(header(&doc, "x-content-type-options"), Some("nosniff"));
        assert_eq!(header(&doc, "referrer-policy"), Some("no-referrer"));
        // The bare session address is the document too; HEAD carries no body.
        assert_eq!(s.respond("GET", &format!("/{}/", o.session)).body, doc.body);
        assert!(s
            .respond("HEAD", &format!("/{}/index.html", o.session))
            .body
            .is_empty());

        // Network on widens loading and connections, nothing else.
        let net = s.open(&dir.join("p.html").to_string_lossy(), true).unwrap();
        let npol = header(
            &s.respond("GET", &format!("/{}/index.html", net.session)),
            "content-security-policy",
        )
        .unwrap()
        .to_owned();
        assert!(npol.contains("connect-src blob: data: https: wss:;"));
        assert!(npol.contains("form-action 'none'") && !npol.contains("http: "));

        // The runtime script.
        let rt = s.respond("GET", &format!("/{}/{RUNTIME_FILE}", o.session));
        assert_eq!(rt.status, 200);
        assert_eq!(rt.body, RUNTIME.as_bytes());

        // Assets: listed types from the page's folder and below.
        let get = |p: &str| s.respond("GET", &format!("/{}/{p}", o.session));
        assert_eq!(get("pic.png").body, [1, 2, 3]);
        assert_eq!(header(&get("pic.png"), "content-type"), Some("image/png"));
        assert_eq!(get("style.css").status, 200);
        assert_eq!(get("sub/deep.png").body, [7]);
        assert_eq!(get("sub%2Fdeep.png").status, 200);
        assert_eq!(
            header(&get("f.woff2"), "access-control-allow-origin"),
            Some("*")
        );
        assert_eq!(header(&get("pic.png"), "access-control-allow-origin"), None);
        // Everything else is a 404 under the deny-all policy: other types, other pages, climbing,
        // drives, streams, backslashes, odd names, and a missing file.
        let out = secret.path().join("s.png").to_string_lossy().into_owned();
        for bad in [
            "data.json",
            "other.html",
            "p.html",
            "missing.png",
            "../s.png",
            "sub/../pic.png",
            "%2e%2e/s.png",
            "..%2fs.png",
            "..%5cs.png",
            "sub\\deep.png",
            "C:/Windows/win.ini",
            "pic.png:stream",
            "pic.png.",
            "pic.png%20",
            "/pic.png",
            "sub//deep.png",
            "sub/",
            "%zz.png",
            "a%00.png",
            out.as_str(),
        ] {
            let r = get(bad);
            assert_eq!(r.status, 404, "{bad}");
            assert_eq!(
                header(&r, "content-security-policy"),
                Some("default-src 'none'; sandbox"),
                "{bad}"
            );
        }

        // Methods and unknown sessions.
        assert_eq!(
            s.respond("POST", &format!("/{}/index.html", o.session))
                .status,
            405
        );
        assert_eq!(s.respond("GET", "/nope/index.html").status, 404);
        assert_eq!(s.respond("GET", "/").status, 404);

        // Closing ends the session.
        assert!(s.close(&o.session));
        assert!(!s.close(&o.session));
        assert_eq!(
            s.respond("GET", &format!("/{}/index.html", o.session))
                .status,
            404
        );
    }

    /// Which files may run as a page, how a changed file shows up, and the session budget.
    #[test]
    fn pages_are_admitted_read_whole_watched_and_budgeted() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        let p = dir.join("a.html");
        fs::write(&p, "<p>one</p>").unwrap();
        let s = server();

        // Refusals: wrong type, empty, binary, not UTF-8, too big, a folder, missing.
        fs::write(dir.join("a.txt"), "x").unwrap();
        fs::write(dir.join("empty.html"), "").unwrap();
        fs::write(dir.join("nul.html"), b"<p>a\0b</p>").unwrap();
        fs::write(dir.join("latin.html"), [0x3c, 0x70, 0x3e, 0xe9]).unwrap();
        let big = fs::File::create(dir.join("big.html")).unwrap();
        big.set_len(MAX_PAGE_BYTES + 1).unwrap();
        fs::create_dir(dir.join("dir.html")).unwrap();
        for bad in [
            "a.txt",
            "empty.html",
            "nul.html",
            "latin.html",
            "big.html",
            "dir.html",
        ] {
            let e = s.open(&dir.join(bad).to_string_lossy(), false).unwrap_err();
            assert!(matches!(e, PageError::Refused(_)), "{bad}: {e:?}");
        }
        assert!(matches!(
            s.open(&dir.join("none.html").to_string_lossy(), false),
            Err(PageError::NotFound(_))
        ));
        assert!(s.open("relative/page.html", false).is_err());
        // A private folder is never a page.
        let private = PageServer::new(PrivateRoots::new(&[dir.to_path_buf()]));
        assert!(private.open(&p.to_string_lossy(), false).is_err());
        assert!(private.check(&p.to_string_lossy()).is_err());

        // The key ignores casing and spelling of the same file.
        let o = s.open(&p.to_string_lossy(), false).unwrap();
        let upper = s.check(&p.to_string_lossy().to_uppercase());
        if cfg!(windows) {
            assert_eq!(upper.unwrap().key, o.key);
        }
        assert_eq!(s.key_of(&o.session).as_deref(), Some(o.key.as_str()));

        // Nothing changed: same revision. A rewrite: re-read and a new revision.
        let st = s.status(&o.session).unwrap();
        assert_eq!((st.revision, st.changed, st.missing), (1, false, false));
        fs::write(&p, "<p>two, longer</p>").unwrap();
        let st = s.status(&o.session).unwrap();
        assert_eq!((st.revision, st.changed), (2, true));
        let doc = s.respond("GET", &format!("/{}/index.html", o.session));
        assert!(String::from_utf8(doc.body)
            .unwrap()
            .ends_with("<p>two, longer</p>"));
        // A half-written (empty) file is reported and the old page stays until it is whole.
        fs::write(&p, "").unwrap();
        let st = s.status(&o.session).unwrap();
        assert_eq!((st.revision, st.changed), (2, false));
        assert!(st.error.is_some());
        fs::write(&p, "<p>three</p>").unwrap();
        assert_eq!(s.status(&o.session).unwrap().revision, 3);
        // Reload re-reads regardless and bumps the revision; a deleted file is reported missing.
        assert_eq!(s.reload(&o.session).unwrap().revision, 4);
        fs::remove_file(&p).unwrap();
        assert!(s.status(&o.session).unwrap().missing);
        assert!(s.reload(&o.session).is_err());
        assert!(matches!(s.status("nope"), Err(PageError::NotFound(_))));

        // At most MAX_SESSIONS sessions; a new open is refused until one closes.
        let q = dir.join("q.html");
        fs::write(&q, "<p>q</p>").unwrap();
        let mut ids = vec![];
        for _ in 0..MAX_SESSIONS - 1 {
            ids.push(s.open(&q.to_string_lossy(), false).unwrap().session);
        }
        assert_eq!(s.session_count(), MAX_SESSIONS);
        let e = s.open(&q.to_string_lossy(), false).unwrap_err();
        assert!(e.message().contains("Close one first"));
        assert!(s.close(&ids[0]));
        assert!(s.open(&q.to_string_lossy(), false).is_ok());
    }

    #[test]
    fn the_policy_is_closed_by_default_and_opens_only_https_and_wss() {
        let off = csp(false);
        let on = csp(true);
        for p in [&off, &on] {
            assert!(p.starts_with("default-src 'none';"));
            assert!(p.contains("object-src 'none'") && p.contains("worker-src blob:;"));
            assert!(!p.contains("http: ") && !p.contains(" *"));
        }
        assert_eq!(off.matches("https:").count(), 0);
        assert!(on.contains("script-src") && on.contains("https:") && on.contains("wss:"));
        // Connections never name the app's own IPC address or the local file scheme.
        for p in [&off, &on] {
            let connect = p
                .split("connect-src ")
                .nth(1)
                .unwrap()
                .split(';')
                .next()
                .unwrap();
            assert!(!connect.contains("ipc") && !connect.contains("localhost"));
        }
    }
}
