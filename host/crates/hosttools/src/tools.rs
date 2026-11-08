//! The project and repository host tools.
//!
//! They act through the projects crate's own `api` ops ([`ToolCtx::op`]), so its rules (paths,
//! archive, revisions) apply unchanged and this crate needs no dependency on it.
//!
//! The repository list lives in the projects crate (`repo_*` ops): `app_repo_locations` reads it,
//! `app_update_repo` edits an entry, `app_register_checkout` adds the chat's own folder and
//! `app_worktree_status` says where the chat stands and what the repository allows.

use crate::registry::{HostToolRegistry, ToolCtx, ToolError, ToolResult};
use crate::spec::{Condition, ToolSpec};
use serde_json::{json, Value};

const EXPLICIT: &str = " Act only when the user explicitly asked for this change; check app_list_projects first for duplicates or an ambiguous match; report what changed. If the reply is lost, check the current state before trying again and never retry automatically.";
const MAX_LISTED: usize = 80;

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

/// What a write to a closed project says.
const CLOSED_PROJECT: &str = "This project is closed. Reopen it first.";

/// One project as the tools describe it. A board (kind `board`) is where chats are shown; its
/// `primaryFolder` is where its new chats start. A folder project (kind `folder`) is where chats
/// run, at `path`.
fn project_summary(p: &Value, all: &[Value]) -> Value {
    let primary = p["primaryFolderProjectId"].as_str();
    let primary_path = primary
        .and_then(|id| all.iter().find(|f| f["id"] == id))
        .map(|f| f["path"].clone())
        .unwrap_or(Value::Null);
    json!({
        "projectId": p["id"],
        "name": p["name"],
        "kind": p["kind"],
        "path": p["path"],
        "primaryFolder": primary_path,
        "archived": p["archived"],
        "pinned": p["pinned"],
        "chatCount": p["chatCount"],
    })
}

/// Two folders compared the way Helm compares them: either slash, no trailing one, ignoring case
/// on Windows.
fn same_folder(a: &str, b: &str) -> bool {
    let norm = |p: &str| {
        let p = p.trim().replace('\\', "/");
        let p = p.trim_end_matches('/').to_owned();
        if cfg!(windows) {
            p.to_lowercase()
        } else {
            p
        }
    };
    norm(a) == norm(b)
}

/// The folder project over `folder`, added when Helm has none. An open one is preferred; a closed
/// one is reused rather than duplicated (a closed folder project can still be where a chat runs).
async fn folder_project(ctx: &ToolCtx, all: &[Value], folder: &str) -> Result<Value, ToolError> {
    let over = |closed: bool| {
        all.iter().find(|p| {
            p["kind"] == "folder"
                && p["archived"] != true
                && (p["closed"] == true) == closed
                && p["path"].as_str().is_some_and(|x| same_folder(x, folder))
        })
    };
    if let Some(p) = over(false).or_else(|| over(true)) {
        return Ok(p["id"].clone());
    }
    let name = std::path::Path::new(folder)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty() && !n.contains(['/', '\\', ':']))
        .unwrap_or_else(|| "Folder".into());
    let p = ctx
        .op("project_create", json!({"name": name, "path": folder}))
        .await?;
    Ok(p["id"].clone())
}

/// Every project, closed ones included: a closed project is left out of what a tool lists, but a
/// tool that targets one by id has to find it to say it is closed.
async fn all_projects(ctx: &ToolCtx, archived: bool) -> Result<Vec<Value>, ToolError> {
    let v = ctx
        .op(
            "project_list",
            json!({"includeArchived": archived, "includeClosed": true}),
        )
        .await?;
    Ok(v.as_array().cloned().unwrap_or_default())
}

fn find(all: &[Value], id: &str) -> Result<Value, ToolError> {
    all.iter()
        .find(|p| p["id"] == id)
        .cloned()
        .ok_or_else(|| ToolError::refused("There is no project with that id."))
}

async fn list_projects(ctx: ToolCtx, a: Value) -> ToolResult {
    let q = a
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    if q.len() > 512 {
        return Err(ToolError::refused("query is at most 512 bytes."));
    }
    let all = all_projects(&ctx, true).await?;
    let hits: Vec<Value> = all
        .iter()
        .filter(|p| p["closed"] != true)
        .filter(|p| {
            q.is_empty()
                || p["name"]
                    .as_str()
                    .is_some_and(|n| n.to_lowercase().contains(&q))
        })
        .map(|p| project_summary(p, &all))
        .collect();
    let truncated = hits.len() > MAX_LISTED;
    Ok(
        json!({"projects": hits.into_iter().take(MAX_LISTED).collect::<Vec<_>>(), "truncated": truncated}),
    )
}

