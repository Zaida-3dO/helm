//! Per-server status across workers. Workers send `mcp_status` lines; the host
//! keeps the newest per server and serves it to Settings → MCP (`mcp_servers_status`, and the
//! `mcp_status` event as a signal to re-read).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;
use ts_rs::TS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ServerState {
    Idle,
    Connecting,
    Connected,
    Failed,
    SigninRequired,
    /// Claude Code loads this server natively, so Helm's client leaves it alone.
    Native,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub server_id: String,
    pub state: ServerState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub tool_count: Option<u32>,
    #[ts(type = "number")]
    pub at: i64,
}

const MAX_ERROR: usize = 2_000;

#[derive(Default)]
pub struct McpStatusStore {
    latest: Mutex<BTreeMap<String, ServerStatus>>,
}

impl McpStatusStore {
    /// Keeps `s` if it is at least as new as what is held. Returns whether it changed anything.
    pub fn update(&self, mut s: ServerStatus) -> bool {
        if let Some(e) = &mut s.error {
            if e.len() > MAX_ERROR {
                let mut cut = MAX_ERROR;
                while !e.is_char_boundary(cut) {
                    cut -= 1;
                }
                e.truncate(cut);
            }
        }
        let mut m = self.latest.lock().unwrap_or_else(|p| p.into_inner());
        match m.get(&s.server_id) {
            Some(old) if old.at > s.at || *old == s => false,
            _ => {
                m.insert(s.server_id.clone(), s);
                true
            }
        }
    }

    /// Forgets servers that are no longer configured.
    pub fn retain(&self, ids: &[String]) {
        let mut m = self.latest.lock().unwrap_or_else(|p| p.into_inner());
        m.retain(|k, _| ids.contains(k));
    }

    pub fn list(&self) -> Vec<ServerStatus> {
        self.latest
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(id: &str, state: ServerState, at: i64) -> ServerStatus {
        ServerStatus {
            server_id: id.into(),
            state,
            error: None,
            tool_count: None,
            at,
        }
    }

    #[test]
    fn newest_wins_across_workers() {
        let s = McpStatusStore::default();
        assert!(s.update(st("a", ServerState::Connected, 10)));
        assert!(
            !s.update(st("a", ServerState::Failed, 5)),
            "an older line is ignored"
        );
        assert!(
            !s.update(st("a", ServerState::Connected, 10)),
            "a repeat changes nothing"
        );
        assert!(s.update(st("a", ServerState::Failed, 11)));
        assert_eq!(s.list()[0].state, ServerState::Failed);
        s.update(st("b", ServerState::Idle, 1));
        s.retain(&["b".into()]);
        assert_eq!(s.list().len(), 1);
    }

    #[test]
    fn wire_shape() {
        let v: ServerStatus = serde_json::from_value(serde_json::json!({
            "serverId": "hub", "state": "signin_required", "error": "x", "toolCount": 3, "at": 1
        }))
        .unwrap();
        assert_eq!(v.state, ServerState::SigninRequired);
        assert_eq!(v.tool_count, Some(3));
        let mut long = st("x", ServerState::Failed, 1);
        long.error = Some("é".repeat(1500));
        let s = McpStatusStore::default();
        s.update(long);
        assert!(s.list()[0].error.as_ref().unwrap().len() <= 2000);
    }
}
