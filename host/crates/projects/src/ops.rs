//! The project and sidebar `api` ops.
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `project_list` | blocking | [`ProjectListArgs`] → `ProjectNode[]` (flat; the tree comes from `parentId`) |
//! | `project_create` | maintenance | [`ProjectCreateArgs`] → [`ProjectNode`] |
//! | `project_create_folder` | maintenance | [`ProjectCreateFolderArgs`] → [`ProjectNode`] (a new folder by name) |
//! | `project_update` | maintenance | [`ProjectUpdateArgs`] → [`ProjectNode`] (a new primary folder retires the project's idle workers) |
//! | `project_archive` / `project_restore` | maintenance | [`ProjectIdArgs`] → [`ProjectNode`] |
//! | `sidebar_page` | heavy | [`SidebarPageArgs`] → [`SidebarPage`] |
//! | `sidebar_rows` | blocking | [`SidebarRowsArgs`] → `SidebarRow[]` |
//! | `chat_organize` | maintenance | [`ChatOrganizeArgs`] → `SidebarRow[]` (the rows that moved) |
//!
//! Events: `project {project}` after a project changes, `sidebar {chatIds}` (a re-fetch signal)
//! after rows move.

use crate::store::{self, Refusal};
use crate::types::*;
use helm_chats::clock::Clock;
use helm_core::db::Database;
use helm_ipc::{ApiError, AppEvent, OpClass, OpCtx, OpRegistry};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("project_list", OpClass::Blocking, project_list)
        .blocking("project_create", OpClass::Maintenance, project_create)
        .blocking(
            "project_create_folder",
            OpClass::Maintenance,
            project_create_folder,
        )
        .blocking("project_update", OpClass::Maintenance, project_update)
        .blocking("project_archive", OpClass::Maintenance, project_archive)
        .blocking("project_restore", OpClass::Maintenance, project_restore)
        .blocking("sidebar_page", OpClass::Heavy, sidebar_page)
        .blocking("sidebar_rows", OpClass::Blocking, sidebar_rows)
        .blocking("chat_organize", OpClass::Maintenance, chat_organize);
    crate::board::register_ops(reg);
}

pub(crate) fn parse_args<T: DeserializeOwned>(args: Value) -> Result<T, ApiError> {
    let args = if args.is_null() {
        Value::Object(Default::default())
    } else {
        args
    };
    serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))
}

pub(crate) fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

impl From<Refusal> for ApiError {
    fn from(r: Refusal) -> Self {
        match r {
            Refusal::BadArgs(m) => ApiError::bad_args(m),
            Refusal::NotFound(m) => ApiError::new("not_found", m),
            Refusal::Conflict(m) => ApiError::conflict(m),
            Refusal::Refused(m) => ApiError::new("refused", m),
            Refusal::Sql(e) => ApiError::internal(e.to_string()),
        }
    }
}

pub(crate) fn emit_project(ctx: &OpCtx, node: &ProjectNode) {
    if let Ok(v) = serde_json::to_value(node) {
        ctx.events.emit(AppEvent::new("project").with("project", v));
    }
}

pub(crate) fn emit_sidebar(ctx: &OpCtx, ids: &[String]) {
    if !ids.is_empty() {
        ctx.events
            .emit(AppEvent::new("sidebar").with("chatIds", serde_json::json!(ids)));
    }
}

fn project_list(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ProjectListArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let list = db.snapshot(|tx| store::list_projects(tx, a.include_archived.unwrap_or(false)))?;
    to_value(&list)
}

fn project_create(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ProjectCreateArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let node = db.write(|tx| {
        let id = store::create_project(tx, &a, helm_core::now_ms())?;
        store::get_project(tx, &id)
    })?;
    emit_project(ctx, &node);
    to_value(&node)
}

fn project_update(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ProjectUpdateArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let (before, node) = db.write(|tx| {
        let before = store::get_project(tx, &a.project_id)?.path;
        store::update_project(tx, &a)?;
        Ok::<_, Refusal>((before, store::get_project(tx, &a.project_id)?))
    })?;
    // Primary folder…: the project's chats were all idle (update_project checks);
    // retire their workers so each next turn starts in the new folder.
    if a.path.is_some() && before != node.path {
        retire_workers(ctx, &db, &a.project_id);
    }
    emit_project(ctx, &node);
    to_value(&node)
}

