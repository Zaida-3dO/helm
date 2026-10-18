//! The project and repository host tools.
//!
//! They act through the projects crate's own `api` ops ([`ToolCtx::op`]), so its rules (paths,
//! archive, revisions) apply unchanged and this crate needs no dependency on it.
//!
//! Helm has no repository registry (no saved remotes or declared checkouts), so
//! `app_repo_locations` reports each folder project as a local checkout, and `app_update_repo`
//! answers that the registry is not built yet and changes nothing.

use crate::registry::{HostToolRegistry, ToolCtx, ToolError, ToolResult};
use crate::spec::{Condition, ToolSpec};
use serde_json::{json, Value};

const EXPLICIT: &str = " Act only when the user explicitly asked for this change; check app_list_projects first for duplicates or an ambiguous match; report what changed. If the reply is lost, check the current state before trying again and never retry automatically.";
const MAX_LISTED: usize = 80;

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

fn project_summary(p: &Value) -> Value {
    json!({
        "projectId": p["id"],
        "name": p["name"],
        "kind": p["kind"],
        "path": p["path"],
        "archived": p["archived"],
        "pinned": p["pinned"],
        "chatCount": p["chatCount"],
    })
}

async fn all_projects(ctx: &ToolCtx, archived: bool) -> Result<Vec<Value>, ToolError> {
    let v = ctx
        .op("project_list", json!({"includeArchived": archived}))
        .await?;
    Ok(v.as_array().cloned().unwrap_or_default())
}

async fn find(ctx: &ToolCtx, id: &str) -> Result<Value, ToolError> {
    all_projects(ctx, true)
        .await?
        .into_iter()
        .find(|p| p["id"] == id)
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
    let hits: Vec<Value> = all_projects(&ctx, true)
        .await?
        .iter()
        .filter(|p| {
            q.is_empty()
                || p["name"]
                    .as_str()
                    .is_some_and(|n| n.to_lowercase().contains(&q))
        })
        .map(project_summary)
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
    Ok(project_summary(&p))
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
    let cur = find(&ctx, &id).await?;
    let mut upd = json!({"projectId": id, "revision": cur["revision"]});
    let mut any = false;
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
    Ok(project_summary(&find(&ctx, &id).await?))
}

async fn repo_locations(ctx: ToolCtx, a: Value) -> ToolResult {
    let path = a.get("path").and_then(Value::as_str).map(str::to_lowercase);
    let q = a
        .get("query")
        .and_then(Value::as_str)
        .map(str::to_lowercase);
    let entries: Vec<Value> = all_projects(&ctx, false)
        .await?
        .iter()
        .filter(|p| p["path"].is_string())
        .filter(|p| {
            let pp = p["path"].as_str().unwrap_or("").to_lowercase();
            path.as_ref().is_none_or(|x| &pp == x)
                && q.as_ref().is_none_or(|x| {
                    pp.contains(x.as_str())
                        || p["name"]
                            .as_str()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(x.as_str())
                })
        })
        .take(50)
        .map(|p| {
            json!({
                "projectId": p["id"],
                "name": p["name"],
                "locations": [{"kind": "local", "path": p["path"]}],
            })
        })
        .collect();
    Ok(json!({
        "entries": entries,
        "note": "Helm has no saved repository registry yet: each folder project is listed as a local checkout.",
    }))
}

fn update_repo(_ctx: &ToolCtx, _a: Value) -> ToolResult {
    Err(ToolError::refused(
        "Helm has no saved repository registry yet, so there is nothing to change; add or move a project folder in the sidebar instead.",
    ))
}

pub fn register_host_tools(r: &mut HostToolRegistry) {
    r.async_tool(
        ToolSpec::read(
            "app_list_projects",
            "List Helm's projects in sidebar order with their ids, folders, chat counts and whether they are pinned or archived; query filters by name.",
            obj(json!({"query": {"type": "string", "maxLength": 512}}), &[]),
        )
        .self_locking(),
        list_projects,
    )
    .async_tool(
        ToolSpec::write(
            "app_create_project",
            format!("Add a project: an existing absolute folder (path), or a new folder named name in Helm's projects folder.{EXPLICIT}"),
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
            format!("Rename, pin or unpin, recolour (a palette name, #rrggbb or none), archive or restore a project. Archiving archives its chats and fails while any of them is working.{EXPLICIT}"),
            obj(
                json!({
                    "projectId": {"type": "string"},
                    "name": {"type": "string", "minLength": 1, "maxLength": 512},
                    "pinned": {"type": "boolean"},
                    "color": {"type": "string", "maxLength": 32},
                    "archived": {"type": "boolean"}
                }),
                &["projectId"],
            ),
        )
        .self_locking(),
        update_project,
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
    .blocking(
        ToolSpec::write(
            "app_update_repo",
            format!("Change one saved repository entry (remote URL, checkout locations).{EXPLICIT}"),
            obj(
                json!({
                    "repoId": {"type": "string"},
                    "expectedRevision": {"type": "integer", "minimum": 0},
                    "remoteUrl": {"type": "string", "maxLength": 2048},
                    "addLocations": {"type": "array", "maxItems": 4},
                    "removeLocationIds": {"type": "array", "maxItems": 20},
                    "remove": {"type": "boolean"}
                }),
                &["repoId", "expectedRevision"],
            ),
        )
        .when(Condition::ProjectsModeAndAllSettings),
        update_repo,
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
                        {"id": "p1", "name": "Helm", "kind": "folder", "path": "C:/w/helm", "archived": false, "pinned": false, "chatCount": 2, "revision": 7},
                        {"id": "p2", "name": "Board", "kind": "board", "path": null, "archived": false, "pinned": true, "chatCount": 0, "revision": 1}
                    ])),
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

    #[tokio::test]
    async fn projects_go_through_the_project_ops() {
        let ops = Arc::new(FakeOps(Mutex::new(Vec::new())));
        let r = list_projects(ctx(ops.clone()), json!({"query": "hel"}))
            .await
            .unwrap();
        assert_eq!(r["projects"].as_array().unwrap().len(), 1);
        assert_eq!(r["projects"][0]["projectId"], "p1");
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
        let r = repo_locations(ctx(ops.clone()), json!({})).await.unwrap();
        assert_eq!(
            r["entries"].as_array().unwrap().len(),
            1,
            "folder projects only"
        );
        assert!(update_repo(&ctx(ops), json!({}))
            .unwrap_err()
            .public
            .contains("no saved repository registry"));
    }
}
