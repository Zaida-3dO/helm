//! Test doubles for the device wire: a [`FakePort`] standing in for a whole install's chats, and
//! builders for the records it holds. Nothing here touches a database, a process or a network.

use crate::serve::{ChatPort, ChatState, PortResult};
use crate::link::BoxFut;
use crate::wire::{
    code, AnswerArgs, ArchiveArgs, CreateArgs, DeviceInfo, Hello, PersonInfo, ProjectInfo,
    RemoteChat, RenameArgs, SetModelArgs, ShareArgs, WireError,
};
use helm_chats::types::{
    ChatRecord, ChatStatus, Harness, Interaction, InteractionAttention, InteractionMethod,
    MessageDetails, MessageRole, MessageRow, MessageStatus, TitleState, UnreadState,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// A chat record with the fields a test cares about; the rest are plain defaults.
pub fn fake_record(id: &str, title: &str, revision: i64) -> ChatRecord {
    ChatRecord {
        id: id.to_owned(),
        project_id: "proj-1".to_owned(),
        cwd: "/work".to_owned(),
        title: title.to_owned(),
        status: ChatStatus::Idle,
        error: None,
        harness: Harness::Claude,
        model: None,
        effort: None,
        draft: "a private draft".to_owned(),
        draft_revision: 7,
        created_at: 1,
        updated_at: 1,
        revision,
        pinned: false,
        archived: false,
        parent_chat_id: None,
        side_source_id: None,
        title_state: TitleState::Fallback,
        unread: UnreadState::default(),
        muted: false,
        last_activity_at: 1,
        stalled: false,
        local_ahead: None,
        started: false,
        profile_id: "profile-local".to_owned(),
        host_device_id: "host".to_owned(),
        shared_with: Vec::new(),
    }
}

/// An assistant row with the given text at the given revision.
pub fn fake_row(chat_id: &str, id: i64, revision: i64, text: &str) -> MessageRow {
    MessageRow {
        id,
        chat_id: chat_id.to_owned(),
        source_key: format!("k{id}"),
        role: MessageRole::Assistant,
        text: text.to_owned(),
        thinking: String::new(),
        thinking_tail: None,
        tool_name: None,
        tool_call_id: None,
        status: MessageStatus::Complete,
        timestamp: id,
        sequence: id,
        revision,
        artifact: None,
        details: MessageDetails::default(),
    }
}

/// A request waiting in a chat.
pub fn fake_interaction(
    chat_id: &str,
    id: &str,
    method: InteractionMethod,
    attention: Option<InteractionAttention>,
    options: Option<&[&str]>,
) -> Interaction {
    Interaction {
        chat_id: chat_id.to_owned(),
        id: id.to_owned(),
        method,
        attention,
        title: None,
        header: None,
        message: None,
        context: None,
        options: options.map(|o| o.iter().map(|s| (*s).to_owned()).collect()),
        option_descriptions: None,
        allow_other: None,
        secret: None,
        step: None,
        steps: None,
        answer: None,
        prefill: None,
    }
}

/// What a [`FakePort`] was asked to do, and what it holds.
#[derive(Default)]
pub struct FakeState {
    pub chats: Vec<RemoteChat>,
    pub rows: HashMap<String, Vec<MessageRow>>,
    pub interactions: HashMap<String, Vec<Interaction>>,
    /// Person ids a `create` accepts.
    pub people: Vec<String>,
    pub claimed: HashSet<String>,
    pub sent: Vec<(String, String, String)>,
    pub stopped: Vec<String>,
    pub answered: Vec<AnswerArgs>,
    pub created: Vec<CreateArgs>,
    pub renamed: Vec<RenameArgs>,
    pub model_changes: Vec<SetModelArgs>,
    pub archived: Vec<ArchiveArgs>,
    pub shared: Vec<ShareArgs>,
    pub cursor: i64,
}

/// A whole install's chats in memory.
pub struct FakePort {
    uid: String,
    name: String,
    run: AtomicBool,
    pub state: Mutex<FakeState>,
}

impl FakePort {
    pub fn new(uid: &str, name: &str) -> FakePort {
        FakePort {
            uid: uid.to_owned(),
            name: name.to_owned(),
            run: AtomicBool::new(true),
            state: Mutex::new(FakeState {
                people: vec!["person-a".to_owned()],
                cursor: 100,
                ..Default::default()
            }),
        }
    }

    pub fn set_run(&self, on: bool) {
        self.run.store(on, Ordering::SeqCst);
    }

    pub fn st(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Add a chat to the list (as `person-a`'s).
    pub fn add_chat(&self, id: &str, title: &str, revision: i64) {
        let rc = self.remote_chat(fake_record(id, title, revision));
        self.st().chats.push(rc);
    }

    /// Set (or replace) a row of a chat.
    pub fn put_row(&self, row: MessageRow) {
        let mut st = self.st();
        let rows = st.rows.entry(row.chat_id.clone()).or_default();
        match rows.iter_mut().find(|r| r.id == row.id) {
            Some(r) => *r = row,
            None => rows.push(row),
        }
    }

    pub fn put_interaction(&self, i: Interaction) {
        self.st()
            .interactions
            .entry(i.chat_id.clone())
            .or_default()
            .push(i);
    }
}

fn ready<T: Send + 'static>(v: PortResult<T>) -> BoxFut<'static, PortResult<T>> {
    Box::pin(async move { v })
}

impl ChatPort for FakePort {
    fn device(&self) -> DeviceInfo {
        DeviceInfo {
            uid: self.uid.clone(),
            name: self.name.clone(),
        }
    }

    fn run_allowed(&self) -> bool {
        self.run.load(Ordering::SeqCst)
    }

    fn hello(&self) -> BoxFut<'_, PortResult<Hello>> {
        ready(Ok(Hello {
            device: self.device(),
            people: self
                .st()
                .people
                .iter()
                .map(|p| PersonInfo {
                    person_id: p.clone(),
                    name: format!("Person {p}"),
                    colour: "#6d8cff".to_owned(),
                    about: String::new(),
                })
                .collect(),
            projects: vec![ProjectInfo {
                id: "proj-1".to_owned(),
                name: "Work".to_owned(),
                colour: "#aabbcc".to_owned(),
                kind: "folder".to_owned(),
                path: "/work".to_owned(),
            }],
            catalogue: json!({ "models": [] }),
        }))
    }

    fn remote_chat(&self, record: ChatRecord) -> RemoteChat {
        RemoteChat::from_record(
            &record,
            Some("person-a".to_owned()),
            Vec::new(),
            "Work",
            "#aabbcc",
            false,
        )
    }

    fn chats(&self, _since: i64) -> BoxFut<'_, PortResult<(Vec<RemoteChat>, i64)>> {
        let st = self.st();
        ready(Ok((st.chats.clone(), st.cursor)))
    }

    fn chat(&self, chat_id: String) -> BoxFut<'_, PortResult<ChatState>> {
        let st = self.st();
        let r = match st.chats.iter().find(|c| c.id() == chat_id) {
            Some(chat) => Ok(ChatState {
                chat: chat.clone(),
                thread: json!({
                    "chatId": chat_id,
                    "interactions": st.interactions.get(&chat_id).cloned().unwrap_or_default(),
                }),
                pending: json!([]),
                interactions: st.interactions.get(&chat_id).cloned().unwrap_or_default(),
            }),
            None => Err(WireError::not_found()),
        };
        ready(r)
    }

    fn rows_since(
        &self,
        chat_id: String,
        since: i64,
        limit: i64,
    ) -> BoxFut<'_, PortResult<(Vec<MessageRow>, bool)>> {
        let st = self.st();
        let mut rows: Vec<MessageRow> = st
            .rows
            .get(&chat_id)
            .map(|r| r.iter().filter(|m| m.revision > since).cloned().collect())
            .unwrap_or_default();
        rows.sort_by_key(|r| (r.timestamp, r.id));
        let more = rows.len() as i64 > limit;
        let skip = rows.len().saturating_sub(limit as usize);
        ready(Ok((rows.split_off(skip), more)))
    }

    fn page(
        &self,
        chat_id: String,
        before: Option<i64>,
        limit: usize,
    ) -> BoxFut<'_, PortResult<Value>> {
        let st = self.st();
        let mut rows: Vec<&MessageRow> = st
            .rows
            .get(&chat_id)
            .map(|r| r.iter().filter(|m| before.is_none_or(|b| m.id < b)).collect())
            .unwrap_or_default();
        rows.sort_by_key(|r| (r.timestamp, r.id));
        let skip = rows.len().saturating_sub(limit);
        let messages: Vec<Value> = rows[skip..]
            .iter()
            .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
            .collect();
        ready(Ok(json!({ "chatId": chat_id, "messages": messages, "hasOlder": skip > 0 })))
    }

    fn claim_send(&self, client_id: &str, _chat_id: &str) -> PortResult<bool> {
        Ok(self.st().claimed.insert(client_id.to_owned()))
    }

    fn release_send(&self, client_id: &str) {
        self.st().claimed.remove(client_id);
    }

    fn send(&self, chat_id: String, client_id: String, text: String) -> BoxFut<'_, PortResult<()>> {
        let mut st = self.st();
        let r = if st.chats.iter().any(|c| c.id() == chat_id) {
            st.sent.push((chat_id, client_id, text));
            Ok(())
        } else {
            Err(WireError::not_found())
        };
        ready(r)
    }

    fn stop(&self, chat_id: String) -> BoxFut<'_, PortResult<()>> {
        self.st().stopped.push(chat_id);
        ready(Ok(()))
    }

    fn answer(&self, a: AnswerArgs) -> BoxFut<'_, PortResult<()>> {
        let mut st = self.st();
        // The chat takes the request away, as the real one does.
        if let Some(list) = st.interactions.get_mut(&a.chat_id) {
            list.retain(|i| i.id != a.request_id);
        }
        st.answered.push(a);
        ready(Ok(()))
    }

    fn create(&self, a: CreateArgs) -> BoxFut<'_, PortResult<String>> {
        let mut st = self.st();
        let r = if st.people.contains(&a.person_id) {
            let id = format!("new-{}", st.created.len() + 1);
            st.created.push(a);
            Ok(id)
        } else {
            Err(WireError::new(
                code::UNKNOWN_PERSON,
                "No one on this PC is linked to that person.",
            ))
        };
        ready(r)
    }

    fn set_model(&self, a: SetModelArgs) -> BoxFut<'_, PortResult<()>> {
        self.st().model_changes.push(a);
        ready(Ok(()))
    }

    fn rename(&self, a: RenameArgs) -> BoxFut<'_, PortResult<()>> {
        self.st().renamed.push(a);
        ready(Ok(()))
    }

    fn archive(&self, a: ArchiveArgs) -> BoxFut<'_, PortResult<()>> {
        self.st().archived.push(a);
        ready(Ok(()))
    }

    fn share(&self, a: ShareArgs) -> BoxFut<'_, PortResult<()>> {
        self.st().shared.push(a);
        ready(Ok(()))
    }
}
