//! The repository and worktree `api` ops.
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `repo_list` | maintenance | `{repoId?, query?, path?, chatId?, cursor?}` → `{entries, nextCursor?, chatRepo?}` (fills the list from folder projects first) |
//! | `repo_update` | maintenance | `{repoId, expectedRevision, remoteUrl?, addLocations?, removeLocationIds?, remove?, protectedBases?}` → the entry |
//! | `repo_set_worktrees` | maintenance | `{repoId, allowed}` → the entry (the user's switch; no tool calls it) |
//! | `repo_register` | maintenance | `{chatId, repoId?}` → `{result, repoId, checkout, remoteUrl}` |
//! | `worktree_status` | maintenance | `{chatId}` → where the chat is, what the repository allows, who else is there |
//! | `worktree_remove` | heavy | `{path}` → `{removed, reason?}`: only merged, clean, unused worktrees |
//! | `worktree_config` | maintenance | `{root?}` → `{root}` (`root: ""` goes back to the default) |
//!
//! After any change that can move the list of repositories with worktrees off, the list file is
//! rewritten (it is what the git stand-in reads). Running workers are left alone; a chat gets the
//! stand-in on its `PATH` (or loses it) when its worker next starts.

use crate::ops::{parse_args, to_value};
use crate::repos::{self, Entry, Patch, Registered};
use crate::store::Refusal;
use helm_chats::types::ChatStatus;
use helm_core::db::Database;
use helm_core::profile::Profile;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use helm_worktrees::{config, git::Git, offlist, remove, rules};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("repo_list", OpClass::Maintenance, repo_list)
        .blocking("repo_update", OpClass::Maintenance, repo_update)
        .blocking(
            "repo_set_worktrees",
            OpClass::Maintenance,
            repo_set_worktrees,
        )
        .blocking("repo_register", OpClass::Maintenance, repo_register)
        .blocking("worktree_status", OpClass::Maintenance, worktree_status)
        .blocking("worktree_remove", OpClass::Heavy, worktree_remove)
        .blocking("worktree_config", OpClass::Maintenance, worktree_config);
}

/// Rewrite the off list. Chats that are already running keep the `PATH` they started with; a
/// chat picks up the change when its worker next starts (workers are not retired here, since an
/// idle chat may still have sub-agents working in the background).
fn refresh_off_list(ctx: &OpCtx, db: &Database) {
    let Ok(profile) = ctx.services.need::<Profile>() else {
        return;
    };
    let file = profile.dir.join(offlist::FILE_NAME);
    let lines = db.snapshot(repos::off_lines).unwrap_or_default();
    if let Err(e) = offlist::write(&file, &lines) {
        log::warn!("the worktree off list was not written: {e}");
    }
}

