//! The production [`ChatPort`]: this install's chats, profiles and ops.
//!
//! Reads go straight to the tables; everything that changes a chat goes through the same ops the
//! app's own window uses, so a remote change is checked exactly like a local one. The tables of
//! the profiles crate (`profiles`, `chat_shares`) are read here directly: that crate depends on
//! this one.

use crate::identity;
use crate::link::BoxFut;
use crate::sends;
use crate::serve::{ChatPort, ChatState, PortResult};
use crate::wire::{
    code, AnswerArgs, ArchiveArgs, CreateArgs, DeviceInfo, Hello, PersonInfo, ProjectInfo,
    RemoteChat, RenameArgs, SetModelArgs, ShareArgs, WireError,
};
use helm_chats::submit::{self, HostSend};
use helm_chats::types::{ChatGetResult, ChatRecord, MessageRow, SubmissionOrigin};
use helm_chats::{store, ChatRuntime};
use helm_core::db::Database;
use helm_ipc::{ApiError, Dispatcher, Services};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use std::sync::Arc;

/// The most chats `chats_sub` lists.
const LISTED: i64 = 2000;
/// A list's cursor is the revision clock's reading; the activity times it is compared with come
/// from the wall clock, which may lag the revision clock a little, so a catch-up reaches back
/// this far.
const CATCH_UP_SLACK_MS: i64 = 60_000;
/// The most chat records one answer carries.
const LIST_BYTES: usize = 900 * 1024;

fn wire(e: ApiError) -> WireError {
    WireError::new(&e.code, e.message)
}

fn db_err(e: rusqlite::Error) -> WireError {
    WireError::internal(e.to_string())
}

fn person_of_profile(conn: &Connection, profile_id: &str) -> Option<String> {
    conn.query_row(
        "SELECT hub_person_id FROM profiles WHERE id = ?1",
        [profile_id],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()
    .ok()
    .flatten()
    .flatten()
}

fn profile_of_person(conn: &Connection, person_id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT id FROM profiles WHERE hub_person_id = ?1",
        [person_id],
        |r| r.get(0),
    )
    .optional()
}

fn folders(conn: &Connection, sql: &str) -> rusqlite::Result<Vec<ProjectInfo>> {
    let mut st = conn.prepare(sql)?;
    let rows = st.query_map([], |r| {
        Ok(ProjectInfo {
            id: r.get(0)?,
            name: r.get(1)?,
            colour: r.get(2)?,
            kind: r.get(3)?,
            path: r.get(4)?,
        })
    })?;
    rows.collect()
}

pub struct HostPort {
    dispatcher: Arc<Dispatcher>,
}

impl HostPort {
    pub fn new(dispatcher: Arc<Dispatcher>) -> Arc<HostPort> {
        Arc::new(HostPort { dispatcher })
    }

    fn services(&self) -> &Arc<Services> {
        &self.dispatcher.ctx().services
    }

    fn db(&self) -> PortResult<Arc<Database>> {
        self.services()
            .get::<Database>()
            .ok_or_else(|| WireError::internal("The database is not available."))
    }

    fn rt(&self) -> PortResult<Arc<ChatRuntime>> {
        self.services()
            .get::<ChatRuntime>()
            .ok_or_else(|| WireError::internal("The chat runtime is not available."))
    }

    async fn op(&self, op: &str, args: Value) -> PortResult<Value> {
        self.dispatcher.call_host(op, args).await.map_err(wire)
    }