async fn create_project(ctx: ToolCtx, a: Value) -> ToolResult {
    let name = a
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty() && n.chars().count() <= 120)
        .ok_or_else(|| ToolError::refused("name must be 1-120 characters."))?;
    let mut args = json!({"name": name});
    if let Some(p) = a.get("path").and_then(Value::as_str) {
        args["path"] = json!(p);
    }
    let p = ctx.op("project_create", args).await?;
    Ok(project_summary(&p, &[]))
}

async fn update_project(ctx: ToolCtx, a: Value) -> ToolResult {
    let id = a
        .get("projectId")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::refused("projectId is required."))?
        .to_owned();
    for k in ["path", "beforeProjectId", "position", "icon"] {
        if a.get(k).is_some() {
            return Err(ToolError::refused(format!(
                "{k} cannot be changed by a chat yet; ask the user to do it in the sidebar."
            )));
        }
    }
    let all = all_projects(&ctx, true).await?;
    let cur = find(&all, &id)?;
    if cur["closed"] == true {
        return Err(ToolError::refused(CLOSED_PROJECT));
    }
    let mut upd = json!({"projectId": id, "revision": cur["revision"]});
    let mut any = false;
    // A board's folder: where its new chats start. The folder is added as a folder project when
    // Helm has none over it.
    if let Some(f) = a.get("primaryFolder") {
        if cur["kind"] != "board" {
            return Err(ToolError::refused(
                "primaryFolder is for boards. A folder project's folder cannot be changed by a chat; ask the user to do it in Settings.",
            ));
        }
        let f = f
            .as_str()
            .map(str::trim)
            .filter(|f| std::path::Path::new(f).is_absolute())
            .ok_or_else(|| ToolError::refused("primaryFolder must be an absolute path."))?;
        if !std::path::Path::new(f).is_dir() {
            return Err(ToolError::refused("That folder does not exist."));
        }
        upd["primaryFolderProjectId"] = folder_project(&ctx, &all, f).await?;
        any = true;
    }
    if let Some(n) = a.get("name").and_then(Value::as_str) {
        let n = n.trim();
        if n.is_empty() || n.len() > 512 {
            return Err(ToolError::refused("name must be 1-512 bytes."));
        }
        upd["name"] = json!(n);
        any = true;
    }
    if let Some(p) = a.get("pinned").and_then(Value::as_bool) {
        upd["pinned"] = json!(p);
        any = true;
    }
    if let Some(c) = a.get("color").and_then(Value::as_str) {
        upd["colour"] = if c == "none" { Value::Null } else { json!(c) };
        any = true;
    }
    if any {
        ctx.op("project_update", upd).await?;
    }
    if let Some(arch) = a.get("archived").and_then(Value::as_bool) {
        if arch != cur["archived"].as_bool().unwrap_or(false) {
            let op = if arch {
                "project_archive"
            } else {
                "project_restore"
            };
            ctx.op(op, json!({"projectId": id})).await?;
        }
    }
    let all = all_projects(&ctx, true).await?;
    Ok(project_summary(&find(&all, &id)?, &all))
}

async fn repo_locations(ctx: ToolCtx, a: Value) -> ToolResult {
    let mut args = a;
    args["chatId"] = json!(ctx.caller_chat_id);
    ctx.op("repo_list", args).await
}

async fn update_repo(ctx: ToolCtx, a: Value) -> ToolResult {
    let r = ctx.op("repo_update", a).await?;
    Ok(json!({"repository": r}))
}

async fn worktree_status(ctx: ToolCtx, _a: Value) -> ToolResult {
    ctx.op("worktree_status", json!({"chatId": ctx.caller_chat_id}))
        .await
}

async fn register_checkout(ctx: ToolCtx, a: Value) -> ToolResult {
    let mut args = json!({"chatId": ctx.caller_chat_id});
    if let Some(id) = a.get("repoId") {
        args["repoId"] = id.clone();
    }
    ctx.op("repo_register", args).await
}

