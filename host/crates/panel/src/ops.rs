//! The side panel's `api` ops (panel crate rows).
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `file_read` / `dir_list` | heavy | `{chatId?, path}` → [`FileView`](crate::viewer::FileView) |
//! | `default_handler` | blocking | `{path}` → `{handler: {name, appPath} \| null, refusal: {step, reason} \| null}` |
//! | `file_open_default` | maintenance | `{path}` → `null`: re-runs every check, then opens |
//! | `file_reveal` | maintenance | `{path}` → `null`: Explorer with the item selected |
//! | `chat_folder` | blocking | `{chatId}` → `{path \| null}`: the chat's project folder |
//! | `path_resolve` | blocking | `{chatId?, path}` → `{path, kind: "file" \| "dir"}`: a path written in a reply |
//! | `quick_open` | heavy | `{chatId?, query, reload?}` → `{mode: "path" \| "files", items, partial}` |
//! | `changes` | heavy | `{chatId}` → `{repoTop, branch, records, truncated}` |
//! | `diff` | heavy | `{chatId, path, side: "head" \| "staged" \| "unstaged", from?}` → `{patch, fullContext, truncated}` |
//! | `changes_stage` / `changes_unstage` | blocking (own try-lock) | `{chatId, path, from?}` → `{changes, noop}` |
//! | `vault_list` | blocking | `{}` → `VaultInfo[]` (Obsidian's vaults plus the user's) |
//! | `vault_add` / `vault_remove` / `vault_set_default` | maintenance | `{path}` / `{path}` / `{name}` → `VaultInfo[]` |
//! | `vault_resolve` | blocking | `{chatId?, note, vault?, fromVault?, fromPath?}` → `{vault, path}` or `{vault, candidates}` |
//! | `vault_read` | heavy | `{vault, path}` → `{vault, path, title, text?, bytes?, mime?, truncated}` |
//! | `vault_open_in_obsidian` | maintenance | `{vault, path}` → `null`: the `obsidian://` URL is built here |
//! | `panel_context_get` / `panel_context_set` | blocking / maintenance | `{chatId}` → `{on}` / `{chatId, on}` → `{on}` (user-only) |
//! | `review_comment_add` | maintenance | `{chatId, id, path, diffSide, side, startLine, endLine, body, fingerprint}` → comment |
//! | `review_comment_list` / `_remove` / `_clear` | blocking / maintenance / maintenance | `{chatId}` / `{chatId, id}` / `{chatId}` |
//! | `panel_tabs_get` / `panel_tabs_save` | blocking / maintenance | `{}` → `panelTabs` v1 / `{value}` → `null` |
//!
//! Arguments are parsed strictly. Refusals come back as `{code: "refused", message}`; the message
//! is written for the user. Every path passes the lexical path gate before any filesystem call.

use crate::handler::{self, Decision};
use crate::private::PrivateRoots;
use crate::viewer::{self, ReadError};
use crate::{changes, comments, context, git, pathgate, quick, resolve, tabs, vault};
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use rusqlite::OptionalExtension;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("file_read", OpClass::Heavy, file_read)
        .blocking("dir_list", OpClass::Heavy, file_read)
        .blocking("default_handler", OpClass::Blocking, default_handler)
        .blocking("file_open_default", OpClass::Maintenance, file_open_default)
        .blocking("file_reveal", OpClass::Maintenance, file_reveal)
        .blocking("chat_folder", OpClass::Blocking, chat_folder)
        .blocking("path_resolve", OpClass::Blocking, path_resolve)
        .blocking("quick_open", OpClass::Heavy, quick_open)
        .blocking("vault_list", OpClass::Blocking, vault_list)
        .blocking("vault_add", OpClass::Maintenance, vault_add)
        .blocking("vault_remove", OpClass::Maintenance, vault_remove)
        .blocking("vault_set_default", OpClass::Maintenance, vault_set_default)
        .blocking("vault_resolve", OpClass::Blocking, vault_resolve)
        .blocking("vault_read", OpClass::Heavy, vault_read)
        .blocking("vault_open_in_obsidian", OpClass::Maintenance, vault_open)
        .blocking("panel_context_get", OpClass::Blocking, context_get)
        .blocking("panel_context_set", OpClass::Maintenance, context_set)
        .blocking("review_comment_add", OpClass::Maintenance, comment_add)
        .blocking("review_comment_list", OpClass::Blocking, comment_list)
        .blocking(
            "review_comment_remove",
            OpClass::Maintenance,
            comment_remove,
        )
        .blocking("review_comment_clear", OpClass::Maintenance, comment_clear)
        .blocking("changes", OpClass::Heavy, changes_op)
        .blocking("diff", OpClass::Heavy, diff_op)
        // Staging takes its own process-wide try-lock, never the maintenance lock or a read permit.
        .blocking("changes_stage", OpClass::Blocking, stage_op)
        .blocking("changes_unstage", OpClass::Blocking, unstage_op)
        .blocking("panel_tabs_get", OpClass::Blocking, panel_tabs_get)
        .blocking("panel_tabs_save", OpClass::Maintenance, panel_tabs_save);
}

