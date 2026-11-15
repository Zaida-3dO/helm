//! Test doubles for the device wire: a [`FakePort`] standing in for a whole install's chats, and
//! builders for the records it holds. Nothing here touches a database, a process or a network.

use crate::link::BoxFut;
use crate::serve::{ChatPort, ChatState, PortResult, SentFile};
use crate::wire::{
    code, AnswerArgs, ArchiveArgs, CheckpointArgs, ConsentDecideArgs, CreateArgs, DefaultsArgs,
    DeviceInfo, DevicePerson, ForkArgs, Hello, MoveProfileArgs, MuteArgs, OrganizeArgs, PersonInfo,
    ProfileInfo, ProjectInfo, QuickOpenArgs, RemoteChat, RenameArgs, RollOverArgs, SetModelArgs,
    SetPermissionArgs, ShareArgs, SnoozeArgs, SubmissionArgs, ToolStopArgs, VerbositySetArgs,
    WireError,
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
        gate: None,
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
    /// For each send, the household member it came from when the chat was not theirs.
    pub sent_from: Vec<Option<String>>,
    /// For each send, the files it carried.
    pub sent_files: Vec<Vec<SentFile>>,
    pub stopped: Vec<String>,
    pub answered: Vec<AnswerArgs>,
    /// The pending Allow cards of each chat, as the cards the consent store would show.
    pub consent: HashMap<String, Vec<Value>>,
    /// The Allow decisions taken from another device: (chat, request id, decision, where from).
    pub consents: Vec<(String, String, String, String)>,
    pub created: Vec<CreateArgs>,
    pub renamed: Vec<RenameArgs>,
    pub model_changes: Vec<SetModelArgs>,
    pub archived: Vec<ArchiveArgs>,
    pub shared: Vec<ShareArgs>,
    /// For each fork: (chat id, fork id).
    pub forked: Vec<(String, String)>,
    pub compacted: Vec<String>,
    pub rolled_over: Vec<RollOverArgs>,
    pub cost_for: Vec<String>,
    pub exported: Vec<String>,
    pub organized: Vec<OrganizeArgs>,
    pub unread_marked: Vec<String>,
    pub muted: Vec<MuteArgs>,
    pub profiles_moved: Vec<MoveProfileArgs>,
    pub permission_set: Vec<SetPermissionArgs>,
    pub verbosity_set: Vec<VerbositySetArgs>,
    pub context_asked: Vec<String>,
    pub quick_opened: Vec<QuickOpenArgs>,
    pub warmed: Vec<String>,
    pub tool_stopped: Vec<ToolStopArgs>,
    pub submissions: Vec<SubmissionArgs>,
    pub checkpoint_for: Vec<CheckpointArgs>,
    pub attention_for: Vec<Vec<String>>,
    pub item_links_for: Vec<Vec<String>>,
    pub snoozed: Vec<SnoozeArgs>,
    pub addressed: Vec<String>,
    /// What `defaults_set` saved, as the three-field answer it would give back.
    pub defaults_saved: Option<Value>,
    pub cursor: i64,
    /// The hub people this install announces acting for (what `people()` answers).
    pub announce: Vec<DevicePerson>,
}

/// A whole install's chats in memory.
pub struct FakePort {
    uid: String,
    name: String,
    run: AtomicBool,
    household: AtomicBool,
    pub state: Mutex<FakeState>,
}