fn retire_workers(ctx: &OpCtx, db: &Database, project_id: &str) {
    let Some(rt) = ctx.services.get::<helm_chats::runtime::ChatRuntime>() else {
        return;
    };
    let ids: Vec<String> = db
        .snapshot(|tx| {
            let mut st = tx.prepare("SELECT id FROM chats WHERE project_id = ?1")?;
            let rows = st.query_map([project_id], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<String>>>()
        })
        .unwrap_or_default();
    for id in ids {
        rt.retire(&id);
    }
}

/// `project_create_folder`: Add project → New folder.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectCreateFolderArgs {
    pub name: String,
    /// Where to make the folder; the per-user projects folder when missing.
    #[serde(default)]
    pub parent: Option<String>,
}

/// The per-user projects folder: `<home>/Helm Projects`. The
/// Add project dialog can pick another parent.
pub fn default_projects_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join("Helm Projects"))
}

/// A project name that is also a safe folder name on Windows: the name rules, plus
/// no `< > " | ? *`, no trailing dot or space, and none of the reserved device names.
pub fn folder_name(raw: &str) -> Result<String, Refusal> {
    let name = store::validate_name(raw)?;
    let bad = |m: &str| Refusal::BadArgs(m.into());
    if name
        .chars()
        .any(|c| matches!(c, '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return Err(bad("A folder name cannot contain < > \" | ? or *."));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(bad("A folder name cannot end with a dot or a space."));
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_lowercase();
    let numbered = |prefix: &str| {
        stem.strip_prefix(prefix).is_some_and(|rest| {
            let mut it = rest.chars();
            matches!(it.next(), Some(c) if c.is_ascii_digit() || matches!(c, '\u{b9}' | '\u{b2}' | '\u{b3}'))
                && it.next().is_none()
        })
    };
    let reserved = matches!(
        stem.as_str(),
        "con" | "prn" | "aux" | "nul" | "conin$" | "conout$"
    ) || numbered("com")
        || numbered("lpt");
    if reserved {
        return Err(bad("That name is reserved by Windows; choose another."));
    }
    Ok(name)
}

/// Make the folder `name` under `parent`, then a project over it; the folder is removed again if
/// the project cannot be made. Refuses a folder that already exists.
pub fn create_folder_project(
    db: &Database,
    name: &str,
    parent: &Path,
    now: i64,
) -> Result<ProjectNode, Refusal> {
    let name = folder_name(name)?;
    if !parent.is_absolute() || !parent.is_dir() {
        return Err(Refusal::BadArgs(
            "The folder to create it in must be an existing absolute path.".into(),
        ));
    }
    let dir = parent.join(&name);
    if dir.exists() {
        return Err(Refusal::Conflict(format!(
            "A folder named {name} already exists there; add it as an existing folder instead."
        )));
    }
    std::fs::create_dir(&dir)
        .map_err(|e| Refusal::Refused(format!("Could not make the folder: {e}")))?;
    let made = db.write(|tx| {
        let id = store::create_project(
            tx,
            &ProjectCreateArgs {
                name: name.clone(),
                path: Some(dir.to_string_lossy().into_owned()),
                parent_id: None,
            },
            now,
        )?;
        store::get_project(tx, &id)
    });
    if made.is_err() {
        // Only an empty folder is removed, so nothing the user put there can go.
        let _ = std::fs::remove_dir(&dir);
    }
    made
}

fn project_create_folder(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ProjectCreateFolderArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let parent = match a.parent.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => {
            let dir = default_projects_dir().ok_or_else(|| {
                ApiError::new("refused", "There is no home folder to make projects in.")
            })?;
            std::fs::create_dir_all(&dir).map_err(|e| {
                ApiError::new("refused", format!("Could not make {}: {e}", dir.display()))
            })?;
            dir
        }
    };
    let node = create_folder_project(&db, &a.name, &parent, helm_core::now_ms())?;
    emit_project(ctx, &node);
    to_value(&node)
}

fn archive_or_restore(ctx: &OpCtx, args: Value, archive: bool) -> Result<Value, ApiError> {
    let a: ProjectIdArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let (node, ids) = db.write(|tx| {
        let ids = if archive {
            store::archive_project(tx, &a.project_id)?
        } else {
            store::restore_project(tx, &a.project_id)?
        };
        Ok::<_, Refusal>((store::get_project(tx, &a.project_id)?, ids))
    })?;
    emit_project(ctx, &node);
    emit_sidebar(ctx, &ids);
    to_value(&node)
}

fn project_archive(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    archive_or_restore(ctx, args, true)
}

fn project_restore(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    archive_or_restore(ctx, args, false)
}

fn revision(ctx: &OpCtx) -> i64 {
    ctx.services
        .get::<Clock>()
        .map(|c| c.current())
        .unwrap_or(0)
}

fn sidebar_page(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SidebarPageArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let rev = revision(ctx);
    to_value(&db.snapshot(|tx| store::sidebar_page(tx, &a, rev))?)
}

fn sidebar_rows(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SidebarRowsArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let rev = revision(ctx);
    to_value(&db.snapshot(|tx| store::sidebar_rows(tx, &a.chat_ids, rev))?)
}

fn chat_organize(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: ChatOrganizeArgs = parse_args(args)?;
    let db = ctx.services.need::<Database>()?;
    let ids = db.write(|tx| store::organize(tx, &a))?;
    let rev = ctx.services.get::<Clock>().map(|c| c.next()).unwrap_or(0);
    let rows = db.snapshot(|tx| store::sidebar_rows(tx, &ids, rev))?;
    emit_sidebar(ctx, &ids);
    to_value(&rows)
}

#[cfg(test)]
mod newchat_tests {
    //! Add project → New folder, and Primary folder… through the op.
    use super::*;
    use crate::schema::tests::temp_db;
    use helm_ipc::{RecordingSink, Services};
    use std::sync::Arc;

    #[test]
    fn folder_names_follow_the_project_rules_and_windows() {
        assert_eq!(folder_name("  site ").unwrap(), "site");
        for bad in [
            "",
            "..",
            "a/b",
            "a\\b",
            "a:b",
            "a?b",
            "a*b",
            "a|b",
            "a\"b",
            "<a>",
            "dot.",
            "trailing .",
            "con",
            "NUL",
            "aux.txt",
            "com1",
            "LPT9.md",
            "CONIN$",
            "conout$.txt",
            "com\u{b9}",
            "LPT\u{b3}.md",
        ] {
            assert!(folder_name(bad).is_err(), "{bad:?}");
        }
        for ok in ["console", "com10", "nullable", "lpt", "my.app"] {
            assert!(folder_name(ok).is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn a_new_folder_project_makes_the_folder_and_refuses_an_existing_one() {
        let (_d, db) = temp_db();
        let parent = tempfile::tempdir().unwrap();
        let node = create_folder_project(&db, "Site", parent.path(), 5).unwrap();
        let dir = parent.path().join("Site");
        assert!(dir.is_dir());
        assert_eq!(node.name, "Site");
        assert_eq!(node.path.as_deref(), Some(dir.to_string_lossy().as_ref()));
        assert!(matches!(
            create_folder_project(&db, "Site", parent.path(), 6),
            Err(Refusal::Conflict(_))
        ));
        assert!(matches!(
            create_folder_project(&db, "nul", parent.path(), 6),
            Err(Refusal::BadArgs(_))
        ));
        assert!(!parent.path().join("nul").exists());
        let missing = parent.path().join("missing");
        assert!(matches!(
            create_folder_project(&db, "x", &missing, 6),
            Err(Refusal::BadArgs(_))
        ));
        let n: i64 = db
            .snapshot(|tx| tx.query_row("SELECT count(*) FROM projects", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(n, 1, "refusals make no project");
    }

    #[test]
    fn the_op_creates_in_the_given_parent_and_announces_the_project() {
        let (_d, db) = temp_db();
        let parent = tempfile::tempdir().unwrap();
        let mut services = Services::new();
        services.insert(Arc::new(db));
        let sink = Arc::new(RecordingSink::default());
        let ctx = OpCtx {
            services: Arc::new(services),
            events: sink.clone(),
        };
        let v = project_create_folder(
            &ctx,
            serde_json::json!({"name": "Helm", "parent": parent.path().to_string_lossy()}),
        )
        .unwrap();
        assert_eq!(v["name"], "Helm");
        assert!(parent.path().join("Helm").is_dir());
        assert_eq!(sink.kinds(), ["project"]);
        let e = project_create_folder(&ctx, serde_json::json!({"name": "x", "where": "y"}))
            .unwrap_err();
        assert_eq!(e.code, "bad_args");
    }
}