fn parse<T: DeserializeOwned>(args: Value) -> Result<T, ApiError> {
    let args = if args.is_null() { json!({}) } else { args };
    serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))
}

pub fn refused(message: impl Into<String>) -> ApiError {
    ApiError::new("refused", message)
}

fn read_error(e: ReadError) -> ApiError {
    match e {
        ReadError::NotFound => ApiError::new("not_found", e.message()),
        ReadError::Io(m) => ApiError::internal(m),
        other => refused(other.message()),
    }
}

/// The profile folder (data, sessions, backups, caches) and the default one, whichever is in use.
pub fn private_roots(ctx: &OpCtx) -> PrivateRoots {
    let mut roots = Vec::new();
    if let Some(p) = ctx.services.get::<Profile>() {
        roots.push(p.dir.clone());
    }
    if let Ok(d) = helm_core::profile::default_profile_dir() {
        roots.push(d);
    }
    PrivateRoots::new(&roots)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PathArgs {
    #[serde(default)]
    #[allow(dead_code)]
    chat_id: Option<String>,
    path: String,
}

fn file_read(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: PathArgs = parse(args)?;
    let view = viewer::read(&a.path, &private_roots(ctx)).map_err(read_error)?;
    serde_json::to_value(view).map_err(|e| ApiError::internal(e.to_string()))
}

fn default_handler(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: PathArgs = parse(args)?;
    let trusted = handler::trusted_roots();
    Ok(
        match handler::check(&a.path, &private_roots(ctx), &trusted, &handler::os_lookup) {
            Ok(Decision::Folder(_)) => {
                json!({"handler": {"name": "File Explorer", "appPath": ""}, "refusal": null})
            }
            Ok(Decision::File(_, h)) => json!({"handler": h, "refusal": null}),
            Err(r) => json!({"handler": null, "refusal": r}),
        },
    )
}

fn file_open_default(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: PathArgs = parse(args)?;
    let trusted = handler::trusted_roots();
    let decision = handler::check(&a.path, &private_roots(ctx), &trusted, &handler::os_lookup)
        .map_err(|r| refused(r.reason().to_owned()))?;
    handler::open(&decision).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Value::Null)
}

fn file_reveal(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: PathArgs = parse(args)?;
    let path = viewer::admit(&a.path, &private_roots(ctx)).map_err(read_error)?;
    if std::fs::symlink_metadata(&path).is_err() {
        return Err(ApiError::new("not_found", "The file was not found."));
    }
    handler::reveal(&path).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Value::Null)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ChatArgs {
    chat_id: String,
}