    fn people_and_projects(&self) -> PortResult<(Vec<PersonInfo>, Vec<ProjectInfo>)> {
        let db = self.db()?;
        db.snapshot(|tx| {
            let mut st = tx.prepare(
                "SELECT hub_person_id, name, colour, about FROM profiles
                 WHERE hub_person_id IS NOT NULL ORDER BY name",
            )?;
            let people = st
                .query_map([], |r| {
                    Ok(PersonInfo {
                        person_id: r.get(0)?,
                        name: r.get(1)?,
                        colour: r.get(2)?,
                        about: r.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            // Folders that are not archived; the archive flag lives in a table of the projects
            // crate, so a database without it lists every folder.
            let projects = folders(
                tx,
                "SELECT p.id, p.name, COALESCE(p.color, ''), p.kind, p.path
                 FROM projects p LEFT JOIN project_local l ON l.project_id = p.id
                 WHERE p.kind = 'folder' AND p.path IS NOT NULL AND COALESCE(l.archived, 0) = 0
                 ORDER BY p.name",
            )
            .or_else(|_| {
                folders(
                    tx,
                    "SELECT p.id, p.name, COALESCE(p.color, ''), p.kind, p.path
                     FROM projects p WHERE p.kind = 'folder' AND p.path IS NOT NULL
                     ORDER BY p.name",
                )
            })?;
            Ok::<_, rusqlite::Error>((people, projects))
        })
        .map_err(db_err)
    }
}

impl ChatPort for HostPort {
    fn device(&self) -> DeviceInfo {
        let none = || DeviceInfo {
            uid: String::new(),
            name: identity::FALLBACK_NAME.to_owned(),
        };
        let Ok(db) = self.db() else { return none() };
        // Read it; the row is made at start-up, so writing is only the fallback.
        let known = db
            .snapshot(|tx| {
                Ok::<_, rusqlite::Error>((identity::self_uid(tx), identity::self_name(tx)))
            })
            .ok();
        if let Some((Some(uid), Some(name))) = known {
            return DeviceInfo { uid, name };
        }
        match db.write(identity::ensure) {
            Ok((uid, name)) => DeviceInfo { uid, name },
            Err(e) => {
                log::warn!("devices: no device identity: {e}");
                none()
            }
        }
    }

    fn run_allowed(&self) -> bool {
        self.db()
            .ok()
            .and_then(|db| {
                db.snapshot(|tx| {
                    Ok::<_, rusqlite::Error>(helm_core::prefs::read(tx, "deviceRunFromOthers"))
                })
                .ok()
                .flatten()
            })
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    }

    fn hello(&self) -> BoxFut<'_, PortResult<Hello>> {
        Box::pin(async move {
            let (people, projects) = self.people_and_projects()?;
            let catalogue = self.op("model_catalogue", json!({})).await?;
            Ok(Hello {
                device: self.device(),
                people,
                projects,
                catalogue,
            })
        })
    }

    fn remote_chat(&self, record: ChatRecord) -> RemoteChat {
        let (person, shared, project) = self
            .db()
            .ok()
            .and_then(|db| {
                db.snapshot(|tx| {
                    let person = person_of_profile(tx, &record.profile_id);
                    let shared: Vec<String> = record
                        .shared_with
                        .iter()
                        .filter_map(|p| person_of_profile(tx, p))
                        .collect();
                    let project: Option<(String, String)> = tx
                        .query_row(
                            "SELECT name, COALESCE(color, '') FROM projects WHERE id = ?1",
                            [&record.project_id],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()?;
                    Ok::<_, rusqlite::Error>((person, shared, project))
                })
                .ok()
            })
            .unwrap_or((None, Vec::new(), None));
        let (project_name, project_colour) = project.unwrap_or_default();
        let waiting = record.unread.completion_id.is_some()
            || self
                .rt()
                .ok()
                .and_then(|rt| rt.live_thread(&record.id))
                .is_some_and(|t| !t.interactions.is_empty());
        RemoteChat::from_record(
            &record,
            person,
            shared,
            &project_name,
            &project_colour,
            waiting,
        )
    }

    fn chats(&self, since: i64) -> BoxFut<'_, PortResult<(Vec<RemoteChat>, i64)>> {
        Box::pin(async move {
            let rt = self.rt()?;
            let db = self.db()?;
            let cursor = rt.clock().current();
            let from = (since > 0).then(|| (since - CATCH_UP_SLACK_MS).max(1));
            let records = db
                .snapshot(|tx| store::list_for_devices(tx, cursor, from, LISTED))
                .map_err(db_err)?;
            let mut chats = Vec::new();
            let mut bytes = 0;
            for r in records {
                let c = self.remote_chat(r);
                bytes += c.to_value().to_string().len();
                if bytes > LIST_BYTES {
                    break;
                }
                chats.push(c);
            }
            Ok((chats, cursor))
        })
    }

    fn chat(&self, chat_id: String) -> BoxFut<'_, PortResult<ChatState>> {
        Box::pin(async move {
            let got = self.op("chat_get", json!({ "chatId": chat_id })).await?;
            let got: ChatGetResult = serde_json::from_value(got)
                .map_err(|e| WireError::internal(format!("chat_get: {e}")))?;
            let pending = self
                .db()?
                .snapshot(|tx| store::pending(tx, &chat_id))
                .map_err(db_err)?;
            Ok(ChatState {
                chat: self.remote_chat(got.chat),
                interactions: got.thread.interactions.clone(),
                thread: serde_json::to_value(&got.thread)
                    .map_err(|e| WireError::internal(e.to_string()))?,
                pending: serde_json::to_value(&pending)
                    .map_err(|e| WireError::internal(e.to_string()))?,
            })
        })
    }

    fn rows_since(
        &self,
        chat_id: String,
        since: i64,
        limit: i64,
    ) -> BoxFut<'_, PortResult<(Vec<MessageRow>, bool)>> {
        Box::pin(async move {
            self.db()?
                .snapshot(|tx| store::messages_since(tx, &chat_id, since, limit))
                .map_err(db_err)
        })
    }

    fn page(
        &self,
        chat_id: String,
        before: Option<i64>,
        limit: usize,
    ) -> BoxFut<'_, PortResult<Value>> {
        Box::pin(async move {
            let mut args = json!({ "chatId": chat_id });
            if let Some(b) = before {
                args["before"] = json!(b.to_string());
            }
            let mut page = self.op("transcript_page", args).await?;
            // The store's page is 100 rows; a smaller limit keeps the newest ones and points the
            // older cursor at the first row kept.
            let cut = match page.get_mut("messages").and_then(Value::as_array_mut) {
                Some(rows) if rows.len() > limit => {
                    let excess = rows.len() - limit;
                    rows.drain(..excess);
                    rows.first().and_then(|r| r.get("id")).and_then(Value::as_i64)
                }
                _ => None,
            };
            if let Some(first) = cut {
                page["hasOlder"] = json!(true);
                page["olderCursor"] = json!(first.to_string());
            }
            Ok(page)
        })
    }

