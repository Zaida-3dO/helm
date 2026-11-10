//! `app_voice_show`: put something on the phone's screen to go with the spoken answer.
//!
//! Only the voice chat may call it. A picture, a table, a list, a link, some text or a file is
//! checked here, read from disk if it is a file, and queued for the answer of the turn that is
//! running. The voice mirror ([`crate::voice_sink`]) sends each queued item up the line as a
//! `voice_attachment` frame just before the answer's own frame, so the phone has them when it
//! speaks. Nothing is ever put on screen by itself: the agent is told to say, in a sentence, what
//! it showed.
//!
//! Limits: four things per answer; a title of at most 120 characters; pictures (PNG, JPEG or
//! WebP) and files of at most 1 MiB; markdown of at most 16,000 characters, a table of at most 50
//! rows and 8 columns; a link must be `https`.

use crate::voice;
use base64::Engine as _;
use helm_core::hidden::is_hidden_char;
use helm_core::profile::Profile;
use helm_hosttools::{HostToolRegistry, ToolCtx, ToolError, ToolResult, ToolSpec};
use helm_platform::private::PrivateRoots;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The most things shown with one answer.
pub const MAX_PER_REPLY: usize = 4;
pub const MAX_TITLE: usize = 120;
/// The most a picture or file may weigh.
pub const MAX_BYTES: u64 = 1024 * 1024;
pub const MAX_MARKDOWN: usize = 16_000;
pub const MAX_TABLE_ROWS: usize = 50;
pub const MAX_TABLE_COLUMNS: usize = 8;
pub const MAX_URL: usize = 2000;

/// What the tool answers when it has queued the item.
pub const SHOWN: &str = "Shown on the user's screen with your next reply.";
const TOO_BIG: &str = "That is too big to show; say it is in the chat instead.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Table,
    List,
    Link,
    Text,
    File,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Table => "table",
            Kind::List => "list",
            Kind::Link => "link",
            Kind::Text => "text",
            Kind::File => "file",
        }
    }
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "image" => Kind::Image,
            "table" => Kind::Table,
            "list" => Kind::List,
            "link" => Kind::Link,
            "text" => Kind::Text,
            "file" => Kind::File,
            _ => return None,
        })
    }
}

/// One thing to show, ready for the line.
#[derive(Clone, Debug, PartialEq)]
pub struct Attachment {
    pub kind: Kind,
    pub title: String,
    /// Pictures and files: the type, and the bytes in base64.
    pub media_type: Option<String>,
    pub data: Option<String>,
    /// Tables, lists and text.
    pub markdown: Option<String>,
    /// Links.
    pub url: Option<String>,
}

impl Attachment {
    /// The frame for the line, for the reply with this id; `n` counts from 1.
    pub fn frame(&self, reply: &str, n: usize) -> Value {
        let mut f = json!({
            "type": "voice_attachment",
            "reply": reply,
            "n": n,
            "kind": self.kind.as_str(),
            "title": self.title,
        });
        for (key, v) in [
            ("mediaType", &self.media_type),
            ("data", &self.data),
            ("markdown", &self.markdown),
            ("url", &self.url),
        ] {
            if let Some(v) = v {
                f[key] = json!(v);
            }
        }
        f
    }
}

/// Where the content of a call comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Path(String),
    Markdown(String),
    Url(String),
}

/// A call whose shape has been checked; nothing has been read from disk yet.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub kind: Kind,
    pub title: String,
    pub source: Source,
}

fn refused(msg: impl Into<String>) -> ToolError {
    ToolError::refused(msg)
}

/// A field that must be text when present.
fn field(o: &Map<String, Value>, key: &str) -> Result<Option<String>, ToolError> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(refused(format!("{key} must be text."))),
    }
}

/// Rows (not counting the divider line) and the widest row of a markdown table, or `None` when the
/// text holds no table.
fn table_shape(md: &str) -> Option<(usize, usize)> {
    let mut rows = 0;
    let mut columns = 0;
    for line in md.lines().map(str::trim).filter(|l| l.contains('|')) {
        let divider = line
            .chars()
            .all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
            && line.contains('-');
        if divider {
            continue;
        }
        rows += 1;
        columns = columns.max(line.trim_matches('|').split('|').count());
    }
    (rows > 0).then_some((rows, columns))
}

