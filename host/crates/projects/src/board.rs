//! The Agent Standup connection and sync ops. Contract stubs for now: the read-only
//! client (`helm-standup`) and the mirror upsert come next.
//!
//! | op | class | args → result |
//! |---|---|---|
//! | `board_connection_get` | blocking | `{}` → [`BoardConnection`] |
//! | `board_connection_save` | maintenance | [`BoardConnectionSaveArgs`] → [`BoardConnection`] |
//! | `board_sync` | async | `{}` → [`BoardSyncState`] |

use crate::ops::to_value;
use crate::types::*;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use serde_json::Value;

/// No built-in board address: the user enters theirs in Settings → Projects (the repo is public).
pub const DEFAULT_URL: &str = "";

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("board_connection_get", OpClass::Blocking, connection_get)
        .blocking(
            "board_connection_save",
            OpClass::Maintenance,
            connection_save,
        )
        .async_op("board_sync", sync);
}

fn connection_get(_ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    to_value(&BoardConnection {
        url: DEFAULT_URL.into(),
        has_token: false,
    })
}

fn connection_save(_ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    Err(ApiError::new(
        "unsupported",
        "The Agent Standup connection is not built yet.",
    ))
}

async fn sync(_ctx: OpCtx, _args: Value) -> Result<Value, ApiError> {
    to_value(&BoardSyncState {
        configured: false,
        syncing: false,
        last_sync_at: None,
        error: None,
        project_count: 0,
    })
}