/// `app_read_project_context`: the shared note of the folder this chat runs in.
async fn read_project_context(ctx: ToolCtx, _a: Value) -> ToolResult {
    ctx.op("project_note_get", json!({"chatId": ctx.caller_chat_id}))
        .await
}

/// `app_update_project_context`: replace that note (checked against the revision read).
async fn update_project_context(ctx: ToolCtx, a: Value) -> ToolResult {
    let text = a
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::refused("text is required (an empty string clears the note)."))?;
    let rev = a
        .get("expectedRevision")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            ToolError::refused("expectedRevision is required: the revision you read.")
        })?;
    ctx.op(
        "project_note_set",
        json!({"chatId": ctx.caller_chat_id, "text": text, "expectedRevision": rev}),
    )
    .await
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.async_tool(
        ToolSpec::read(
            "app_list_projects",
            "List Helm's projects in sidebar order with their ids, kinds, folders, chat counts and whether they are pinned or archived; query filters by name. Projects the user has closed are left out, and a closed project refuses new chats and edits until the user reopens it. Boards (kind board) are where chats are shown: app_create_chat takes a board's id, and a board's primaryFolder is where its new chats start. Folder projects (kind folder) are where chats run (path).",
            obj(json!({"query": {"type": "string", "maxLength": 512}}), &[]),
        )
        .self_locking(),
        list_projects,
    )
    .async_tool(
        ToolSpec::write(
            "app_create_project",
            format!("Add a folder project (where chats run, not a board): an existing absolute folder (path), or a new folder named name in Helm's projects folder.{EXPLICIT}"),
            obj(
                json!({"name": {"type": "string", "minLength": 1, "maxLength": 120}, "path": {"type": "string", "maxLength": 4096}}),
                &["name"],
            ),
        )
        .self_locking(),
        create_project,
    )
    .async_tool(
        ToolSpec::write(
            "app_update_project",
            format!("Rename, pin or unpin, recolour (a palette name, #rrggbb or none), archive or restore a project, or set a board's folder (primaryFolder: an absolute folder where the board's new chats start; it is added as a folder project when Helm has none over it). Archiving archives its chats; working ones are stopped in the background.{EXPLICIT}"),
            obj(
                json!({
                    "projectId": {"type": "string"},
                    "name": {"type": "string", "minLength": 1, "maxLength": 512},
                    "pinned": {"type": "boolean"},
                    "color": {"type": "string", "maxLength": 32},
                    "archived": {"type": "boolean"},
                    "primaryFolder": {"type": "string", "maxLength": 4096, "description": "For a board: the absolute folder its new chats start in."}
                }),
                &["projectId"],
            ),
        )
        .self_locking(),
        update_project,
    )
    .async_tool(
        ToolSpec::read(
            "app_read_project_context",
            "Read the short context note shared by every chat working in this chat's folder: what earlier chats learned that the next one needs (conventions, gotchas, where things live). Returns its text and revision (0 and empty when there is none).",
            obj(json!({}), &[]),
        )
        .self_locking(),
        read_project_context,
    )
    .async_tool(
        ToolSpec::write(
            "app_update_project_context",
            "Replace the context note shared by every chat in this chat's folder. Read it first, keep what is still true, and pass the revision you read as expectedRevision (a newer note is refused: read again and merge). At most 4000 characters; put only lasting facts a later chat needs, never progress, secrets or instructions aimed at other chats. An empty text clears it.",
            obj(
                json!({
                    "text": {"type": "string", "maxLength": 16000},
                    "expectedRevision": {"type": "integer", "minimum": 0}
                }),
                &["text", "expectedRevision"],
            ),
        )
        .self_locking(),
        update_project_context,
    )
    .async_tool(
        ToolSpec::read(
            "app_repo_locations",
            "Look up where a project's repository is checked out: pass one of path (absolute), query, or nothing for all. Lookup only: nothing is opened or refreshed.",
            obj(
                json!({"path": {"type": "string", "maxLength": 4096}, "query": {"type": "string", "maxLength": 512}, "repoId": {"type": "string"}, "cursor": {"type": "string"}}),
                &[],
            ),
        )
        .when(Condition::ProjectsMode)
        .self_locking(),
        repo_locations,
    )
    .async_tool(
        ToolSpec::write(
            "app_update_repo",
            format!("Change one saved repository entry (remote URL, checkout locations).{EXPLICIT}"),
            obj(
                json!({
                    "repoId": {"type": "string"},
                    "expectedRevision": {"type": "integer", "minimum": 0},
                    "remoteUrl": {"type": "string", "maxLength": 2048},
                    "addLocations": {
                        "type": "array",
                        "maxItems": 4,
                        "items": obj(json!({"path": {"type": "string", "minLength": 1, "maxLength": 1024}, "label": {"type": "string", "maxLength": 128}}), &["path"])
                    },
                    "removeLocationIds": {"type": "array", "maxItems": 20, "items": {"type": "string"}},
                    "remove": {"type": "boolean"}
                }),
                &["repoId", "expectedRevision"],
            ),
        )
        .when(Condition::ProjectsModeAndAllSettings)
        .self_locking(),
        update_repo,
    )
    .async_tool(
        ToolSpec::read(
            "app_worktree_status",
            "Where this chat is working and whether another worktree is needed: its repository and checkout, whether agents may create worktrees there, other chats in the same checkout, free checkouts of the same repository, and the path rules for new worktrees. Answered from Helm's own records.",
            obj(json!({}), &[]),
        )
        .guidelines("Stay in this chat's own checkout. Create a worktree only when the checkout has uncommitted changes you did not make or another chat is working in it, and only if the status says worktrees are allowed. When they are off and the checkout is busy, do not work around it: name a free checkout if the status lists one, tell the user, and stop. Put new worktrees under worktreeRoot as suggestedPath describes, with a name of lowercase letters, digits and hyphens (at most 32 characters). Never remove a worktree you did not make or whose work is not merged.")
        .self_locking(),
        worktree_status,
    )
    .async_tool(
        ToolSpec::write(
            "app_register_checkout",
            "Add the folder this chat works in to Helm's repository list, when it is the root of a clone. It joins the repository with the same GitHub remote, or becomes a new one named after the remote (or the folder when there is no remote). It only adds; it never changes or removes anything.",
            obj(json!({"repoId": {"type": "string", "minLength": 1, "maxLength": 512}}), &[]),
        )
        .self_locking(),
        register_checkout,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{CallOrigin, OpInvoker};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    struct FakeOps(Mutex<Vec<(String, Value)>>);
    impl OpInvoker for FakeOps {
        fn invoke<'a>(
            &'a self,
            op: &'a str,
            args: Value,
        ) -> Pin<Box<dyn Future<Output = Result<Value, helm_ipc::ApiError>> + Send + 'a>> {
            self.0.lock().unwrap().push((op.to_owned(), args.clone()));
            Box::pin(async move {
                match op {
                    "project_list" => Ok(json!([
                        {"id": "p1", "name": "Helm", "kind": "folder", "path": FOLDER, "archived": false, "pinned": false, "chatCount": 2, "revision": 7},
                        {"id": "p2", "name": "Board", "kind": "board", "path": null, "primaryFolderProjectId": "p1", "archived": false, "pinned": true, "chatCount": 0, "revision": 1},
                        {"id": "p8", "name": "src", "kind": "folder", "path": format!("{FOLDER}/src"), "archived": false, "closed": true, "pinned": false, "chatCount": 0, "revision": 2},
                        {"id": "p9", "name": "Helmsman", "kind": "board", "path": null, "archived": false, "closed": true, "pinned": false, "chatCount": 0, "revision": 3}
                    ])),
                    "project_create" => {
                        Ok(json!({"id": "p3", "kind": "folder", "path": args["path"]}))
                    }
                    "project_update" | "project_archive" => Ok(json!({})),
                    _ => Err(helm_ipc::ApiError::bad_args("no")),
                }
            })
        }
    }

    fn ctx(ops: Arc<FakeOps>) -> ToolCtx {
        ToolCtx {
            caller_chat_id: "c".into(),
            is_child: false,
            tool_call_id: None,
            origin: CallOrigin::Agent,
            services: Arc::new(helm_ipc::Services::new()),
            events: Arc::new(helm_ipc::NullSink),
            ops: Some(ops),
        }
    }

    /// A folder that exists on every machine the tests run on.
    const FOLDER: &str = env!("CARGO_MANIFEST_DIR");

    #[tokio::test]
    async fn projects_go_through_the_project_ops() {
        let ops = Arc::new(FakeOps(Mutex::new(Vec::new())));
        let r = list_projects(ctx(ops.clone()), json!({"query": "hel"}))
            .await
            .unwrap();
        assert_eq!(r["projects"].as_array().unwrap().len(), 1);
        assert_eq!(r["projects"][0]["projectId"], "p1");
        // A closed project is left out of the listing and refuses edits.
        assert_eq!(r["projects"].as_array().unwrap().len(), 1, "p9 is closed");
        let e = update_project(ctx(ops.clone()), json!({"projectId": "p9", "name": "x"}))
            .await
            .unwrap_err();
        assert!(e
            .public
            .contains("This project is closed. Reopen it first."));
        update_project(
            ctx(ops.clone()),
            json!({"projectId": "p1", "name": "Helm 2", "archived": true}),
        )
        .await
        .unwrap();
        let calls = ops.0.lock().unwrap().clone();
        let upd = calls.iter().find(|(o, _)| o == "project_update").unwrap();
        assert_eq!(upd.1["revision"], 7, "the current revision is passed");
        assert_eq!(upd.1["name"], "Helm 2");
        assert!(calls
            .iter()
            .any(|(o, a)| o == "project_archive" && a["projectId"] == "p1"));
        let e = update_project(ctx(ops.clone()), json!({"projectId": "p1", "path": "C:/x"}))
            .await
            .unwrap_err();
        assert!(e.public.contains("path cannot be changed"));
        // A board's folder: an open folder project over it is reused, else one is added; only a
        // board takes one, and only an absolute folder that exists.
        assert_eq!(r["projects"][0]["primaryFolder"], Value::Null);
        let boards = list_projects(ctx(ops.clone()), json!({"query": "board"}))
            .await
            .unwrap();
        assert_eq!(boards["projects"][0]["primaryFolder"], FOLDER);
        let parent = std::path::Path::new(FOLDER).parent().unwrap();
        let parent = parent.to_string_lossy().into_owned();
        for (args, err, folder_project) in [
            (
                json!({"projectId": "p2", "primaryFolder": format!("{FOLDER}/")}),
                "",
                "p1",
            ),
            (
                json!({"projectId": "p2", "primaryFolder": parent}),
                "",
                "p3",
            ),
            // A closed folder project is reused, not duplicated.
            (
                json!({"projectId": "p2", "primaryFolder": format!("{FOLDER}/src")}),
                "",
                "p8",
            ),
            (
                json!({"projectId": "p1", "primaryFolder": FOLDER}),
                "for boards",
                "",
            ),
            (
                json!({"projectId": "p2", "primaryFolder": "relative"}),
                "absolute",
                "",
            ),
            (
                json!({"projectId": "p2", "primaryFolder": format!("{FOLDER}/missing-folder")}),
                "does not exist",
                "",
            ),
        ] {
            ops.0.lock().unwrap().clear();
            let r = update_project(ctx(ops.clone()), args.clone()).await;
            let calls = ops.0.lock().unwrap().clone();
            let upd = calls.iter().find(|(o, _)| o == "project_update");
            if err.is_empty() {
                assert!(r.is_ok(), "{args}: {r:?}");
                assert_eq!(
                    upd.unwrap().1["primaryFolderProjectId"],
                    folder_project,
                    "{args}"
                );
                assert_eq!(
                    calls.iter().any(|(o, _)| o == "project_create"),
                    folder_project == "p3",
                    "{args}: a folder project is added only when none is open over it"
                );
            } else {
                assert!(r.unwrap_err().public.contains(err), "{args}");
                assert!(upd.is_none(), "{args}");
            }
        }
        // The repository tools hand their arguments to the repository ops, with the caller's chat.
        repo_locations(ctx(ops.clone()), json!({"query": "helm"}))
            .await
            .unwrap_err();
        let _ = update_repo(
            ctx(ops.clone()),
            json!({"repoId": "r", "expectedRevision": 1}),
        )
        .await;
        let _ = worktree_status(ctx(ops.clone()), json!({})).await;
        let _ = register_checkout(ctx(ops.clone()), json!({"repoId": "x"})).await;
        let calls = ops.0.lock().unwrap().clone();
        let call = |op: &str| calls.iter().rev().find(|(o, _)| o == op).unwrap().1.clone();
        assert_eq!(call("repo_list"), json!({"query": "helm", "chatId": "c"}));
        assert_eq!(call("repo_update")["expectedRevision"], 1);
        assert_eq!(call("worktree_status"), json!({"chatId": "c"}));
        assert_eq!(call("repo_register"), json!({"chatId": "c", "repoId": "x"}));
    }
}