impl FakePort {
    pub fn new(uid: &str, name: &str) -> FakePort {
        FakePort {
            uid: uid.to_owned(),
            name: name.to_owned(),
            run: AtomicBool::new(true),
            household: AtomicBool::new(true),
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

    pub fn set_household(&self, on: bool) {
        self.household.store(on, Ordering::SeqCst);
    }

    pub fn st(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Add a chat to the list (as `person-a`'s).
    pub fn add_chat(&self, id: &str, title: &str, revision: i64) {
        let rc = self.remote_chat(fake_record(id, title, revision));
        self.st().chats.push(rc);
    }

    /// Add a chat that belongs to `person` and is shared with `shared`.
    pub fn add_chat_of(&self, id: &str, title: &str, revision: i64, person: &str, shared: &[&str]) {
        let rc = RemoteChat::from_record(
            &fake_record(id, title, revision),
            Some(person.to_owned()),
            shared.iter().map(|p| (*p).to_owned()).collect(),
            "Work",
            "#aabbcc",
            false,
        );
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

    /// File an Allow card on a chat, as `app_request_allow` would.
    pub fn put_consent(&self, chat_id: &str, card: Value) {
        self.st()
            .consent
            .entry(chat_id.to_owned())
            .or_default()
            .push(card);
    }

    /// Announce the hub people this install acts for (the home person's channel flags first).
    pub fn announce_people(&self, people: &[(&str, bool, bool)]) {
        self.st().announce = people
            .iter()
            .map(|(p, telegram, voice)| DevicePerson {
                person_id: (*p).to_owned(),
                telegram: *telegram,
                voice: *voice,
            })
            .collect();
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

    fn household_run(&self) -> bool {
        self.household.load(Ordering::SeqCst)
    }

    fn people(&self) -> Vec<DevicePerson> {
        self.st().announce.clone()
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
            profiles: vec![
                ProfileInfo {
                    profile_id: "profile-local".to_owned(),
                    name: "Local".to_owned(),
                    person_id: Some("person-a".to_owned()),
                },
                ProfileInfo {
                    profile_id: "profile-p".to_owned(),
                    name: "Person P".to_owned(),
                    person_id: None,
                },
            ],
            catalogue: Some(json!({ "models": [] })),
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
                consent: st.consent.get(&chat_id).cloned().unwrap_or_default(),
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
            .map(|r| {
                r.iter()
                    .filter(|m| before.is_none_or(|b| m.id < b))
                    .collect()
            })
            .unwrap_or_default();
        rows.sort_by_key(|r| (r.timestamp, r.id));
        let skip = rows.len().saturating_sub(limit);
        let messages: Vec<Value> = rows[skip..]
            .iter()
            .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
            .collect();
        ready(Ok(
            json!({ "chatId": chat_id, "messages": messages, "hasOlder": skip > 0 }),
        ))
    }

    fn claim_send(&self, client_id: &str, _chat_id: &str) -> PortResult<bool> {
        Ok(self.st().claimed.insert(client_id.to_owned()))
    }

    fn release_send(&self, client_id: &str) {
        self.st().claimed.remove(client_id);
    }

    fn send(
        &self,
        chat_id: String,
        client_id: String,
        text: String,
        from: Option<String>,
        files: Vec<SentFile>,
    ) -> BoxFut<'_, PortResult<()>> {
        let mut st = self.st();
        let r = if st.chats.iter().any(|c| c.id() == chat_id) {
            st.sent.push((chat_id, client_id, text));
            st.sent_from.push(from);
            st.sent_files.push(files);
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

    fn consent(&self, chat_id: String) -> BoxFut<'_, PortResult<Vec<Value>>> {
        let st = self.st();
        ready(Ok(st.consent.get(&chat_id).cloned().unwrap_or_default()))
    }

    fn consent_decide(&self, a: ConsentDecideArgs, via: String) -> BoxFut<'_, PortResult<()>> {
        let mut st = self.st();
        // One compare-and-set out of pending, as the consent store does: a second answer to
        // the same card is refused, and the card leaves the list.
        let list = st.consent.entry(a.chat_id.clone()).or_default();
        let at = list
            .iter()
            .position(|c| c["id"].as_str() == Some(&a.request_id));
        let r = match at {
            Some(at) => {
                list.remove(at);
                st.consents.push((
                    a.chat_id.clone(),
                    a.request_id.clone(),
                    a.decision.clone(),
                    via,
                ));
                Ok(())
            }
            // The real store answers a late decide with `conflict`.
            None => Err(WireError::new(
                "conflict",
                "This request was already answered or has expired.",
            )),
        };
        ready(r)
    }

    fn consent_states(&self, chat_id: String) -> BoxFut<'_, PortResult<Vec<Value>>> {
        let st = self.st();
        let mut out: Vec<Value> = st
            .consent
            .get(&chat_id)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|c| {
                json!({
                    "id": c["id"],
                    "state": c["state"].as_str().unwrap_or("pending"),
                    "outcome": Value::Null,
                })
            })
            .collect();
        for (_, id, decision, _) in st.consents.iter().filter(|(c, ..)| *c == chat_id) {
            out.push(json!({
                "id": id,
                "state": if decision == "allow" { "allowed" } else { "denied" },
                "outcome": Value::Null,
            }));
        }
        ready(Ok(out))
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

    fn fork(&self, a: ForkArgs) -> BoxFut<'_, PortResult<Value>> {
        let mut st = self.st();
        let r = if st.chats.iter().any(|c| c.id() == a.chat_id) {
            st.forked.push((a.chat_id.clone(), a.fork_id));
            Ok(json!({ "chat": { "id": format!("{}-copy", a.chat_id) } }))
        } else {
            Err(WireError::not_found())
        };
        ready(r)
    }

    fn compact(&self, chat_id: String) -> BoxFut<'_, PortResult<()>> {
        self.st().compacted.push(chat_id);
        ready(Ok(()))
    }

    fn roll_over(&self, a: RollOverArgs) -> BoxFut<'_, PortResult<Value>> {
        self.st().rolled_over.push(a);
        ready(Ok(json!({ "started": true })))
    }

    fn cost(&self, chat_id: String) -> BoxFut<'_, PortResult<Value>> {
        self.st().cost_for.push(chat_id.clone());
        ready(Ok(json!({ "chatId": chat_id, "totalTokens": 1234 })))
    }

