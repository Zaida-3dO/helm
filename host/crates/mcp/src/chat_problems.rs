//! Why a chat's tool servers are not connected, in words for the person and for the agent.
//!
//! One answer serves both: the chat view's notice line (op `mcp_chat_problems`) and
//! `app_this_chat`'s `toolServers` (through the [`helm_hosttools::McpChatProblemsGate`] service).
//! Only a server this chat was actually given counts — the same scoped list the worker's
//! `mcp_config` line carries — and only the name, state and reason travel out: the resolved
//! config holds every secret, so no server entry is ever returned.

use crate::config::McpStore;
use crate::lines::{chat_person, config_line_for_chat, ServicesCell};
use crate::status::{McpStatusStore, ServerState};
use helm_ipc::Services;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use ts_rs::TS;

/// One server the chat was given but cannot use right now.
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatMcpProblem {
    pub server_id: String,
    pub name: String,
    pub state: ServerState,
    pub reason: String,
}

/// The chat's unusable servers: first a config the host itself could not build (no token, a
/// secret that will not read — the reason words are the host's own), then a server whose last
/// connection failed or asked for a sign-in. A server that was simply never used is fine: it
/// connects the first time a chat asks it something.
pub fn problems(services: &Services, chat_id: &str) -> Vec<ChatMcpProblem> {
    let (Some(store), Some(status)) =
        (services.get::<McpStore>(), services.get::<McpStatusStore>())
    else {
        return Vec::new();
    };
    let who = chat_person(services, chat_id);
    let Some(line) = config_line_for_chat(services, &store, &who, chat_id) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for s in line
        .get("servers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(id), Some(name)) = (
            s.get("id").and_then(Value::as_str),
            s.get("name").and_then(Value::as_str),
        ) else {
            continue;
        };
        if s.get("hostServed")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        if let Some(err) = s.get("error").and_then(Value::as_str) {
            // The words a browser sign-in would need differ from a plain token failure, and the
            // worker picks the state the same way when it configures.
            let state = if s.get("oauth").and_then(Value::as_bool).unwrap_or(false) {
                ServerState::SigninRequired
            } else {
                ServerState::Failed
            };
            out.push(problem(id, name, state, err.to_owned()));
            continue;
        }
        match status.get(id) {
            Some(st) if st.state == ServerState::Failed => {
                out.push(problem(
                    id,
                    name,
                    ServerState::Failed,
                    st.error
                        .unwrap_or_else(|| "The last connection failed.".into()),
                ));
            }
            Some(st) if st.state == ServerState::SigninRequired => {
                out.push(problem(
                    id,
                    name,
                    ServerState::SigninRequired,
                    st.error.unwrap_or_else(|| "Needs you to sign in.".into()),
                ));
            }
            _ => {}
        }
    }
    out
}

fn problem(id: &str, name: &str, state: ServerState, reason: String) -> ChatMcpProblem {
    ChatMcpProblem {
        server_id: id.to_owned(),
        name: name.to_owned(),
        state,
        reason,
    }
}

/// The agent-side answer. The tool runs while a worker is live, so the cell the first worker
/// start filled in already holds the app's services; before any worker has started there is
/// nothing to say.
pub struct ThroughWorkerServices(pub Arc<ServicesCell>);