    fn claim_send(&self, client_id: &str, chat_id: &str) -> PortResult<bool> {
        self.db()?
            .write(|tx| sends::claim(tx, client_id, chat_id, helm_core::now_ms()))
            .map_err(db_err)
    }

    fn release_send(&self, client_id: &str) {
        if let Ok(db) = self.db() {
            if let Err(e) = db.write(|tx| sends::release(tx, client_id)) {
                log::warn!("devices: a send record was not released: {e}");
            }
        }
    }

    fn send(&self, chat_id: String, client_id: String, text: String) -> BoxFut<'_, PortResult<()>> {
        Box::pin(async move {
            let rt = self.rt()?;
            let submission_id = format!("remote-{client_id}");
            tokio::task::spawn_blocking(move || {
                submit::host_send(
                    &rt,
                    HostSend {
                        chat_id: &chat_id,
                        submission_id: &submission_id,
                        text: &text,
                        origin: SubmissionOrigin::Remote,
                    },
                )
            })
            .await
            .map_err(|e| WireError::internal(e.to_string()))?
            .map(|_| ())
            .map_err(wire)
        })
    }

    fn stop(&self, chat_id: String) -> BoxFut<'_, PortResult<()>> {
        Box::pin(async move {
            self.op("chat_stop", json!({ "chatId": chat_id })).await?;
            Ok(())
        })
    }

    fn answer(&self, a: AnswerArgs) -> BoxFut<'_, PortResult<()>> {
        Box::pin(async move {
            let mut args = json!({ "chatId": a.chat_id, "requestId": a.request_id });
            if let Some(v) = &a.value {
                args["value"] = json!(v);
            }
            if let Some(c) = a.confirmed {
                args["confirmed"] = json!(c);
            }
            if let Some(c) = a.cancelled {
                args["cancelled"] = json!(c);
            }
            self.op("interaction_answer", args).await?;
            Ok(())
        })
    }

    fn create(&self, a: CreateArgs) -> BoxFut<'_, PortResult<String>> {
        Box::pin(async move {
            let db = self.db()?;
            let (profile, path) = db
                .snapshot(|tx| {
                    let profile = profile_of_person(tx, &a.person_id)?;
                    let path: Option<String> = tx
                        .query_row(
                            "SELECT path FROM projects WHERE id = ?1 AND kind = 'folder'",
                            [&a.project_id],
                            |r| r.get(0),
                        )
                        .optional()?
                        .flatten();
                    Ok::<_, rusqlite::Error>((profile, path))
                })
                .map_err(db_err)?;
            let profile = profile.ok_or_else(|| {
                WireError::new(
                    code::UNKNOWN_PERSON,
                    "No one on this PC is linked to that person.",
                )
            })?;
            let path = path.ok_or_else(|| {
                WireError::new(code::NOT_FOUND, "That project is not on this PC.")
            })?;
            let mut args = json!({ "cwd": path, "projectId": a.project_id });
            if let Some(m) = &a.model {
                args["model"] = json!(m);
            }
            if let Some(e) = &a.effort {
                args["effort"] = json!(e);
            }
            let made = self.op("chat_create", args).await?;
            let chat_id = made
                .get("id")
                .or_else(|| made.get("chat").and_then(|c| c.get("id")))
                .and_then(Value::as_str)
                .ok_or_else(|| WireError::internal("The new chat has no id."))?
                .to_owned();
            self.op(
                "profiles_assign_chats",
                json!({ "chatIds": [chat_id], "profileId": profile }),
            )
            .await?;
            Ok(chat_id)
        })
    }

    fn set_model(&self, a: SetModelArgs) -> BoxFut<'_, PortResult<()>> {
        Box::pin(async move {
            let mut args = json!({ "chatId": a.chat_id, "model": a.model });
            if let Some(e) = &a.effort {
                args["effort"] = json!(e);
            }
            self.op("chat_set_model", args).await?;
            Ok(())
        })
    }

    fn rename(&self, a: RenameArgs) -> BoxFut<'_, PortResult<()>> {
        Box::pin(async move {
            self.op("chat_rename", json!({ "chatId": a.chat_id, "title": a.title }))
                .await?;
            Ok(())
        })
    }

    fn archive(&self, a: ArchiveArgs) -> BoxFut<'_, PortResult<()>> {
        Box::pin(async move {
            let op = if a.archived { "chat_archive" } else { "chat_restore" };
            self.op(op, json!({ "chatId": a.chat_id })).await?;
            Ok(())
        })
    }

    fn share(&self, a: ShareArgs) -> BoxFut<'_, PortResult<()>> {
        Box::pin(async move {
            let profile = self
                .db()?
                .snapshot(|tx| profile_of_person(tx, &a.person_id))
                .map_err(db_err)?
                .ok_or_else(|| {
                    WireError::new(
                        code::UNKNOWN_PERSON,
                        "No one on this PC is linked to that person.",
                    )
                })?;
            self.op(
                "profiles_share_chats",
                json!({ "chatIds": [a.chat_id], "profileId": profile, "shared": a.shared }),
            )
            .await?;
            Ok(())
        })
    }
}