    fn export_markdown(&self, chat_id: String) -> BoxFut<'_, PortResult<Value>> {
        self.st().exported.push(chat_id.clone());
        ready(Ok(
            json!({ "messages": 3, "path": format!("{chat_id}.md") }),
        ))
    }

    fn organize(&self, a: OrganizeArgs) -> BoxFut<'_, PortResult<Value>> {
        // The host answers with the changed sidebar rows; echo what the request set, so the
        // watcher can show the new placement before the device's own news arrives.
        let chat_id = a.chat_id.clone();
        let mut row = json!({ "chatId": chat_id, "groupProjectId": "proj-1", "position": 3 });
        if let Some(pinned) = a.pinned {
            row["pinned"] = json!(pinned);
        }
        if let Some(parent) = a.parent_chat_id.clone() {
            row["parentChatId"] = json!(parent);
        }
        self.st().organized.push(a);
        ready(Ok(json!([row])))
    }

    fn mark_unread(&self, chat_id: String) -> BoxFut<'_, PortResult<()>> {
        self.st().unread_marked.push(chat_id);
        ready(Ok(()))
    }

    fn set_muted(&self, a: MuteArgs) -> BoxFut<'_, PortResult<()>> {
        self.st().muted.push(a);
        ready(Ok(()))
    }

    fn move_profile(&self, a: MoveProfileArgs) -> BoxFut<'_, PortResult<()>> {
        self.st().profiles_moved.push(a);
        ready(Ok(()))
    }

    fn set_permission(&self, a: SetPermissionArgs) -> BoxFut<'_, PortResult<()>> {
        self.st().permission_set.push(a);
        ready(Ok(()))
    }

    fn verbosity_get(&self, chat_id: String) -> BoxFut<'_, PortResult<Value>> {
        let level = self
            .st()
            .verbosity_set
            .iter()
            .rev()
            .find(|v| v.chat_id == chat_id)
            .and_then(|v| v.level.clone())
            .unwrap_or_else(|| "low".to_owned());
        let own = self
            .st()
            .verbosity_set
            .iter()
            .any(|v| v.chat_id == chat_id && v.level.is_some());
        ready(Ok(json!({ "level": level, "own": own, "default": "low" })))
    }

    fn verbosity_set(&self, a: VerbositySetArgs) -> BoxFut<'_, PortResult<Value>> {
        let own = a.level.is_some();
        let level = a.level.clone().unwrap_or_else(|| "low".to_owned());
        self.st().verbosity_set.push(a);
        ready(Ok(json!({ "level": level, "own": own, "default": "low" })))
    }

    fn context_on(&self, chat_id: String) -> BoxFut<'_, PortResult<Value>> {
        self.st().context_asked.push(chat_id.clone());
        ready(Ok(json!({ "on": false })))
    }

    fn quick_open(&self, a: QuickOpenArgs) -> BoxFut<'_, PortResult<Value>> {
        let hit = format!("src/{}.ts", a.query);
        self.st().quick_opened.push(a);
        ready(Ok(
            json!({ "mode": "files", "items": [{ "path": hit }], "partial": false }),
        ))
    }

    fn warm(&self, chat_id: String) -> BoxFut<'_, PortResult<()>> {
        self.st().warmed.push(chat_id);
        ready(Ok(()))
    }

    fn tool_stop(&self, a: ToolStopArgs) -> BoxFut<'_, PortResult<Value>> {
        self.st().tool_stopped.push(a);
        ready(Ok(json!({ "stopped": true })))
    }

    fn submission(&self, a: SubmissionArgs) -> BoxFut<'_, PortResult<()>> {
        if !matches!(a.action.as_str(), "withdraw" | "recover") {
            return ready(Err(WireError::bad_args(
                "An action is withdraw or recover.",
            )));
        }
        self.st().submissions.push(a);
        ready(Ok(()))
    }

    fn checkpoint(&self, a: CheckpointArgs) -> BoxFut<'_, PortResult<Value>> {
        self.st().checkpoint_for.push(a.clone());
        ready(Ok(json!({
            "chatId": a.chat_id,
            "text": "back to here",
            "userMessages": 1,
            "files": [],
            "filesRestored": false,
        })))
    }

    fn attention_rows(&self, chat_ids: Vec<String>) -> BoxFut<'_, PortResult<Value>> {
        let flagged: Vec<Value> = self
            .st()
            .chats
            .iter()
            .filter(|c| chat_ids.iter().any(|id| id == c.id()))
            .map(|c| json!({ "chatId": c.id(), "kind": "idle", "reason": "quiet but unfinished" }))
            .collect();
        ready(Ok(json!({ "needsYou": [], "flagged": flagged })))
    }

    fn item_links(&self, chat_ids: Vec<String>) -> BoxFut<'_, PortResult<Value>> {
        let links: Vec<Value> = chat_ids
            .iter()
            .map(|id| json!({ "chatId": id, "itemId": "item-7", "title": "Task seven", "main": true }))
            .collect();
        self.st().item_links_for.push(chat_ids.clone());
        ready(Ok(json!({ "links": links })))
    }

    fn snooze(&self, a: SnoozeArgs) -> BoxFut<'_, PortResult<()>> {
        self.st().snoozed.push(a);
        ready(Ok(()))
    }

    fn address(&self, chat_id: String) -> BoxFut<'_, PortResult<()>> {
        self.st().addressed.push(chat_id);
        ready(Ok(()))
    }

    fn defaults_get(&self, _chat_id: String) -> BoxFut<'_, PortResult<Value>> {
        let saved = self
            .st()
            .defaults_saved
            .clone()
            .unwrap_or_else(|| json!({}));
        ready(Ok(json!({
            "model": saved.get("model").cloned().unwrap_or(Value::Null),
            "effort": "high",
            "responseVerbosity": saved.get("responseVerbosity").cloned().unwrap_or(json!("low")),
        })))
    }

    fn defaults_set(&self, a: DefaultsArgs) -> BoxFut<'_, PortResult<Value>> {
        let prev = self
            .st()
            .defaults_saved
            .clone()
            .unwrap_or_else(|| json!({}));
        let mut saved = prev.as_object().cloned().unwrap_or_default();
        if let Some(m) = &a.model {
            saved.insert("model".into(), json!(m));
        }
        if let Some(e) = &a.effort {
            saved.insert("effort".into(), json!(e));
        }
        if let Some(v) = &a.response_verbosity {
            if !matches!(v.as_str(), "low" | "medium" | "high") {
                return ready(Err(WireError::bad_args(
                    "Reply length is low, medium or high.",
                )));
            }
            saved.insert("responseVerbosity".into(), json!(v));
        }
        let saved = Value::Object(saved);
        self.st().defaults_saved = Some(saved.clone());
        ready(Ok(json!({
            "model": saved.get("model").cloned().unwrap_or(Value::Null),
            "responseVerbosity": saved.get("responseVerbosity").cloned().unwrap_or(json!("low")),
        })))
    }

    fn chat_brief(&self, chat_id: String) -> BoxFut<'_, PortResult<Option<RemoteChat>>> {
        let st = self.st();
        ready(Ok(st.chats.iter().find(|c| c.id() == chat_id).cloned()))
    }
}