fn check_markdown(kind: Kind, md: &str) -> Result<(), ToolError> {
    if md.trim().is_empty() {
        return Err(refused("markdown is required and cannot be blank."));
    }
    if md.chars().count() > MAX_MARKDOWN {
        return Err(refused(format!(
            "markdown is longer than {MAX_MARKDOWN} characters; show less, or say it is in the chat."
        )));
    }
    if kind == Kind::Table {
        let Some((rows, columns)) = table_shape(md) else {
            return Err(refused(
                "a table needs markdown rows with | between the cells.",
            ));
        };
        if rows > MAX_TABLE_ROWS || columns > MAX_TABLE_COLUMNS {
            return Err(refused(format!(
                "a table can have at most {MAX_TABLE_ROWS} rows and {MAX_TABLE_COLUMNS} columns; show less, or say it is in the chat."
            )));
        }
    }
    Ok(())
}

fn check_url(url: &str) -> Result<(), ToolError> {
    let https = url
        .get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("https://"));
    if !https
        || url.len() == 8
        || url.chars().count() > MAX_URL
        || url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || is_hidden_char(c))
    {
        return Err(refused(format!(
            "url must be an https:// address of at most {MAX_URL} characters."
        )));
    }
    Ok(())
}

/// Check the shape of a call. Each kind takes exactly one source: a picture or a file a `path`, a
/// table, list or text `markdown`, a link a `url`.
pub fn parse(a: &Value) -> Result<Request, ToolError> {
    let o = a
        .as_object()
        .ok_or_else(|| refused("Expected an object with kind, title and what to show."))?;
    if let Some(extra) = o
        .keys()
        .find(|k| !["kind", "title", "path", "markdown", "url"].contains(&k.as_str()))
    {
        return Err(refused(format!(
            "{extra} is not a field of app_voice_show."
        )));
    }
    let kind = o
        .get("kind")
        .and_then(Value::as_str)
        .and_then(Kind::parse)
        .ok_or_else(|| refused("kind must be image, table, list, link, text or file."))?;
    let title = field(o, "title")?
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| refused("title is required: a short caption for what is on screen."))?;
    if title.chars().count() > MAX_TITLE {
        return Err(refused(format!(
            "title is longer than {MAX_TITLE} characters."
        )));
    }
    if title.chars().any(|c| c.is_control() || is_hidden_char(c)) {
        return Err(refused(
            "title cannot hold line breaks or invisible characters.",
        ));
    }
    let (path, markdown, url) = (field(o, "path")?, field(o, "markdown")?, field(o, "url")?);
    let source = match (kind, path, markdown, url) {
        (Kind::Image | Kind::File, Some(p), None, None) if !p.trim().is_empty() => Source::Path(p),
        (Kind::Table | Kind::List | Kind::Text, None, Some(m), None) => {
            check_markdown(kind, &m)?;
            Source::Markdown(m)
        }
        (Kind::Link, None, None, Some(u)) => {
            check_url(&u)?;
            Source::Url(u)
        }
        _ => {
            let needs = match kind {
                Kind::Image | Kind::File => "path",
                Kind::Link => "url",
                Kind::Table | Kind::List | Kind::Text => "markdown",
            };
            return Err(refused(format!(
                "{} takes {needs} and nothing else.",
                kind.as_str()
            )));
        }
    };
    Ok(Request {
        kind,
        title,
        source,
    })
}

/// Whether a path looks like somewhere secrets are kept. These are never sent to a phone,
/// whatever the agent was asked.
pub fn sensitive(path: &Path) -> bool {
    const FOLDERS: [&str; 9] = [
        ".ssh", ".aws", ".gnupg", ".azure", ".kube", ".docker", ".claude", ".gcloud", ".config",
    ];
    const ENDS: [&str; 7] = [".pem", ".key", ".pfx", ".p12", ".kdbx", ".ppk", ".keystore"];
    let lower = path.to_string_lossy().to_lowercase().replace('\\', "/");
    let mut parts = lower.split('/').filter(|p| !p.is_empty()).peekable();
    while let Some(p) = parts.next() {
        let last = parts.peek().is_none();
        if !last && FOLDERS.contains(&p) {
            return true;
        }
        if last
            && (p.starts_with(".env")
                || p.starts_with("id_rsa")
                || p.starts_with("id_ed25519")
                || p.starts_with("id_ecdsa")
                || p.contains("credentials")
                || p.contains("secret")
                || p.ends_with(".npmrc")
                || ENDS.iter().any(|e| p.ends_with(e)))
        {
            return true;
        }
    }
    false
}