impl helm_hosttools::McpChatProblems for ThroughWorkerServices {
    fn problems(&self, chat_id: &str) -> Vec<helm_hosttools::McpChatProblem> {
        let Some(services) = self.0.get() else {
            return Vec::new();
        };
        problems(&services, chat_id)
            .into_iter()
            .map(|p| helm_hosttools::McpChatProblem {
                name: p.name,
                state: serde_json::to_value(p.state)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "failed".into()),
                reason: p.reason,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::{hub, store};
    use crate::status::ServerStatus;
    use helm_hosttools::McpServerFilter;

    struct NoFilter;
    impl McpServerFilter for NoFilter {
        fn allowed(&self, _: &str) -> Option<Vec<String>> {
            None
        }
        fn any(&self) -> bool {
            false
        }
    }

    fn services_with(st: McpStore, filter: impl McpServerFilter + 'static) -> Arc<Services> {
        let mut services = Services::new();
        services.insert(Arc::new(st));
        services.insert(Arc::new(McpStatusStore::default()));
        services.insert(Arc::new(ServicesCell::default()));
        services.insert(Arc::new(helm_hosttools::McpServerFilterGate(Arc::new(
            filter,
        ))));
        Arc::new(services)
    }

    /// Save a server with a real token, then corrupt the credential file its saved secret
    /// points at: the host can no longer build the server's config, and its own words tell
    /// the person to enter the value again in Settings.
    fn save_then_break(st: &McpStore, dir: &std::path::Path, id: &str, name: &str) {
        let mut s = hub(&format!("tok-{id}"));
        s.id = id.into();
        s.name = name.into();
        st.mutate(None, |l| {
            l.push(s);
            Ok(())
        })
        .unwrap();
        let saved_id = st
            .load()
            .unwrap()
            .into_iter()
            .find(|s| s.id == id)
            .unwrap()
            .transport
            .headers
            .get("Authorization")
            .unwrap()
            .secret_id
            .clone()
            .expect("a saved literal carries its sealed id");
        std::fs::write(
            dir.join("credentials").join(format!("{saved_id}.cred")),
            b"not a sealed envelope",
        )
        .unwrap();
    }

    fn status_of(services: &Services, id: &str, state: ServerState, error: Option<&str>) {
        services
            .get::<McpStatusStore>()
            .unwrap()
            .update(ServerStatus {
                server_id: id.into(),
                state,
                error: error.map(str::to_owned),
                tool_count: None,
                at: 5,
            });
    }

    struct Only;
    impl McpServerFilter for Only {
        fn allowed(&self, chat: &str) -> Option<Vec<String>> {
            (chat == "narrow").then(|| vec!["crm".to_owned()])
        }
        fn any(&self) -> bool {
            true
        }
    }

    #[test]
    fn an_unusable_server_is_named_with_its_reason_and_never_its_secrets() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        save_then_break(&st, d.path(), "files", "Local files");
        let mut down = hub("tok-123456");
        down.id = "crm".into();
        down.name = "CRM".into();
        st.mutate(None, |l| {
            l.push(down);
            Ok(())
        })
        .unwrap();
        let services = services_with(st, NoFilter);
        status_of(
            &services,
            "crm",
            ServerState::Failed,
            Some("Connection refused"),
        );
        let out = problems(&services, "chat");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].server_id, "files");
        assert_eq!(out[0].name, "Local files");
        assert_eq!(out[0].state, ServerState::Failed);
        assert!(
            out[0].reason.contains("Settings"),
            "a broken token says where to fix it: {}",
            out[0].reason
        );
        assert_eq!(out[1].server_id, "crm");
        assert_eq!(out[1].state, ServerState::Failed);
        assert_eq!(out[1].reason, "Connection refused");
        // Nothing secret travels out: no token value, no secrets list, no header names.
        let wire = serde_json::to_string(&out).unwrap();
        assert!(
            !wire.contains("tok-") && !wire.contains("secrets") && !wire.contains("headers"),
            "{wire}"
        );

        // A connected server is not a problem; a sign-in ask is one, with the fallback words
        // when the worker sent none.
        status_of(&services, "crm", ServerState::Connected, None);
        let out = problems(&services, "chat");
        assert_eq!(out.len(), 1, "a connected server is not a problem");
        status_of(&services, "crm", ServerState::SigninRequired, None);
        let out = problems(&services, "chat");
        assert_eq!(out.len(), 2);
        let crm = out.iter().find(|p| p.server_id == "crm").unwrap();
        assert_eq!(crm.state, ServerState::SigninRequired);
        assert_eq!(crm.reason, "Needs you to sign in.");
        // A server the chat never used is never a problem on its own.
        let d2 = tempfile::tempdir().unwrap();
        let st2 = store(d2.path());
        let mut idle = hub("tok-abcdef");
        idle.id = "quiet".into();
        st2.mutate(None, |l| {
            l.push(idle);
            Ok(())
        })
        .unwrap();
        let services = services_with(st2, NoFilter);
        assert_eq!(problems(&services, "chat"), []);
    }

    #[test]
    fn a_servers_list_narrowed_to_the_project_only_reports_what_the_chat_gets() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        save_then_break(&st, d.path(), "files", "Local files");
        save_then_break(&st, d.path(), "crm", "CRM");
        let services = services_with(st, Only);
        let mut ids: Vec<String> = problems(&services, "narrow")
            .into_iter()
            .map(|p| p.server_id)
            .collect();
        assert_eq!(ids, ["crm"], "the chat is only given crm");
        ids = problems(&services, "free")
            .into_iter()
            .map(|p| p.server_id)
            .collect();
        ids.sort();
        assert_eq!(
            ids,
            ["crm", "files"],
            "a chat outside the project gets both"
        );
    }
}
