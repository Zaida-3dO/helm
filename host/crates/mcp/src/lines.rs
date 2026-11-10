//! The MCP client's worker lines, through the host-tools worker-line registry:
//!
//! - `mcp_config {revision, servers}` goes to each worker when it starts (a start line), and to
//!   every running worker again after each config save (the [`Broadcaster`]). Secrets are
//!   resolved here, at send time, and travel only in this line.
//! - `mcp_status` from a worker updates the [`McpStatusStore`] and signals `mcp_status` to the UI.

use crate::config::McpStore;
use crate::status::{McpStatusStore, ServerStatus};
use helm_core::db::Database;
use helm_hosttools::lines::{Broadcaster, WorkerLines};
use helm_ipc::{AppEvent, Services};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, Weak};

/// The `mcp_config` line for the current config, or `None` when it cannot be read.
pub fn config_line(store: &McpStore) -> Option<Value> {
    config_line_for(store, None)
}

/// The `mcp_config` line for one chat: a header that names the chat's person carries `person`.
pub fn config_line_for(store: &McpStore, person: Option<&str>) -> Option<Value> {
    let (revision, servers) = store
        .resolve_enabled_for(&|k| std::env::var(k).ok(), person)
        .ok()?;
    Some(json!({ "type": "mcp_config", "revision": revision, "servers": servers }))
}

/// The hub person a chat belongs to: the one its profile is linked to (the home profile's when
/// the chat has none of its own), or `None` when that profile is not linked to a hub person.
pub fn chat_person(services: &Services, chat_id: &str) -> Option<String> {
    let db = services.get::<Database>()?;
    db.snapshot(|tx| {
        let own = tx
            .query_row(
                "SELECT hub_person_id FROM profiles
                 WHERE id = (SELECT NULLIF(profile_id, '') FROM chats WHERE id = ?1)",
                [chat_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?;
        match own {
            Some(person) => Ok::<_, rusqlite::Error>(person),
            None => tx
                .query_row(
                    "SELECT hub_person_id FROM profiles WHERE is_home = 1",
                    [],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()
                .map(Option::flatten),
        }
    })
    .ok()
    .flatten()
    .filter(|p| !p.is_empty())
}

/// Remembers the app's services the first time a worker starts, so a config save (which happens
/// in an op, not on a worker line) can reach the [`Broadcaster`].
#[derive(Default)]
pub struct ServicesCell(Mutex<Option<Weak<Services>>>);

impl ServicesCell {
    fn set(&self, s: &Arc<Services>) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::downgrade(s));
    }
    fn get(&self) -> Option<Arc<Services>> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(Weak::upgrade)
    }

    /// Sends the current config to every running worker.
    pub fn broadcast(&self) {
        let Some(s) = self.get() else { return };
        let (Some(store), Some(b)) = (s.get::<McpStore>(), s.get::<Broadcaster>()) else {
            return;
        };
        // A config that sends the chat's person differs from chat to chat.
        if store.uses_chat_person() {
            let running = b.0.running_chats();
            if !running.is_empty() {
                for chat in running {
                    let person = chat_person(&s, &chat);
                    if let Some(line) = config_line_for(&store, person.as_deref()) {
                        b.0.send_to_worker(&chat, &line);
                    }
                }
                return;
            }
        }
        if let Some(line) = config_line(&store) {
            b.0.broadcast_to_workers(&line);
        }
    }
}

