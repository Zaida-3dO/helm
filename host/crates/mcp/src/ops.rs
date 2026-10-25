//! The `api` ops Settings → MCP uses. The main window's saves take the revision when it
//! sends one; agents go through [`crate::admin`] instead, which always requires it.
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `mcp_config_get` | blocking | `{}` → [`McpEditorSnapshot`] |
//! | `mcp_server_save` | maintenance | [`McpServerSaveArgs`] → [`McpEditorSnapshot`] (add, or replace the same id) |
//! | `mcp_server_remove` | maintenance | [`McpServerRemoveArgs`] → [`McpEditorSnapshot`] |
//! | `mcp_server_set_enabled` | maintenance | [`McpServerEnableArgs`] → [`McpEditorSnapshot`] |
//!
//! | `mcp_server_test` | blocking | [`McpServerTestArgs`](crate::probe::McpServerTestArgs) → [`TestOutcome`](crate::probe::TestOutcome) (connects once, saves nothing) |
//! | `mcp_presets_list` | blocking | `{}` → [`McpPreset`](crate::presets::McpPreset)`[]` |
//! | `mcp_servers_status` | blocking | `{}` → [`ServerStatus`](crate::status::ServerStatus)`[]` |
//! | `mcp_repo_servers_list` | blocking | `{}` → [`RepoServer`](crate::repo_gate::RepoServer)`[]` (seen since launch) |
//! | `mcp_repo_approve` | maintenance | `{file, name, hash}` → the list |
//! | `mcp_repo_revoke` | maintenance | `{file, name}` → the list |
//!
//! Every save emits the `mcp_config {revision}` event (a signal to re-read).

use crate::config::{McpStore, StoreError};
use crate::repo_gate::RepoGate;
use crate::types::*;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde_json::Value;

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("mcp_config_get", OpClass::Blocking, config_get)
        .blocking("mcp_server_save", OpClass::Maintenance, server_save)
        .blocking("mcp_server_remove", OpClass::Maintenance, server_remove)
        .blocking(
            "mcp_server_set_enabled",
            OpClass::Maintenance,
            server_set_enabled,
        )
        .blocking("mcp_server_test", OpClass::Blocking, server_test)
        .blocking("mcp_presets_list", OpClass::Blocking, |_, _| {
            to_value(&crate::presets::presets())
        })
        .blocking("mcp_servers_status", OpClass::Blocking, |ctx, _| {
            let st = ctx.services.need::<crate::status::McpStatusStore>()?;
            to_value(&st.list())
        })
        .blocking("mcp_repo_servers_list", OpClass::Blocking, |ctx, _| {
            to_value(&ctx.services.need::<RepoGate>()?.seen())
        })
        .blocking("mcp_repo_approve", OpClass::Maintenance, |ctx, args| {
            let a: RepoApproveArgs = parse(args)?;
            let g = ctx.services.need::<RepoGate>()?;
            g.approve(&a.file, &a.name, &a.hash)
                .map_err(|e| ApiError::bad_args(e.0))?;
            to_value(&g.seen())
        })
        .blocking("mcp_repo_revoke", OpClass::Maintenance, |ctx, args| {
            let a: RepoRevokeArgs = parse(args)?;
            let g = ctx.services.need::<RepoGate>()?;
            g.revoke(&a.file, &a.name)
                .map_err(|e| ApiError::internal(e.0))?;
            to_value(&g.seen())
        });
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepoApproveArgs {
    file: String,
    name: String,
    hash: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepoRevokeArgs {
    file: String,
    name: String,
}

pub(crate) fn api_error(e: StoreError) -> ApiError {
    match e {
        StoreError::Invalid(m) => ApiError::bad_args(m),
        StoreError::Conflict(m) => ApiError::conflict(m),
        StoreError::Unavailable(m) => ApiError::internal(m),
    }
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

fn parse<T: for<'de> serde::Deserialize<'de>>(args: Value) -> Result<T, ApiError> {
    serde_json::from_value(args).map_err(|e| ApiError::bad_args(e.to_string()))
}

fn config_get(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    if !(args.is_null() || args.as_object().is_some_and(|o| o.is_empty())) {
        return Err(ApiError::bad_args("mcp_config_get takes no arguments."));
    }
    let st = ctx.services.need::<McpStore>()?;
    to_value(&st.snapshot().map_err(api_error)?)
}

pub(crate) fn save(st: &McpStore, a: McpServerSaveArgs) -> Result<McpEditorSnapshot, StoreError> {
    st.mutate(a.revision.as_deref(), |list| {
        match list.iter_mut().find(|s| s.id == a.server.id) {
            Some(slot) => *slot = a.server,
            None => list.push(a.server),
        }
        Ok(())
    })
}

fn server_save(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: McpServerSaveArgs = parse(args)?;
    let st = ctx.services.need::<McpStore>()?;
    to_value(&save(&st, a).map_err(api_error)?)
}

fn server_test(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: crate::probe::McpServerTestArgs = parse(args)?;
    let st = ctx.services.need::<McpStore>()?;
    let resolved = st.resolve_draft(&a.server);
    to_value(&crate::probe::test_resolved(&a.server, &resolved))
}

fn server_remove(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: McpServerRemoveArgs = parse(args)?;
    let st = ctx.services.need::<McpStore>()?;
    let snap = st
        .mutate(a.revision.as_deref(), |list| {
            let n = list.len();
            list.retain(|s| s.id != a.server_id);
            if list.len() == n {
                return Err(StoreError::Invalid("That server no longer exists.".into()));
            }
            Ok(())
        })
        .map_err(api_error)?;
    to_value(&snap)
}

fn server_set_enabled(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: McpServerEnableArgs = parse(args)?;
    let st = ctx.services.need::<McpStore>()?;
    let snap = st
        .mutate(a.revision.as_deref(), |list| {
            let s = list
                .iter_mut()
                .find(|s| s.id == a.server_id)
                .ok_or_else(|| StoreError::Invalid("That server no longer exists.".into()))?;
            s.enabled = a.enabled;
            Ok(())
        })
        .map_err(api_error)?;
    to_value(&snap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::{hub, store};

    #[test]
    fn the_user_may_set_the_opt_in_and_replace_by_id() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        let mut s = hub("tok");
        s.transport.url = Some("http://10.20.30.7:8080/mcp".into());
        s.allow_private_http = true;
        let snap = save(
            &st,
            McpServerSaveArgs {
                revision: None,
                server: s.clone(),
            },
        )
        .unwrap();
        assert_eq!(snap.servers.len(), 1);
        s.name = "Renamed".into();
        // Keep the saved secret by sending its id back.
        s.transport.headers = snap.servers[0].transport.headers.clone();
        let snap2 = save(
            &st,
            McpServerSaveArgs {
                revision: Some(snap.revision),
                server: s,
            },
        )
        .unwrap();
        assert_eq!(snap2.servers.len(), 1);
        assert_eq!(snap2.servers[0].name, "Renamed");
        let (_, r) = st.resolve_enabled(&|_| None).unwrap();
        assert_eq!(r[0].headers["Authorization"], "Bearer tok");
    }

    #[test]
    fn errors_map_to_api_codes() {
        assert_eq!(api_error(StoreError::Conflict("x".into())).code, "conflict");
        assert_eq!(api_error(StoreError::Invalid("x".into())).code, "bad_args");
    }
}