/// The chat's project folder, when it passes the gate.
fn folder_of(ctx: &OpCtx, chat_id: &str) -> Result<Option<String>, ApiError> {
    let db = ctx.services.need::<Database>()?;
    let path: Option<String> = db
        .snapshot(|tx| {
            tx.query_row(
                "SELECT p.path FROM chats c JOIN projects p ON p.id = c.project_id WHERE c.id = ?1",
                [chat_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
        })
        .map_err(|e| ApiError::internal(e.to_string()))?
        .flatten();
    // A folder the gate would refuse is reported as none, so the UI never lists it.
    Ok(path.filter(|p| pathgate::check(p).is_ok()))
}

fn chat_folder(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatArgs = parse(args)?;
    Ok(json!({ "path": folder_of(ctx, &a.chat_id)? }))
}

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(Into::into)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ResolveArgs {
    #[serde(default)]
    chat_id: Option<String>,
    path: String,
}

fn path_resolve(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ResolveArgs = parse(args)?;
    let folder = match &a.chat_id {
        Some(id) => folder_of(ctx, id)?.map(std::path::PathBuf::from),
        None => None,
    };
    let home = home_dir();
    match resolve::resolve(
        &a.path,
        home.as_deref(),
        folder.as_deref(),
        &private_roots(ctx),
    ) {
        Ok(resolve::Resolved::File(p)) => Ok(json!({"path": p.to_string_lossy(), "kind": "file"})),
        Ok(resolve::Resolved::Dir(p)) => Ok(json!({"path": p.to_string_lossy(), "kind": "dir"})),
        Err(m) if m.starts_with("Not found") => Err(ApiError::new("not_found", m)),
        Err(m) => Err(refused(m)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct QuickArgs {
    #[serde(default)]
    chat_id: Option<String>,
    query: String,
    #[serde(default)]
    reload: bool,
}

fn quick_open(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: QuickArgs = parse(args)?;
    let folder = match &a.chat_id {
        Some(id) => folder_of(ctx, id)?.map(std::path::PathBuf::from),
        None => None,
    };
    let roots = private_roots(ctx);
    let q = a.query.trim();
    let result = if q.is_empty() {
        quick::QuickResult {
            mode: "files",
            items: vec![],
            partial: false,
        }
    } else if quick::looks_like_path(q) {
        quick::complete(q, home_dir().as_deref(), folder.as_deref(), &roots).map_err(refused)?
    } else {
        match folder.as_deref() {
            Some(f) => quick::fuzzy(q, f, &roots, a.reload).map_err(refused)?,
            None => quick::QuickResult {
                mode: "files",
                items: vec![],
                partial: false,
            },
        }
    };
    serde_json::to_value(result).map_err(|e| ApiError::internal(e.to_string()))
}

fn panel_tabs_get(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let db = ctx.services.need::<Database>()?;
    db.snapshot(tabs::load)
        .map_err(|e| ApiError::internal(e.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveArgs {
    value: Value,
}

fn panel_tabs_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SaveArgs = parse(args)?;
    let sets = tabs::validate(&a.value).map_err(ApiError::bad_args)?;
    let db = ctx.services.need::<Database>()?;
    db.write(|tx| tabs::save(tx, &sets))
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Value::Null)
}

/// The chat's folder for local review: the chat must exist and not be archived.
fn review_folder(ctx: &OpCtx, chat_id: &str) -> Result<std::path::PathBuf, ApiError> {
    let db = ctx.services.need::<Database>()?;
    let archived: Option<i64> = db
        .snapshot(|tx| {
            tx.query_row("SELECT archived FROM chats WHERE id = ?1", [chat_id], |r| {
                r.get(0)
            })
            .optional()
        })
        .map_err(|e| ApiError::internal(e.to_string()))?;
    match archived {
        None => return Err(ApiError::new("not_found", "That chat no longer exists.")),
        Some(a) if a != 0 => return Err(refused("Archived chats are read-only.")),
        _ => {}
    }
    folder_of(ctx, chat_id)?
        .map(std::path::PathBuf::from)
        .ok_or_else(|| refused("This chat has no project folder."))
}

fn git_error(e: git::GitError) -> ApiError {
    match e {
        git::GitError::NotARepository => ApiError::new("no_repository", e.message()),
        other => refused(other.message()),
    }
}

fn changes_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatArgs = parse(args)?;
    let folder = review_folder(ctx, &a.chat_id)?;
    let g = git::Git::locate().map_err(git_error)?;
    let v = changes::read_changes(&g, &folder, &private_roots(ctx)).map_err(git_error)?;
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DiffArgs {
    chat_id: String,
    path: String,
    side: changes::Side,
    #[serde(default)]
    from: Option<String>,
}

fn diff_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: DiffArgs = parse(args)?;
    let folder = review_folder(ctx, &a.chat_id)?;
    let g = git::Git::locate().map_err(git_error)?;
    let v = changes::read_diff(
        &g,
        &folder,
        &a.path,
        a.from.as_deref(),
        a.side,
        &private_roots(ctx),
    )
    .map_err(git_error)?;
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct StageArgs {
    chat_id: String,
    path: String,
    #[serde(default)]
    from: Option<String>,
}

fn stage_common(ctx: &OpCtx, args: Value, op: changes::StageOp) -> Result<Value, ApiError> {
    let a: StageArgs = parse(args)?;
    // Arguments first, before the chat or the disk is consulted.
    changes::check_rel(&a.path).map_err(ApiError::bad_args)?;
    let folder = review_folder(ctx, &a.chat_id)?;
    let profile = ctx.services.need::<Profile>()?;
    let hooks = changes::hooks_dir(&profile.dir);
    helm_core::privfs::create_private_dir_all(&hooks)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let g = git::Git::locate().map_err(git_error)?;
    let r = changes::stage(
        &g,
        &folder,
        &a.path,
        a.from.as_deref(),
        op,
        &private_roots(ctx),
        &hooks,
    )
    .map_err(|e| match e {
        changes::StageError::Busy => ApiError::new("busy", e.message()),
        changes::StageError::Stale => ApiError::conflict(e.message()),
        changes::StageError::Invalid(m) => ApiError::bad_args(m),
        changes::StageError::Git(g) => git_error(g),
    })?;
    serde_json::to_value(r).map_err(|e| ApiError::internal(e.to_string()))
}

fn stage_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    stage_common(ctx, args, changes::StageOp::Stage)
}

fn unstage_op(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    stage_common(ctx, args, changes::StageOp::Unstage)
}

fn vaults(ctx: &OpCtx) -> Result<Vec<vault::VaultInfo>, ApiError> {
    let db = ctx.services.need::<Database>()?;
    Ok(vault::current(&db))
}

fn vault_list(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    serde_json::to_value(vaults(ctx)?).map_err(|e| ApiError::internal(e.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VaultPathArgs {
    path: String,
}

fn vault_add(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: VaultPathArgs = parse(args)?;
    let path = viewer::admit(a.path.trim(), &private_roots(ctx)).map_err(read_error)?;
    if !path.is_dir() {
        return Err(ApiError::bad_args("That folder does not exist."));
    }
    let db = ctx.services.need::<Database>()?;
    db.write(|tx| {
        tx.execute(
            "INSERT OR IGNORE INTO panel_vaults (path, added_at) VALUES (?1, ?2)",
            rusqlite::params![path.to_string_lossy(), helm_core::now_ms()],
        )
    })
    .map_err(|e| ApiError::internal(e.to_string()))?;
    vault_list(ctx, Value::Null)
}

fn vault_remove(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: VaultPathArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    db.write(|tx| {
        tx.execute(
            "DELETE FROM panel_vaults WHERE lower(path) = lower(?1)",
            [a.path.trim()],
        )
    })
    .map_err(|e| ApiError::internal(e.to_string()))?;
    vault_list(ctx, Value::Null)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VaultNameArgs {
    name: String,
}

fn vault_set_default(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: VaultNameArgs = parse(args)?;
    if !vaults(ctx)?
        .iter()
        .any(|v| v.name.eq_ignore_ascii_case(&a.name))
    {
        return Err(ApiError::bad_args("No vault by that name."));
    }
    let db = ctx.services.need::<Database>()?;
    db.write(|tx| tx.execute("INSERT INTO panel_vault_default (id, name) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET name = excluded.name", [&a.name]))
        .map_err(|e| ApiError::internal(e.to_string()))?;
    vault_list(ctx, Value::Null)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct VaultResolveArgs {
    #[serde(default)]
    chat_id: Option<String>,
    note: String,
    #[serde(default)]
    vault: Option<String>,
    #[serde(default)]
    from_vault: Option<String>,
    #[serde(default)]
    from_path: Option<String>,
}

fn vault_resolve(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: VaultResolveArgs = parse(args)?;
    let list = vaults(ctx)?;
    let folder = match &a.chat_id {
        Some(id) => folder_of(ctx, id)?.map(std::path::PathBuf::from),
        None => None,
    };
    // Inside a vault note, links resolve in that note's vault (beside it first).
    let named = a.vault.as_deref().or(a.from_vault.as_deref());
    let v = vault::choose(&list, named, folder.as_deref()).map_err(refused)?;
    let from = if a
        .from_vault
        .as_deref()
        .is_some_and(|f| f.eq_ignore_ascii_case(&v.name))
    {
        a.from_path.as_deref()
    } else {
        None
    };
    match vault::resolve(v, &a.note, from) {
        Ok(r) => serde_json::to_value(r).map_err(|e| ApiError::internal(e.to_string())),
        Err(m) => Err(ApiError::new("not_found", m)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VaultDocArgs {
    vault: String,
    path: String,
}

fn vault_by_name(ctx: &OpCtx, name: &str) -> Result<vault::VaultInfo, ApiError> {
    vaults(ctx)?
        .into_iter()
        .find(|v| v.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| refused(format!("No vault named \"{name}\" is known to Helm.")))
}

fn vault_read(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: VaultDocArgs = parse(args)?;
    let v = vault_by_name(ctx, &a.vault)?;
    let doc = vault::read(&v, &a.path, &private_roots(ctx)).map_err(|m| {
        if m.contains("not found") {
            ApiError::new("not_found", m)
        } else {
            refused(m)
        }
    })?;
    serde_json::to_value(doc).map_err(|e| ApiError::internal(e.to_string()))
}

fn vault_open(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: VaultDocArgs = parse(args)?;
    let v = vault_by_name(ctx, &a.vault)?;
    // The note must be a real, readable file of that vault before Obsidian is asked to open it.
    let full = vault::confine(std::path::Path::new(&v.path), &a.path, &private_roots(ctx))
        .map_err(refused)?;
    let ext = full
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !full.is_file() || !matches!(ext.as_str(), "md" | "canvas") {
        return Err(ApiError::new(
            "not_found",
            "That note does not exist in the vault.",
        ));
    }
    let url = vault::obsidian_url(&v, &a.path).map_err(refused)?;
    vault::open_url(&url).map_err(|e| refused(e.to_string()))?;
    Ok(Value::Null)
}

fn db_err(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

fn context_get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let on = db
        .snapshot(|tx| context::is_on(tx, &a.chat_id))
        .map_err(db_err)?;
    Ok(json!({ "on": on }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ContextSetArgs {
    chat_id: String,
    on: bool,
}

/// User-only: only the main window's ops reach this (the dispatcher refuses other windows), and no
/// agent tool calls it.
fn context_set(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ContextSetArgs = parse(args)?;
    review_folder(ctx, &a.chat_id)?; // exists and not archived
    let db = ctx.services.need::<Database>()?;
    db.write(|tx| context::set_on(tx, &a.chat_id, a.on))
        .map_err(db_err)?;
    Ok(json!({ "on": a.on }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CommentAddArgs {
    chat_id: String,
    id: String,
    path: String,
    diff_side: String,
    side: String,
    start_line: u32,
    end_line: u32,
    body: String,
    fingerprint: String,
}

fn comment_add(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: CommentAddArgs = parse(args)?;
    let nc = comments::NewComment {
        id: &a.id,
        path: &a.path,
        diff_side: &a.diff_side,
        side: &a.side,
        start: a.start_line,
        end: a.end_line,
        body: &a.body,
    };
    comments::validate(&nc).map_err(ApiError::bad_args)?;
    let folder = review_folder(ctx, &a.chat_id)?;
    let side = match a.diff_side.as_str() {
        "head" => changes::Side::Head,
        "staged" => changes::Side::Staged,
        _ => changes::Side::Unstaged,
    };
    // Re-read the diff the comment is on; refuse when it changed under the user.
    let g = git::Git::locate().map_err(git_error)?;
    let diff = changes::read_diff(&g, &folder, &a.path, None, side, &private_roots(ctx))
        .map_err(git_error)?;
    if comments::fingerprint(&diff.patch) != a.fingerprint {
        return Err(ApiError::conflict(
            "This diff changed. Refresh it, then comment again.",
        ));
    }
    let quote =
        comments::quote(&diff.patch, &a.side, a.start_line, a.end_line).ok_or_else(|| {
            ApiError::bad_args("A comment covers consecutive lines on one side of the diff.")
        })?;
    let db = ctx.services.need::<Database>()?;
    let c = db
        .write(|tx| {
            Ok::<_, rusqlite::Error>(comments::add(
                tx,
                &a.chat_id,
                &nc,
                &quote,
                helm_core::now_ms(),
            ))
        })
        .map_err(db_err)?
        .map_err(refused)?;
    serde_json::to_value(c).map_err(|e| ApiError::internal(e.to_string()))
}

fn comment_list(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    let list = db
        .snapshot(|tx| comments::list(tx, &a.chat_id))
        .map_err(db_err)?;
    serde_json::to_value(list).map_err(|e| ApiError::internal(e.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CommentIdArgs {
    chat_id: String,
    id: String,
}

fn comment_remove(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: CommentIdArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    db.write(|tx| comments::remove(tx, &a.chat_id, &a.id))
        .map_err(db_err)?;
    comment_list(ctx, json!({ "chatId": a.chat_id }))
}

fn comment_clear(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatArgs = parse(args)?;
    let db = ctx.services.need::<Database>()?;
    db.write(|tx| comments::clear(tx, &a.chat_id))
        .map_err(db_err)?;
    Ok(json!([]))
}