pub fn register_worker_lines(l: &mut WorkerLines) {
    l.register_worker_line("mcp_status", |ctx, v| {
        let Ok(st) = serde_json::from_value::<ServerStatus>(v.clone()) else {
            return;
        };
        if let Some(store) = ctx.services.get::<McpStatusStore>() {
            if store.update(st.clone()) {
                ctx.events.emit(
                    AppEvent::new("mcp_status")
                        .with("serverId", st.server_id)
                        .with("state", serde_json::to_value(st.state).unwrap_or_default()),
                );
            }
        }
    });
    l.register_start_line(|chat, services| {
        if let Some(cell) = services.get::<ServicesCell>() {
            cell.set(services);
        }
        let person = chat_person(services, chat);
        config_line_for(&*services.get::<McpStore>()?, person.as_deref())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::{hub, store};
    use helm_hosttools::lines::{WorkerBroadcast, WorkerLineCtx};
    use helm_ipc::{EventSink, RecordingSink};

    #[derive(Default)]
    struct Sent(Mutex<Vec<Value>>);
    impl WorkerBroadcast for Sent {
        fn broadcast_to_workers(&self, line: &Value) {
            self.0.lock().unwrap().push(line.clone());
        }
        fn send_to_worker(&self, _: &str, _: &Value) -> bool {
            false
        }
    }

    #[test]
    fn a_chat_starts_with_its_profiles_hub_person() {
        use helm_core::db::MigrationRegistry;
        let d = tempfile::tempdir().unwrap();
        let db =
            Arc::new(Database::open(&d.path().join("t.db"), &MigrationRegistry::new()).unwrap());
        db.write(|tx| {
            tx.execute_batch(
                "CREATE TABLE profiles (id TEXT PRIMARY KEY, hub_person_id TEXT, is_home INTEGER);
                 ALTER TABLE chats ADD COLUMN profile_id TEXT;
                 INSERT INTO projects (id, name, created_at) VALUES ('p', 'P', 1);
                 INSERT INTO profiles VALUES ('home', '3', 1), ('sam', '7', 0), ('guest', NULL, 0);
                 INSERT INTO chats (id, project_id, created_at, updated_at, profile_id)
                 VALUES ('c-sam', 'p', 1, 1, 'sam'), ('c-guest', 'p', 1, 1, 'guest'),
                        ('c-none', 'p', 1, 1, ''), ('c-gone', 'p', 1, 1, 'deleted');",
            )
        })
        .unwrap();
        let st = Arc::new(store(d.path()));
        st.mutate(None, |l| {
            let mut s = hub("tok-123456");
            s.preset = Some("helm_hub".into());
            s.id = "helm_hub".into();
            l.push(s);
            Ok(())
        })
        .unwrap();
        let mut services = Services::new();
        services.insert(db);
        services.insert(st);
        let services = Arc::new(services);
        let mut l = WorkerLines::new();
        register_worker_lines(&mut l);
        // The home person's chats, a chat with no profile and one whose profile is gone send
        // the home person; a chat of a profile with no hub person sends none.
        for (chat, want) in [
            ("c-sam", Some("7")),
            ("c-guest", None),
            ("c-none", Some("3")),
            ("c-gone", Some("3")),
            ("unknown", Some("3")),
        ] {
            let start = l.start_lines(chat, &services);
            let headers = &start[0]["servers"][0]["headers"];
            assert_eq!(headers["X-Helm-Person"].as_str(), want, "{chat}");
            assert_eq!(headers["Authorization"], "Bearer tok-123456");
        }
    }

    #[test]
    fn start_line_status_line_and_broadcast() {
        let d = tempfile::tempdir().unwrap();
        let st = Arc::new(store(d.path()));
        st.mutate(None, |l| {
            l.push(hub("tok-123456"));
            Ok(())
        })
        .unwrap();
        let sent = Arc::new(Sent::default());
        let cell = Arc::new(ServicesCell::default());
        let mut services = Services::new();
        services.insert(st.clone());
        services.insert(Arc::new(McpStatusStore::default()));
        services.insert(Arc::new(Broadcaster(sent.clone())));
        services.insert(cell.clone());
        let services = Arc::new(services);

        let mut l = WorkerLines::new();
        register_worker_lines(&mut l);
        let start = l.start_lines("chat", &services);
        assert_eq!(start.len(), 1);
        assert_eq!(start[0]["type"], "mcp_config");
        assert_eq!(
            start[0]["servers"][0]["headers"]["Authorization"],
            "Bearer tok-123456"
        );
        assert_eq!(start[0]["servers"][0]["secrets"][0], "tok-123456");

        let sink = Arc::new(RecordingSink::default());
        let events: Arc<dyn EventSink> = sink.clone();
        let ctx = WorkerLineCtx {
            chat_id: "chat",
            services: &services,
            events: &events,
        };
        let line = json!({"type": "mcp_status", "serverId": "hub", "state": "connected", "toolCount": 4, "at": 5});
        assert!(l.handle(&ctx, &line));
        assert!(l.handle(&ctx, &line), "a repeat is still handled");
        assert_eq!(sink.kinds(), ["mcp_status"], "a repeat emits nothing new");
        assert_eq!(
            services.get::<McpStatusStore>().unwrap().list()[0].tool_count,
            Some(4)
        );

        cell.broadcast();
        let b = sent.0.lock().unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0]["type"], "mcp_config");
        drop(b);

        // A per-server change (a second server added, the first disabled) reaches every running
        // worker as a fresh line carrying only the servers still enabled: the worker reconfigures
        // live, no worker restart, and the disabled server's secret is no longer sent.
        let mut board = hub("tok-abcdef");
        board.id = "board".into();
        board.name = "Board".into();
        st.mutate(None, |l| {
            l.push(board);
            Ok(())
        })
        .unwrap();
        st.mutate(None, |l| {
            if let Some(h) = l.iter_mut().find(|s| s.id == "hub") {
                h.enabled = false;
            }
            Ok(())
        })
        .unwrap();
        cell.broadcast();
        let b = sent.0.lock().unwrap();
        assert_eq!(b.len(), 2);
        let ids: Vec<&str> = b[1]["servers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["board"], "the line holds only the enabled servers");
        assert!(
            !b[1].to_string().contains("tok-123456"),
            "the disabled server's secret is gone from the line"
        );
    }
}
