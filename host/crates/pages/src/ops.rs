//! The `api` ops behind sealed pages.
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `page_open` | heavy | `{path, chatId?}` → the session (`session`, `channel`, `url`, `path`, `key`, `title`, `network`, `scripts`, `revision`, `size`) plus `state`, `stateRevision` and `pins`; `network` is the host's own decision — on when the page is pinned for the chat's profile, off otherwise |
//! | `page_reload` | heavy | `{session}` → the session again, re-read |
//! | `page_close` | blocking | `{session}` → `{closed}` |
//! | `page_status` | blocking | `{session}` → `{revision, changed, missing, error?}`: the file watch, polled by the open frame |
//! | `page_state_save` | blocking | `{session, state, revision}` → `{revision}`; a stale revision is a `conflict` |
//! | `page_state_reset` | blocking | `{session}` → `{revision: 0}` |
//! | `page_pin` | blocking | `{path, title?, chatId?, profileId?}` → `{page, created}` |
//! | `page_unpin` | blocking | `{id}` → `{removed}` |
//! | `page_link_chat` | blocking | `{id, chatId?}` → `{linked}` |
//! | `page_list` | blocking | `{profileId?, chatId?}` → `{pages}` (a chat's profile stands in for `profileId`) |
//! | `page_icon` | blocking | `{path}` → `{icon}`: a data URL of the page bundle's own favicon (a `<link rel=icon>` file beside it, else `favicon.ico`/`.png`/`.svg` there, at most 64 KiB), or `null` |
//!
//! The answers are built as JSON here, and the UI's copy of them (`ui/src/pages/`) is typed by hand
//! from this table: there is no Rust type to generate bindings from.
//!
//! These answer the main window and the host's own tools; a page itself can call none of them,
//! and it never names a session, a key or a path: the open frame's bridge quotes the session it
//! was given, and the session fixes which page's state a save touches.

use crate::server::{PageError, PageServer};
use crate::store::{self, PinError, SaveError};
use crate::ChatProfileLookup;
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
use rusqlite::OptionalExtension;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("page_open", OpClass::Heavy, page_open)
        .blocking("page_reload", OpClass::Heavy, page_reload)
        .blocking("page_close", OpClass::Blocking, page_close)
        .blocking("page_status", OpClass::Blocking, page_status)
        .blocking("page_state_save", OpClass::Blocking, page_state_save)
        .blocking("page_state_reset", OpClass::Blocking, page_state_reset)
        .blocking("page_pin", OpClass::Blocking, page_pin)
        .blocking("page_unpin", OpClass::Blocking, page_unpin)
        .blocking("page_link_chat", OpClass::Blocking, page_link_chat)
        .blocking("page_list", OpClass::Blocking, page_list)
        .blocking("page_icon", OpClass::Blocking, page_icon);
}

fn parse<T: DeserializeOwned>(args: Value) -> Result<T, ApiError> {
    let args = if args.is_null() { json!({}) } else { args };
    serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))
}

fn page_error(e: PageError) -> ApiError {
    match e {
        PageError::Refused(m) => ApiError::new("refused", m),
        PageError::NotFound(m) => ApiError::new("not_found", m),
        PageError::Io(m) => ApiError::internal(m),
    }
}

fn internal(e: impl ToString) -> ApiError {
    ApiError::internal(e.to_string())
}