/// The path a picture or file may be read from, or the refusal. Nothing is read here: the side
/// panel's checks come first (no shares, device paths, reserved names or alternate streams), then
/// no links along the way, then neither Helm's own folders nor places secrets are kept.
pub fn admit(raw: &str, roots: &PrivateRoots) -> Result<PathBuf, ToolError> {
    let path = helm_platform::pathgate::check(raw.trim()).map_err(|r| refused(r.message()))?;
    helm_platform::fs::refuse_links_along(&path).map_err(|e| refused(e.to_string()))?;
    if roots.contains(&path) {
        return Err(refused("Helm's own data is not shown."));
    }
    if sensitive(&path) {
        return Err(refused(
            "That file may hold secrets, so it is not sent to the phone.",
        ));
    }
    Ok(path)
}

/// The picture type from a file's first bytes; only PNG, JPEG and WebP are shown.
fn picture_type(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("image/png")
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// A file's type from its name, for the phone to pick a viewer; unknown ones are plain bytes.
fn file_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("pdf") => "application/pdf",
        Some("txt" | "log") => "text/plain",
        Some("md") => "text/markdown",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html",
        Some("zip") => "application/zip",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    }
}

/// Read a picture or file already admitted: its type and bytes.
pub fn read(path: &Path, kind: Kind) -> Result<(String, Vec<u8>), ToolError> {
    let bytes = helm_core::privfs::read_capped(path, MAX_BYTES).map_err(|e| match e.kind() {
        std::io::ErrorKind::InvalidData => refused(TOO_BIG),
        std::io::ErrorKind::NotFound => refused("That file was not found."),
        _ => refused("That file could not be read."),
    })?;
    if bytes.is_empty() {
        return Err(refused("That file is empty."));
    }
    let media_type = match kind {
        Kind::Image => picture_type(&bytes)
            .ok_or_else(|| refused("Only PNG, JPEG or WebP pictures can be shown."))?
            .to_owned(),
        _ => file_type(path).to_owned(),
    };
    Ok((media_type, bytes))
}

/// Turn a checked call into the thing to show, reading its file if it has one.
pub fn load(req: Request, roots: &PrivateRoots) -> Result<Attachment, ToolError> {
    let mut att = Attachment {
        kind: req.kind,
        title: req.title,
        media_type: None,
        data: None,
        markdown: None,
        url: None,
    };
    match req.source {
        Source::Markdown(m) => att.markdown = Some(m),
        Source::Url(u) => att.url = Some(u),
        Source::Path(raw) => {
            let path = admit(&raw, roots)?;
            let (media_type, bytes) = read(&path, req.kind)?;
            att.media_type = Some(media_type);
            att.data = Some(base64::engine::general_purpose::STANDARD.encode(bytes));
        }
    }
    Ok(att)
}

/// The things queued to go with the next answer, by chat.
#[derive(Default)]
pub struct Shown(Mutex<HashMap<String, Vec<Attachment>>>);

impl Shown {
    pub fn count(&self, chat: &str) -> usize {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(chat)
            .map_or(0, Vec::len)
    }

    /// Queue one more; `Err` once [`MAX_PER_REPLY`] are waiting.
    pub fn add(&self, chat: &str, a: Attachment) -> Result<usize, String> {
        let mut m = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let q = m.entry(chat.to_owned()).or_default();
        if q.len() >= MAX_PER_REPLY {
            return Err(format!(
                "{MAX_PER_REPLY} things are already queued for this answer; that is the most."
            ));
        }
        q.push(a);
        Ok(q.len())
    }

    /// Everything queued for the chat, oldest first, leaving nothing behind.
    pub fn take(&self, chat: &str) -> Vec<Attachment> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(chat)
            .unwrap_or_default()
    }

    /// Drop what is queued: the answer it was for is not going to the phone.
    pub fn clear(&self, chat: &str) {
        self.take(chat);
    }
}

/// Whether `caller` is the chat the voice screen talks to.
pub fn is_voice_chat(profile_dir: &Path, caller: &str) -> bool {
    voice::load_tag(&voice::tag_path(profile_dir)).is_some_and(|t| t.chat_id == caller)
}