fn chat_folder(tx: &rusqlite::Transaction<'_>, chat_id: &str) -> Result<String, Refusal> {
    helm_chats::store::get_chat(tx, chat_id, 0)?
        .map(|c| c.cwd)
        .ok_or_else(|| Refusal::NotFound("There is no chat with that id.".into()))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListArgs {
    repo_id: Option<String>,
    query: Option<String>,
    path: Option<String>,
    chat_id: Option<String>,
    cursor: Option<String>,
}

pub(crate) fn repo_list(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ListArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let out = db.write(|tx| {
        if let Err(e) = repos::sync_from_projects(tx, helm_core::now_ms()) {
            log::warn!("filling the repository list failed: {e:?}");
        }
        let mut entries = repos::all(tx)?;
        let unfiltered = a.repo_id.is_none() && a.query.is_none() && a.path.is_none();
        if let Some(id) = &a.repo_id {
            entries.retain(|e| &e.repo_id == id);
        }
        if let Some(q) = a.query.as_deref().map(str::to_lowercase) {
            entries.retain(|e| {
                e.repo_id.to_lowercase().contains(&q)
                    || e.remote_url
                        .as_deref()
                        .is_some_and(|u| u.to_lowercase().contains(&q))
                    || e.locations
                        .iter()
                        .any(|l| l.path.to_lowercase().contains(&q))
            });
        }
        if let Some(p) = &a.path {
            // Component by component, no disk: the entry with a checkout that is or holds it.
            entries.retain(|e| e.locations.iter().any(|l| rules::is_within(p, &l.path)));
        }
        let start: usize = a
            .cursor
            .as_deref()
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let page: Vec<&Entry> = entries.iter().skip(start).take(repos::MAX_LISTED).collect();
        let next = (start + page.len() < entries.len()).then(|| (start + page.len()).to_string());
        let mut out = json!({"entries": page});
        if let Some(n) = next {
            out["nextCursor"] = json!(n);
        }
        if unfiltered {
            if let Some(chat) = &a.chat_id {
                let folder = chat_folder(tx, chat)?;
                if let Some((repo, loc)) = repos::enclosing(tx, &folder)? {
                    out["chatRepo"] = json!({"repoId": repo, "checkout": loc});
                }
            }
        }
        Ok::<_, Refusal>(out)
    })?;
    refresh_off_list(ctx, &db);
    Ok(out)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocationArg {
    path: String,
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateArgs {
    repo_id: String,
    expected_revision: i64,
    remote_url: Option<String>,
    #[serde(default)]
    add_locations: Vec<LocationArg>,
    #[serde(default)]
    remove_location_ids: Vec<String>,
    #[serde(default)]
    remove: bool,
    protected_bases: Option<Vec<String>>,
}

pub(crate) fn repo_update(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: UpdateArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let patch = Patch {
        repo_id: a.repo_id,
        expected_revision: a.expected_revision,
        remote_url: a.remote_url,
        add_locations: a
            .add_locations
            .into_iter()
            .map(|l| (l.path, l.label))
            .collect(),
        remove_location_ids: a.remove_location_ids,
        remove: a.remove,
        protected_bases: a.protected_bases,
    };
    let entry = db.write(|tx| repos::update(tx, &patch))?;
    refresh_off_list(ctx, &db);
    to_value(&entry)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetWorktreesArgs {
    repo_id: String,
    allowed: bool,
}

pub(crate) fn repo_set_worktrees(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SetWorktreesArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let entry =
        db.write(|tx| repos::set_worktrees(tx, &a.repo_id, a.allowed, helm_core::now_ms()))?;
    refresh_off_list(ctx, &db);
    to_value(&entry)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegisterArgs {
    chat_id: String,
    repo_id: Option<String>,
}

pub(crate) fn repo_register(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: RegisterArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let out = db.write(|tx| {
        let folder = chat_folder(tx, &a.chat_id)?;
        let (repo, how) = repos::register_checkout(
            tx,
            Path::new(&folder),
            a.repo_id.as_deref(),
            helm_core::now_ms(),
        )?;
        let entry = repos::get(tx, &repo)?;
        let key = rules::path_key(&folder);
        let checkout = entry
            .locations
            .iter()
            .find(|l| rules::path_key(&l.path) == key)
            .or_else(|| {
                entry
                    .locations
                    .iter()
                    .find(|l| rules::is_within(&folder, &l.path))
            });
        Ok::<_, Refusal>(json!({
            "result": match how {
                Registered::Added => "added",
                Registered::Created => "created",
                Registered::AlreadyListed => "alreadyListed",
            },
            "repoId": entry.repo_id,
            "checkout": checkout,
            "remoteUrl": entry.remote_url,
        }))
    })?;
    refresh_off_list(ctx, &db);
    Ok(out)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChatArgs {
    chat_id: String,
}

fn busy(s: ChatStatus) -> bool {
    matches!(
        s,
        ChatStatus::Starting | ChatStatus::Running | ChatStatus::Stopping
    )
}

/// For a folder that is a linked worktree, the main checkout it belongs to, read from the `.git`
/// file (`gitdir: <main>/.git/worktrees/<name>`) without starting git.
fn main_of_linked(folder: &Path) -> Option<String> {
    let text = std::fs::read_to_string(folder.join(".git")).ok()?;
    let gitdir = text.trim().strip_prefix("gitdir:")?.trim();
    let dotgit = Path::new(gitdir).parent()?.parent()?;
    (dotgit.file_name()? == ".git")
        .then(|| dotgit.parent().map(|p| p.to_string_lossy().into_owned()))?
}

pub(crate) fn worktree_status(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let profile_dir = ctx.services.get::<Profile>().map(|p| p.dir.clone());
    let out = db.write(|tx| {
        let _ = repos::sync_from_projects(tx, helm_core::now_ms());
        let folder = chat_folder(tx, &a.chat_id)?;
        let path = Path::new(&folder);
        let linked = !config::is_clone_root(path) && path.join(".git").is_file();
        let anchor = if linked { main_of_linked(path) } else { None };
        let found = repos::enclosing(tx, anchor.as_deref().unwrap_or(&folder))?;
        let root = repos::root(tx)?;
        let chats = helm_chats::store::list_chats(tx, 0)?;
        let mut out = json!({
            "folder": folder,
            "isLinkedWorktree": linked,
            "worktreeRoot": root.to_string_lossy(),
            "limits": {"maxNameChars": rules::MAX_NAME, "maxPathChars": rules::MAX_PATH_CHARS, "nameCharacters": "a-z, 0-9 and hyphens"},
            "folderPathChars": rules::path_chars(path),
            "note": "Check git status before assuming the checkout is free to change.",
        });
        if linked {
            out["folderOverBudget"] = json!(rules::check_budget(path).is_err());
        }
        let Some((repo_id, loc)) = found else {
            out["repo"] = Value::Null;
            out["registerable"] = json!(config::is_clone_root(path));
            out["worktrees"] = json!("allowed");
            return Ok::<_, Refusal>(out);
        };
        let entry = repos::get(tx, &repo_id)?;
        out["repo"] = json!({
            "repoId": entry.repo_id,
            "checkoutPath": loc.path,
            "defaultBranch": entry.default_branch,
            "protectedBases": entry.protected_bases,
        });
        out["worktrees"] = json!(if entry.worktrees_allowed { "allowed" } else { "off" });
        if !entry.worktrees_allowed {
            out["enforcement"] = json!("git worktree add is refused for this repository in Helm's chats; Claude Code's own worktree tools stay off while any repository is switched off.");
        } else if let Ok(path) = rules::plan_path(&root, &entry.repo_id, "your-change") {
            out["suggestedPath"] = json!(path.to_string_lossy().replace("your-change", "<name>"));
        } else {
            out["suggestedPathProblem"] = json!("The worktree root is too long to hold a worktree within the path limit; shorten it in Settings.");
        }
        let mut others = Vec::new();
        for c in chats.iter().filter(|c| c.id != a.chat_id && rules::is_within(&c.cwd, &loc.path)) {
            others.push(json!({"chatId": c.id, "title": c.title, "state": if busy(c.status) {"working"} else {"idle"}}));
        }
        let truncated = others.len() > 20;
        others.truncate(20);
        out["otherChats"] = json!(others);
        out["otherChatsTruncated"] = json!(truncated);
        let mut free = Vec::new();
        for l in entry.locations.iter().filter(|l| l.id != loc.id) {
            if !chats.iter().any(|c| rules::is_within(&c.cwd, &l.path)) {
                free.push(json!({"path": l.path}));
            }
        }
        out["freeCheckouts"] = json!(free.iter().take(20).collect::<Vec<_>>());
        out["freeCheckoutsTruncated"] = json!(free.len() > 20);
        Ok(out)
    })?;
    let mut out = out;
    if let Some(dir) = profile_dir {
        out["claudeWorktreeTools"] = json!(
            helm_workers::launch::ClaudeRuntime::load(&dir)
                .0
                .worktree_tools
        );
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoveArgs {
    path: String,
}

fn worktree_remove(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: RemoveArgs = parse_args(args)?;
    let path = Path::new(&a.path);
    if !path.is_absolute() || !path.is_dir() {
        return Err(ApiError::bad_args(
            "The path must be an existing folder given in full.",
        ));
    }
    let db = ctx.services.need::<Database>()?;
    // A chat that lives in the folder, working or not, keeps it.
    let in_use = db
        .snapshot(|tx| helm_chats::store::list_chats(tx, 0))
        .map(|chats| chats.iter().any(|c| rules::is_within(&c.cwd, &a.path)))
        .ok();
    let git = Git::locate()
        .ok_or_else(|| ApiError::new("refused", "Git is not installed or not on PATH."))?;
    match remove::remove(&git, path, in_use) {
        remove::Outcome::Removed { links_removed } => {
            Ok(json!({"removed": true, "linksRemoved": links_removed}))
        }
        remove::Outcome::Kept(reason) => Ok(json!({"removed": false, "reason": reason})),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfigArgs {
    root: Option<String>,
}

pub(crate) fn worktree_config(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ConfigArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let root = match a.root {
        None => db
            .snapshot(repos::root)
            .map_err(|e| ApiError::internal(e.to_string()))?,
        Some(r) => {
            let r = r.trim().to_owned();
            db.write(|tx| repos::set_root(tx, (!r.is_empty()).then_some(r.as_str())))?
        }
    };
    Ok(json!({"root": root.to_string_lossy()}))
}