/// The folder of the project this chat runs in, if it has one.
fn chat_folder(db: &Database, chat_id: &str) -> Result<Option<String>, ApiError> {
    db.snapshot(|tx| {
        tx.query_row(
            "SELECT p.path FROM chats c JOIN projects p ON p.id = c.project_id WHERE c.id = ?1",
            [chat_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
    })
    .map(Option::flatten)
    .map_err(internal)
}

fn chat_exists(db: &Database, chat_id: &str) -> Result<bool, ApiError> {
    db.snapshot(|tx| {
        tx.query_row("SELECT 1 FROM chats WHERE id = ?1", [chat_id], |_| Ok(()))
            .optional()
    })
    .map(|r| r.is_some())
    .map_err(internal)
}

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(Into::into)
}

/// The absolute form of a written path: `~`, a drive path, or a path relative to the chat's
/// project folder. Nothing touches the disk here; the server's own gate does that.
fn absolute(ctx: &OpCtx, raw: &str, chat_id: Option<&str>) -> Result<String, ApiError> {
    let folder = match chat_id {
        Some(id) => {
            let db = ctx.services.need::<Database>()?;
            chat_folder(&db, id)?.map(std::path::PathBuf::from)
        }
        None => None,
    };
    helm_panel::resolve::to_absolute(raw, home_dir().as_deref(), folder.as_deref())
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|m| ApiError::new("refused", m))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct OpenArgs {
    path: String,
    #[serde(default)]
    chat_id: Option<String>,
}

/// The profile an op acts for: the chat's own, or the unassigned one when no chat is named.
fn acting_profile(ctx: &OpCtx, chat_id: Option<&str>) -> String {
    chat_id
        .and_then(|c| ctx.services.get::<ChatProfileLookup>()?.0.profile_of(c))
        .unwrap_or_default()
}

fn page_open(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: OpenArgs = parse(args)?;
    let server = ctx.services.need::<PageServer>()?;
    let db = ctx.services.need::<Database>()?;
    let path = absolute(ctx, &a.path, a.chat_id.as_deref())?;
    // The network follows the pin, not the sender: a page pinned for the acting profile runs
    // online; anything else stays sealed offline. The UI has no say.
    let checked = server.check(&path).map_err(page_error)?;
    let profile = acting_profile(ctx, a.chat_id.as_deref());
    let network = db
        .snapshot(|tx| store::is_pinned_for(tx, &profile, &checked.key))
        .map_err(internal)?;
    let opened = server.open(&path, network).map_err(page_error)?;
    let (state, revision, pins) = db
        .snapshot(|tx| {
            let (s, r) = store::state_get(tx, &opened.key)?;
            Ok::<_, rusqlite::Error>((s, r, store::pins_for_key(tx, &opened.key)?))
        })
        .map_err(internal)?;
    let mut out = opened.to_json();
    out["state"] = state;
    out["stateRevision"] = json!(revision);
    out["pins"] = json!(pins);
    Ok(out)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SessionArgs {
    session: String,
}

fn page_reload(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SessionArgs = parse(args)?;
    let server = ctx.services.need::<PageServer>()?;
    Ok(server.reload(&a.session).map_err(page_error)?.to_json())
}

fn page_close(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SessionArgs = parse(args)?;
    let server = ctx.services.need::<PageServer>()?;
    Ok(json!({ "closed": server.close(&a.session) }))
}

fn page_status(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SessionArgs = parse(args)?;
    let server = ctx.services.need::<PageServer>()?;
    let s = server.status(&a.session).map_err(page_error)?;
    Ok(json!({
        "revision": s.revision,
        "changed": s.changed,
        "missing": s.missing,
        "error": s.error,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SaveArgs {
    session: String,
    state: Value,
    revision: i64,
}

fn page_state_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SaveArgs = parse(args)?;
    let server = ctx.services.need::<PageServer>()?;
    let db = ctx.services.need::<Database>()?;
    // Saves of one open are spaced out; a page that saves on every keystroke waits its turn.
    let (key, wait) = server.save_turn(&a.session).map_err(page_error)?;
    if !wait.is_zero() {
        std::thread::sleep(wait);
    }
    let revision = db
        .write(|tx| store::state_save(tx, &key, &a.state, a.revision, helm_core::now_ms()))
        .map_err(|e| match e {
            SaveError::TooLarge => ApiError::bad_args("App state exceeds 256 KiB."),
            SaveError::Stale => ApiError::conflict(
                "The page's saved state changed elsewhere. Reload the page before saving.",
            ),
            SaveError::Db(m) => ApiError::internal(m),
        })?;
    Ok(json!({ "revision": revision }))
}

fn page_state_reset(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SessionArgs = parse(args)?;
    let server = ctx.services.need::<PageServer>()?;
    let db = ctx.services.need::<Database>()?;
    let key = server
        .key_of(&a.session)
        .ok_or_else(|| ApiError::new("not_found", "That page is no longer open."))?;
    db.write(|tx| store::state_reset(tx, &key))
        .map_err(internal)?;
    Ok(json!({ "revision": 0 }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PinArgs {
    path: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    chat_id: Option<String>,
    #[serde(default)]
    profile_id: Option<String>,
}

fn changed(ctx: &OpCtx) {
    ctx.events.emit(AppEvent::new("pages_changed"));
}

fn page_pin(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: PinArgs = parse(args)?;
    let server = ctx.services.need::<PageServer>()?;
    let db = ctx.services.need::<Database>()?;
    if let Some(chat) = &a.chat_id {
        if !chat_exists(&db, chat)? {
            return Err(ApiError::new("not_found", "That chat no longer exists."));
        }
    }
    let path = absolute(ctx, &a.path, a.chat_id.as_deref())?;
    let checked = server.check(&path).map_err(page_error)?;
    let profile = a
        .profile_id
        .clone()
        .unwrap_or_else(|| acting_profile(ctx, a.chat_id.as_deref()));
    let id = uuid::Uuid::new_v4().simple().to_string();
    let shown = checked.path.to_string_lossy().into_owned();
    let (page, created) = db
        .write(|tx| {
            store::pin(
                tx,
                &id,
                &profile,
                &checked.key,
                &shown,
                a.title.as_deref(),
                a.chat_id.as_deref(),
                helm_core::now_ms(),
            )
        })
        .map_err(|e| match e {
            PinError::TooMany => ApiError::new(
                "refused",
                format!(
                    "At most {} pages can be pinned. Unpin one first.",
                    store::MAX_PINS
                ),
            ),
            PinError::Db(m) => ApiError::internal(m),
        })?;
    changed(ctx);
    Ok(json!({ "page": page, "created": created }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct IdArgs {
    id: String,
}

fn page_unpin(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: IdArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let removed = db.write(|tx| store::unpin(tx, &a.id)).map_err(internal)?;
    if removed {
        changed(ctx);
    }
    Ok(json!({ "removed": removed }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LinkArgs {
    id: String,
    #[serde(default)]
    chat_id: Option<String>,
}

fn page_link_chat(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: LinkArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    if let Some(chat) = &a.chat_id {
        if !chat_exists(&db, chat)? {
            return Err(ApiError::new("not_found", "That chat no longer exists."));
        }
    }
    let linked = db
        .write(|tx| store::link_chat(tx, &a.id, a.chat_id.as_deref()))
        .map_err(internal)?;
    if linked {
        changed(ctx);
    }
    Ok(json!({ "linked": linked }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ListArgs {
    #[serde(default)]
    profile_id: Option<String>,
    #[serde(default)]
    chat_id: Option<String>,
}

fn page_list(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ListArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let profile = a.profile_id.or_else(|| {
        let chat = a.chat_id.as_deref()?;
        ctx.services.get::<ChatProfileLookup>()?.0.profile_of(chat)
    });
    let pages = db
        .snapshot(|tx| store::list(tx, profile.as_deref()))
        .map_err(internal)?;
    Ok(json!({ "pages": pages }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct IconArgs {
    path: String,
}

/// The largest favicon the list will show.
const MAX_ICON_BYTES: usize = 64 * 1024;
/// Plain favicon names a page bundle may carry beside its document, in the order they are tried
/// when the document names none.
const FALLBACK_ICONS: [&str; 3] = ["favicon.ico", "favicon.png", "favicon.svg"];

fn page_icon(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: IconArgs = parse(args)?;
    let server = ctx.services.need::<PageServer>()?;
    let path = absolute(ctx, &a.path, None)?;
    Ok(json!({ "icon": favicon_of(&server, &path)? }))
}

/// The page's own favicon as a data URL: the file its `<link rel=icon>` names beside it, else a
/// plain `favicon.*` there. Each candidate goes through the same asset gate a page request would,
/// so nothing outside the page's own folder is read. `None` when there is no local icon, the icon
/// is not an image type, or it is larger than [`MAX_ICON_BYTES`].
fn favicon_of(server: &PageServer, path: &str) -> Result<Option<String>, ApiError> {
    let doc = server.bundle_read(path, None).map_err(page_error)?;
    let html = String::from_utf8_lossy(&doc);
    let mut candidates: Vec<String> = Vec::new();
    if let Some(rel) = link_icon_href(&html) {
        candidates.push(rel);
    }
    candidates.extend(FALLBACK_ICONS.map(str::to_owned));
    for rel in candidates {
        let bytes = match server.bundle_read(path, Some(&rel)) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if bytes.is_empty() || bytes.len() > MAX_ICON_BYTES {
            continue;
        }
        let ext = rel
            .rsplit(['/', '\\'])
            .next()
            .and_then(|n| n.rsplit_once('.'))
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        let Some(mime) = crate::server::asset_mime(&ext) else {
            continue;
        };
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
        return Ok(Some(format!("data:{mime};base64,{b64}")));
    }
    Ok(None)
}

/// The `href` of the page's own `<link rel="icon">` (or `rel="shortcut icon"`) when it names a
/// file beside the page: a bare or quoted relative path, no scheme, no host, no absolute path.
/// Absolute web addresses, `data:` hrefs and missing links give `None` — the fallback names are
/// still tried.
fn link_icon_href(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut pos = 0;
    while let Some(off) = lower[pos..].find("<link") {
        let start = pos + off;
        let Some(end_off) = lower[start..].find('>') else {
            break;
        };
        pos = start + end_off + 1;
        let tag = &html[start..=start + end_off];
        let rel = attr_value(tag, "rel").map(|v| v.to_ascii_lowercase());
        if !matches!(rel.as_deref(), Some("icon") | Some("shortcut icon")) {
            continue;
        }
        let href = attr_value(tag, "href")?;
        let href = href.trim();
        if !href.is_empty()
            && !href.contains(':')
            && !href.starts_with('/')
            && !href.starts_with('\\')
        {
            return Some(href.to_owned());
        }
    }
    None
}

/// The value of an attribute in a tag: quoted or bare, matched case-insensitively on the name
/// but keeping the value's own spelling (asset names can be case-sensitive).
fn attr_value(tag: &str, name: &str) -> Option<String> {
    let tag_l = tag.to_ascii_lowercase();
    let name_l = name.to_ascii_lowercase();
    let mut pos = 0;
    while let Some(off) = tag_l[pos..].find(&name_l) {
        let at = pos + off;
        pos = at + name_l.len();
        // An attribute starts after whitespace and is followed by `=`.
        if !tag[..at].ends_with(char::is_whitespace) {
            continue;
        }
        let rest = &tag[at + name_l.len()..];
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let value = if let Some(r) = rest.strip_prefix('"') {
            let end = r.find('"')?;
            &r[..end]
        } else if let Some(r) = rest.strip_prefix('\'') {
            let end = r.find('\'')?;
            &r[..end]
        } else {
            let ws = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let gt = rest.find('>').unwrap_or(rest.len());
            &rest[..ws.min(gt)]
        };
        return Some(value.to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChatProfiles;
    use helm_core::db::MigrationRegistry;
    use helm_ipc::{RecordingSink, Services};
    use helm_panel::private::PrivateRoots;
    use std::sync::Arc;

    struct Fixed;
    impl ChatProfiles for Fixed {
        fn profile_of(&self, chat_id: &str) -> Option<String> {
            (chat_id == "c1").then(|| "profile-a".to_owned())
        }
    }

    /// The ops end to end over a real database: open, save with the revision rule, reset, the
    /// watch, pinning from a chat with a relative path, listing by profile, linking and closing.
    #[test]
    fn a_page_opens_saves_is_pinned_and_closes_through_the_ops() {
        let files = tempfile::tempdir().unwrap();
        std::fs::write(files.path().join("plan.html"), "<p>plan</p>").unwrap();
        std::fs::write(files.path().join("notes.txt"), "no").unwrap();
        let mut reg = MigrationRegistry::new();
        reg.initializer("pages", store::init);
        let dbdir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&dbdir.path().join("t.sqlite3"), &reg).unwrap());
        let folder = files.path().to_string_lossy().into_owned();
        db.write(|tx| {
            tx.execute(
                "INSERT INTO projects (id, name, path, created_at) VALUES ('p', 'p', ?1, 0)",
                [&folder],
            )?;
            tx.execute("INSERT INTO chats (id, project_id, created_at, updated_at) VALUES ('c1', 'p', 0, 0)", [])
        })
        .unwrap();
        let events = Arc::new(RecordingSink::default());
        let mut services = Services::new();
        services.insert(db.clone());
        services.insert(Arc::new(PageServer::new(PrivateRoots::default())));
        services.insert(Arc::new(ChatProfileLookup(Arc::new(Fixed))));
        let ctx = OpCtx {
            services: Arc::new(services),
            events: events.clone(),
        };
        let abs = files
            .path()
            .join("plan.html")
            .to_string_lossy()
            .into_owned();
        let code = |r: Result<Value, ApiError>| r.unwrap_err().code;

        // Open: a session, no saved state yet, nothing pinned — so offline, with no say from the
        // sender: asking for the network in the open is refused.
        let o = page_open(&ctx, json!({"path": abs})).unwrap();
        let session = o["session"].as_str().unwrap().to_owned();
        assert_eq!(
            (
                o["network"].clone(),
                o["state"].clone(),
                o["stateRevision"].clone()
            ),
            (json!(false), Value::Null, json!(0))
        );
        assert_eq!(o["pins"], json!([]));
        assert_eq!(
            code(page_open(&ctx, json!({"path": abs, "network": true}))),
            "bad_args"
        );
        assert!(o["url"].as_str().unwrap().contains(&session));
        // Other files, unknown fields and a path with no folder to resolve against are refused.
        assert_eq!(
            code(page_open(
                &ctx,
                json!({"path": files.path().join("notes.txt").to_string_lossy()})
            )),
            "refused"
        );
        assert_eq!(
            code(page_open(&ctx, json!({"path": abs, "extra": 1}))),
            "bad_args"
        );
        assert_eq!(
            code(page_open(&ctx, json!({"path": "plan.html"}))),
            "refused"
        );

        // Saving follows the revision; a second view quoting the old one is told to reload.
        let save = |s: &str, v: Value, r: i64| {
            page_state_save(&ctx, json!({"session": s, "state": v, "revision": r}))
        };
        assert_eq!(
            save(&session, json!({"folded": ["a"]}), 0).unwrap()["revision"],
            1
        );
        assert_eq!(code(save(&session, json!(1), 0)), "conflict");
        assert_eq!(code(save("nope", json!(1), 0)), "not_found");
        let again = page_open(&ctx, json!({"path": abs})).unwrap();
        assert_eq!(
            (again["state"].clone(), again["stateRevision"].clone()),
            (json!({"folded": ["a"]}), json!(1))
        );
        assert_eq!(
            code(save(&session, Value::String("x".repeat(300_000)), 1)),
            "bad_args"
        );

        // The watch sees a rewrite; reset empties the state.
        std::fs::write(files.path().join("plan.html"), "<p>plan v2</p>").unwrap();
        assert_eq!(
            page_status(&ctx, json!({"session": session})).unwrap()["changed"],
            true
        );
        assert_eq!(
            page_reload(&ctx, json!({"session": session})).unwrap()["revision"],
            3
        );
        page_state_reset(&ctx, json!({"session": session})).unwrap();
        assert_eq!(
            page_open(&ctx, json!({"path": abs})).unwrap()["state"],
            Value::Null
        );

        // A pin for another profile keeps this chat's open offline; the network follows the
        // acting profile's own pin.
        let foreign = page_pin(&ctx, json!({"path": abs, "profileId": "profile-b"})).unwrap();
        assert_eq!(
            page_open(&ctx, json!({"path": abs, "chatId": "c1"})).unwrap()["network"],
            false
        );
        page_unpin(&ctx, json!({"id": foreign["page"]["id"]})).unwrap();

        // Pin from the chat with a path relative to its project; the chat's profile files it.
        let pinned = page_pin(
            &ctx,
            json!({"path": "plan.html", "title": "Plan", "chatId": "c1"}),
        )
        .unwrap();
        assert_eq!(pinned["created"], true);
        assert_eq!(
            (
                pinned["page"]["profileId"].as_str(),
                pinned["page"]["chatId"].as_str()
            ),
            (Some("profile-a"), Some("c1"))
        );
        let id = pinned["page"]["id"].as_str().unwrap().to_owned();
        assert_eq!(
            page_pin(&ctx, json!({"path": "plan.html", "chatId": "c1"})).unwrap()["created"],
            false
        );
        assert_eq!(
            code(page_pin(
                &ctx,
                json!({"path": "plan.html", "chatId": "gone"})
            )),
            "not_found"
        );
        assert_eq!(
            code(page_pin(&ctx, json!({"path": "notes.txt", "chatId": "c1"}))),
            "refused"
        );
        let titles = |a: Value| -> Vec<String> {
            page_list(&ctx, a).unwrap()["pages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["title"].as_str().unwrap().to_owned())
                .collect()
        };
        assert_eq!(titles(json!({})), ["Plan"]);
        assert_eq!(titles(json!({"chatId": "c1"})), ["Plan"]);
        assert!(titles(json!({"profileId": "profile-b"})).is_empty());
        // Opening a pinned page reports its pin and runs online: the pin for the chat's profile
        // decides, and an open with no chat (from the Pages list) counts any pin.
        let pinned_open = page_open(&ctx, json!({"path": abs, "chatId": "c1"})).unwrap();
        assert_eq!(pinned_open["pins"][0]["id"], json!(id));
        assert_eq!(pinned_open["network"], true);
        assert_eq!(
            page_open(&ctx, json!({"path": abs})).unwrap()["network"],
            true
        );
        assert_eq!(
            page_link_chat(&ctx, json!({"id": id})).unwrap()["linked"],
            true
        );
        assert_eq!(
            page_list(&ctx, json!({})).unwrap()["pages"][0]["chatId"],
            Value::Null
        );
        assert_eq!(
            page_unpin(&ctx, json!({"id": id})).unwrap()["removed"],
            true
        );
        assert_eq!(
            page_unpin(&ctx, json!({"id": id})).unwrap()["removed"],
            false
        );

        // Closing ends the session for every op that needs it.
        assert_eq!(
            page_close(&ctx, json!({"session": session})).unwrap()["closed"],
            true
        );
        assert_eq!(code(save(&session, json!(1), 0)), "not_found");
        assert_eq!(
            code(page_status(&ctx, json!({"session": session}))),
            "not_found"
        );
        assert!(events.kinds().iter().all(|k| k == "pages_changed"));
        assert!(!events.kinds().is_empty());
    }

    /// The list's icon: the file the page's own `<link rel=icon>` names beside it, else a plain
    /// `favicon.*` there; web addresses, missing files, unlisted types and anything over 64 KiB
    /// give nothing.
    #[test]
    fn a_page_icon_is_the_bundled_favicon_capped_or_nothing() {
        let files = tempfile::tempdir().unwrap();
        let dir = files.path();
        std::fs::create_dir(dir.join("assets")).unwrap();
        std::fs::write(dir.join("assets").join("i.svg"), "<svg/>").unwrap();
        std::fs::write(dir.join("favicon.ico"), [0u8; 8]).unwrap();
        std::fs::write(
            dir.join("p1.html"),
            "<head><link rel=\"shortcut icon\" href=\"assets/i.svg\"></head><p>x</p>",
        )
        .unwrap();
        std::fs::write(dir.join("p2.html"), "<p>no link</p>").unwrap();
        std::fs::write(
            dir.join("p3.html"),
            "<link rel=icon href=\"gone.png\"><p>3</p>",
        )
        .unwrap();
        let plain = dir.join("plain");
        std::fs::create_dir(&plain).unwrap();
        std::fs::write(
            plain.join("p4.html"),
            "<link rel=\"icon\" href=\"https://example.com/i.ico\"><p>4</p>",
        )
        .unwrap();
        std::fs::write(plain.join("p6.html"), "<p>big</p>").unwrap();
        std::fs::write(plain.join("favicon.png"), vec![7u8; MAX_ICON_BYTES + 1]).unwrap();
        let p = |n: &str| dir.join(n).to_string_lossy().into_owned();
        let events = Arc::new(RecordingSink::default());
        let mut services = Services::new();
        services.insert(Arc::new(PageServer::new(PrivateRoots::default())));
        let ctx = OpCtx {
            services: Arc::new(services),
            events,
        };
        let icon = |a: Value| page_icon(&ctx, a).unwrap()["icon"].clone();

        assert_eq!(
            icon(json!({"path": p("p1.html")})).as_str().unwrap(),
            format!(
                "data:image/svg+xml;base64,{}",
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"<svg/>")
            ),
            "the page's own link comes first, with its folder"
        );
        assert!(
            icon(json!({"path": p("p2.html")}))
                .as_str()
                .unwrap()
                .starts_with("data:image/x-icon;base64,"),
            "favicon.ico beside the page"
        );
        assert!(
            icon(json!({"path": p("p3.html")}))
                .as_str()
                .unwrap()
                .starts_with("data:image/x-icon;base64,"),
            "a missing named icon falls back to the plain name"
        );
        assert!(
            icon(json!({"path": p("plain/p4.html")})).is_null(),
            "a web address is not a local file"
        );
        assert!(
            icon(json!({"path": p("plain/p6.html")})).is_null(),
            "an icon over 64 KiB is nothing"
        );
        assert_eq!(
            page_icon(&ctx, json!({"path": p("none.html")}))
                .unwrap_err()
                .code,
            "not_found"
        );
        std::fs::write(dir.join("s.txt"), "not a page").unwrap();
        assert_eq!(
            page_icon(&ctx, json!({"path": p("s.txt")}))
                .unwrap_err()
                .code,
            "refused"
        );
    }

    #[test]
    fn every_op_registers_once() {
        let mut reg = OpRegistry::new();
        register_ops(&mut reg);
        assert_eq!(reg.names().count(), 11);
        assert!(reg.names().all(|n| n.starts_with("page_")));
    }
}