fn show(ctx: &ToolCtx, a: Value) -> ToolResult {
    let profile = ctx.services.get::<Profile>();
    if !profile
        .as_ref()
        .is_some_and(|p| is_voice_chat(&p.dir, &ctx.caller_chat_id))
    {
        return Err(refused(
            "app_voice_show works only in the voice chat, the one the phone's voice screen talks to.",
        ));
    }
    let req = parse(&a)?;
    let shown = ctx
        .services
        .get::<Shown>()
        .ok_or_else(|| refused("Helm is still starting."))?;
    if shown.count(&ctx.caller_chat_id) >= MAX_PER_REPLY {
        return Err(refused(format!(
            "{MAX_PER_REPLY} things are already queued for this answer; that is the most."
        )));
    }
    let mut private = vec![];
    if let Some(p) = &profile {
        private.push(p.dir.clone());
    }
    if let Ok(d) = helm_core::profile::default_profile_dir() {
        private.push(d);
    }
    let att = load(req, &PrivateRoots::new(&private))?;
    let queued = shown.add(&ctx.caller_chat_id, att).map_err(refused)?;
    Ok(json!({ "queued": queued, "message": SHOWN }))
}

/// The `app_voice_show` host tool.
pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.blocking(
        ToolSpec::write(
            "app_voice_show",
            format!(
                "Put one thing on the screen of the user's phone with your next spoken answer. \
                 Only in the voice chat. Use it sparingly, only when hearing it would be worse: a \
                 table of numbers, a picture, a link to open. Always say in a sentence what you \
                 put on screen. At most {MAX_PER_REPLY} per answer. kind: image or file (give \
                 path: an absolute file of at most 1 MiB; pictures are PNG, JPEG or WebP), table, \
                 list or text (give markdown, GitHub style, at most {MAX_MARKDOWN} characters; a \
                 table at most {MAX_TABLE_ROWS} rows and {MAX_TABLE_COLUMNS} columns) or link \
                 (give url, https only). title: a short caption, at most {MAX_TITLE} characters. \
                 Too big or not allowed is refused with nothing shown; then say it is in the chat \
                 instead."
            ),
            json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["image", "table", "list", "link", "text", "file"]
                    },
                    "title": { "type": "string", "minLength": 1, "maxLength": MAX_TITLE },
                    "path": { "type": "string" },
                    "markdown": { "type": "string", "maxLength": MAX_MARKDOWN },
                    "url": { "type": "string", "maxLength": MAX_URL }
                },
                "required": ["kind", "title"],
                "additionalProperties": false
            }),
        )
        .large(),
        show,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: usize, cols: usize) -> String {
        let row = format!("|{}", " a |".repeat(cols));
        let divider = format!("|{}", "---|".repeat(cols));
        let mut out = vec![row.clone(), divider];
        out.extend((1..rows).map(|_| row.clone()));
        out.join("\n")
    }

    #[test]
    fn calls_are_checked_before_anything_is_read() {
        let long_title = "t".repeat(MAX_TITLE + 1);
        let long_md = "x".repeat(MAX_MARKDOWN + 1);
        let long_url = format!("https://example.com/{}", "a".repeat(MAX_URL));
        // (call, the refusal it gets)
        let bad: Vec<(Value, &str)> = vec![
            (json!("image"), "Expected an object"),
            (json!({"kind": "gif", "title": "t"}), "kind must be"),
            (json!({"title": "t", "path": "C:/a.png"}), "kind must be"),
            (
                json!({"kind": "text", "title": " ", "markdown": "m"}),
                "title is required",
            ),
            (
                json!({"kind": "text", "markdown": "m"}),
                "title is required",
            ),
            (
                json!({"kind": "text", "title": 4, "markdown": "m"}),
                "title must be text",
            ),
            (
                json!({"kind": "text", "title": long_title, "markdown": "m"}),
                "title is longer",
            ),
            (
                json!({"kind": "text", "title": "a\nb", "markdown": "m"}),
                "line breaks or invisible",
            ),
            (
                json!({"kind": "text", "title": "a\u{202e}b", "markdown": "m"}),
                "line breaks or invisible",
            ),
            (
                json!({"kind": "text", "title": "t", "markdown": "m", "x": 1}),
                "x is not a field",
            ),
            // One source per kind, and the right one.
            (
                json!({"kind": "image", "title": "t"}),
                "takes path and nothing else",
            ),
            (
                json!({"kind": "image", "title": "t", "path": "C:/a.png", "url": "https://a.b"}),
                "takes path",
            ),
            (
                json!({"kind": "image", "title": "t", "path": " "}),
                "takes path",
            ),
            (
                json!({"kind": "link", "title": "t", "markdown": "m"}),
                "takes url",
            ),
            (
                json!({"kind": "table", "title": "t", "path": "C:/a.png"}),
                "takes markdown",
            ),
            // Markdown.
            (
                json!({"kind": "list", "title": "t", "markdown": "  "}),
                "markdown is required",
            ),
            (
                json!({"kind": "text", "title": "t", "markdown": long_md}),
                "longer than 16000",
            ),
            (
                json!({"kind": "table", "title": "t", "markdown": "no table here"}),
                "needs markdown rows",
            ),
            (
                json!({"kind": "table", "title": "t", "markdown": table(51, 2)}),
                "at most 50 rows",
            ),
            (
                json!({"kind": "table", "title": "t", "markdown": table(2, 9)}),
                "8 columns",
            ),
            // Links are https and bounded.
            (
                json!({"kind": "link", "title": "t", "url": "http://example.com"}),
                "https://",
            ),
            (
                json!({"kind": "link", "title": "t", "url": "https://"}),
                "https://",
            ),
            (
                json!({"kind": "link", "title": "t", "url": "https://a b.com"}),
                "https://",
            ),
            (
                json!({"kind": "link", "title": "t", "url": "javascript:alert(1)"}),
                "https://",
            ),
            (
                json!({"kind": "link", "title": "t", "url": long_url}),
                "https://",
            ),
        ];
        for (call, says) in bad {
            let e = parse(&call).unwrap_err();
            assert!(e.public.contains(says), "{call} gave {}", e.public);
        }
        let good: Vec<(Value, Kind, Source)> = vec![
            (
                json!({"kind": "image", "title": " Chart ", "path": "C:/a.png"}),
                Kind::Image,
                Source::Path("C:/a.png".into()),
            ),
            (
                json!({"kind": "file", "title": "t", "path": "C:/a.pdf", "markdown": null}),
                Kind::File,
                Source::Path("C:/a.pdf".into()),
            ),
            (
                json!({"kind": "table", "title": "t", "markdown": table(50, 8)}),
                Kind::Table,
                Source::Markdown(table(50, 8)),
            ),
            (
                json!({"kind": "list", "title": "t", "markdown": "- a\n- b"}),
                Kind::List,
                Source::Markdown("- a\n- b".into()),
            ),
            (
                json!({"kind": "link", "title": "t", "url": "HTTPS://example.com/a?b=1"}),
                Kind::Link,
                Source::Url("HTTPS://example.com/a?b=1".into()),
            ),
        ];
        for (call, kind, source) in good {
            let r = parse(&call).unwrap();
            assert_eq!((r.kind, r.source), (kind, source), "{call}");
        }
        assert_eq!(
            parse(&json!({"kind": "image", "title": " Chart ", "path": "C:/a.png"}))
                .unwrap()
                .title,
            "Chart"
        );
    }

    #[test]
    fn files_are_admitted_read_and_sized_only_when_safe() {
        // Places and names secrets live in are never sent, whatever the file holds.
        for p in [
            r"D:\Home\a\.ssh\id_rsa",
            r"D:\Home\a\.aws\config",
            "/home/a/.claude/notes.txt",
            r"D:\Home\a\.config\x\y.json",
            r"C:\work\.env",
            r"C:\work\.env.local",
            r"C:\work\server.pem",
            r"C:\work\my credentials.txt",
            r"C:\work\Secrets.json",
        ] {
            assert!(sensitive(Path::new(p)), "{p}");
        }
        for p in [
            r"C:\work\report.pdf",
            "/home/a/notes/chart.png",
            r"C:\work\.github\x.md",
        ] {
            assert!(!sensitive(Path::new(p)), "{p}");
        }
        // The side panel's path rules apply first.
        let roots = PrivateRoots::new(&[]);
        for raw in [
            "relative.png",
            "//host/share/a.png",
            r"C:\a\..\b.png",
            r"C:\x\nul.png",
            r"D:\Home\a\.ssh\id_rsa",
        ] {
            assert!(admit(raw, &roots).is_err(), "{raw}");
        }

        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, b: &[u8]| {
            let p = dir.path().join(name);
            std::fs::write(&p, b).unwrap();
            p
        };
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend([0u8; 16]);
        let (mime, bytes) = read(&write("a.png", &png), Kind::Image).unwrap();
        assert_eq!((mime.as_str(), bytes.len()), ("image/png", png.len()));
        // The first bytes decide for a picture, not the name; other types are not pictures.
        assert_eq!(
            read(&write("b.dat", &png), Kind::Image).unwrap().0,
            "image/png"
        );
        assert_eq!(
            read(&write("c.jpg", &[0xff, 0xd8, 0xff, 0xe0, 0]), Kind::Image)
                .unwrap()
                .0,
            "image/jpeg"
        );
        assert_eq!(
            read(&write("d.webp", b"RIFF....WEBPVP8 "), Kind::Image)
                .unwrap()
                .0,
            "image/webp"
        );
        let not_pictures: [(&str, &[u8]); 3] = [
            ("e.png", b"not a picture"),
            ("f.gif", b"GIF89a...."),
            ("g.png", b""),
        ];
        for (name, body) in not_pictures {
            assert!(read(&write(name, body), Kind::Image).is_err(), "{name}");
        }
        // A file is any type up to 1 MiB; its type comes from its name.
        assert_eq!(
            read(&write("h.pdf", b"%PDF-1"), Kind::File).unwrap().0,
            "application/pdf"
        );
        assert_eq!(
            read(&write("i.bin", b"xx"), Kind::File).unwrap().0,
            "application/octet-stream"
        );
        let exact = vec![1u8; MAX_BYTES as usize];
        assert!(read(&write("j.bin", &exact), Kind::File).is_ok());
        let over = vec![1u8; MAX_BYTES as usize + 1];
        let e = read(&write("k.bin", &over), Kind::File).unwrap_err();
        assert!(e.public.contains("too big to show"), "{}", e.public);
        assert!(read(&dir.path().join("missing.png"), Kind::Image)
            .unwrap_err()
            .public
            .contains("not found"));

        // A request with its content in it needs no disk.
        let att = load(
            parse(&json!({"kind": "link", "title": "Docs", "url": "https://example.com"})).unwrap(),
            &roots,
        )
        .unwrap();
        assert_eq!(att.url.as_deref(), Some("https://example.com"));
        assert!(att.data.is_none() && att.markdown.is_none());
    }

    #[test]
    fn what_is_shown_waits_in_a_queue_of_four_and_goes_up_as_numbered_frames() {
        let item = |t: &str| Attachment {
            kind: Kind::Image,
            title: t.into(),
            media_type: Some("image/png".into()),
            data: Some("AAAA".into()),
            markdown: None,
            url: None,
        };
        let q = Shown::default();
        for i in 1..=MAX_PER_REPLY {
            assert_eq!(q.add("voice", item(&format!("n{i}"))), Ok(i));
        }
        assert!(q.add("voice", item("n5")).unwrap_err().contains("most"));
        assert_eq!(q.count("other"), 0, "each chat has its own queue");
        let taken = q.take("voice");
        assert_eq!(
            taken.iter().map(|a| a.title.as_str()).collect::<Vec<_>>(),
            ["n1", "n2", "n3", "n4"]
        );
        assert_eq!(q.count("voice"), 0);
        q.add("voice", item("again")).unwrap();
        q.clear("voice");
        assert!(q.take("voice").is_empty());

        assert_eq!(
            taken[0].frame("r1", 1),
            json!({"type": "voice_attachment", "reply": "r1", "n": 1, "kind": "image",
                   "title": "n1", "mediaType": "image/png", "data": "AAAA"})
        );
        let link = Attachment {
            kind: Kind::Link,
            title: "Docs".into(),
            media_type: None,
            data: None,
            markdown: None,
            url: Some("https://example.com".into()),
        };
        assert_eq!(
            link.frame("r1", 2),
            json!({"type": "voice_attachment", "reply": "r1", "n": 2, "kind": "link",
                   "title": "Docs", "url": "https://example.com"})
        );

        // Only the chat the voice screen talks to may call the tool.
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_voice_chat(dir.path(), "c1"), "no voice chat yet");
        let tag = voice::VoiceTag {
            chat_id: "c1".into(),
            started_at: 0,
            last_turn_at: 0,
            tokens_at_start: 0,
            conversation: "x".into(),
            primed: true,
        };
        voice::save_tag(&voice::tag_path(dir.path()), Some(&tag)).unwrap();
        assert!(is_voice_chat(dir.path(), "c1"));
        assert!(!is_voice_chat(dir.path(), "c2"));
    }
}
