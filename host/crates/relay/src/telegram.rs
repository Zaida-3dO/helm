//! The Telegram side of the line, on the PC.
//!
//! - **Origin.** A message from the bot enters its chat with origin `telegram`
//!   ([`TELEGRAM_ORIGIN`]), never as typed. Helm's consent check counts only typed messages, so a
//!   Telegram message never counts as consent, whatever it says (trust-boundary item e).
//! - **The active Telegram chat.** One chat carries the tag. An inbound message attaches to it;
//!   with no tag, or once the tag has expired (age or token threshold, or the chat was archived
//!   or deleted), Helm starts a new chat named `Telegram <bot name> <date>`. The tag is a small
//!   file in the profile, `relay/telegram-chat.json`.
//! - **Queued messages** arrive marked with how long ago they were sent.
//! - **The limits** (age and tokens) are set in Settings → Hub and kept beside the tag, in
//!   `relay/telegram-policy.json`; Settings can also drop the tag so the next message starts fresh.
//! - **Answered questions.** The hub ids of replies that answered a question are kept in
//!   `relay/telegram-answered.json`, so a resend after a restart is not taken as a new prompt.
//!
//! - **Rendering** for the phone: a short status line while a turn runs, the final answer cut
//!   with a "reply more" hint, questions as numbered options. No thinking, no tool cards.
//!
//! Delivering into a chat goes through [`ChatPort`]; the real one and the mirror are in
//! [`crate::sink`].

use helm_chats::types::SubmissionOrigin;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The origin every Telegram message is stored with.
pub const TELEGRAM_ORIGIN: SubmissionOrigin = SubmissionOrigin::Telegram;

/// When the tag drops on its own. Defaults: a day, or 400k tokens used in the chat.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TagPolicy {
    pub max_age_ms: i64,
    pub max_tokens: u64,
}

const HOUR_MS: i64 = 60 * 60 * 1000;
/// The limits Settings accepts: an hour to 30 days, 10k to 5M tokens.
pub const AGE_HOURS: std::ops::RangeInclusive<i64> = 1..=720;
pub const TOKENS: std::ops::RangeInclusive<u64> = 10_000..=5_000_000;

impl Default for TagPolicy {
    fn default() -> Self {
        TagPolicy {
            max_age_ms: 24 * HOUR_MS,
            max_tokens: 400_000,
        }
    }
}

/// The limits as stored and as Settings shows them.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyFile {
    pub max_age_hours: i64,
    pub max_tokens: u64,
}

impl From<TagPolicy> for PolicyFile {
    fn from(p: TagPolicy) -> Self {
        PolicyFile {
            max_age_hours: p.max_age_ms / HOUR_MS,
            max_tokens: p.max_tokens,
        }
    }
}

impl PolicyFile {
    /// Refuses a value outside the ranges Settings offers.
    pub fn check(self) -> Result<TagPolicy, String> {
        if !AGE_HOURS.contains(&self.max_age_hours) {
            return Err("The age limit must be 1 to 720 hours.".into());
        }
        if !TOKENS.contains(&self.max_tokens) {
            return Err("The token limit must be 10,000 to 5,000,000.".into());
        }
        Ok(TagPolicy {
            max_age_ms: self.max_age_hours * HOUR_MS,
            max_tokens: self.max_tokens,
        })
    }
}

pub fn policy_path(profile_dir: &Path) -> PathBuf {
    profile_dir.join("relay").join("telegram-policy.json")
}

/// The saved limits, or the defaults when none are saved (or the file is unreadable).
pub fn load_policy(path: &Path) -> TagPolicy {
    helm_core::privfs::read_capped(path, 4 * 1024)
        .ok()
        .and_then(|b| serde_json::from_slice::<PolicyFile>(&b).ok())
        .and_then(|f| f.check().ok())
        .unwrap_or_default()
}

pub fn save_policy(path: &Path, policy: TagPolicy) -> std::io::Result<()> {
    write_private(path, &PolicyFile::from(policy))
}

fn write_private<T: Serialize>(path: &Path, v: &T) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        helm_core::privfs::create_private_dir_all(dir)?;
    }
    let bytes = serde_json::to_vec(v).map_err(std::io::Error::other)?;
    helm_core::privfs::write_atomic(path, &bytes)
}

/// How many answered hub ids are remembered; a resend comes long before this many replies.
pub const ANSWERED_KEPT: usize = 256;

pub fn answered_path(profile_dir: &Path) -> PathBuf {
    profile_dir.join("relay").join("telegram-answered.json")
}

pub fn load_answered(path: &Path) -> std::collections::VecDeque<String> {
    helm_core::privfs::read_capped(path, 64 * 1024)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Remember one more answered hub id (the oldest go past [`ANSWERED_KEPT`]) and save the list.
pub fn remember_answered(
    path: &Path,
    list: &mut std::collections::VecDeque<String>,
    id: &str,
) -> std::io::Result<()> {
    list.push_back(id.to_owned());
    while list.len() > ANSWERED_KEPT {
        list.pop_front();
    }
    write_private(path, list)
}

/// The tagged chat, as stored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveTag {
    pub chat_id: String,
    pub started_at: i64,
    /// The chat's token count when it was tagged, so the threshold counts Telegram use only.
    pub tokens_at_start: u64,
}

/// What Helm knows about a chat, for the tag check.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChatFacts {
    pub archived: bool,
    pub tokens: u64,
}

/// The chats side, as this module needs it.
pub trait ChatPort {
    /// `None` when the chat no longer exists.
    fn facts(&self, chat_id: &str) -> Option<ChatFacts>;
    /// Start a new chat with this title; its id.
    fn create(&self, title: &str) -> Result<String, String>;
}

/// The tag file inside the profile.
pub fn tag_path(profile_dir: &Path) -> PathBuf {
    profile_dir.join("relay").join("telegram-chat.json")
}

pub fn load_tag(path: &Path) -> Option<ActiveTag> {
    let bytes = helm_core::privfs::read_capped(path, 64 * 1024).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn save_tag(path: &Path, tag: Option<&ActiveTag>) -> std::io::Result<()> {
    match tag {
        None => match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
        Some(t) => write_private(path, t),
    }
}

/// Why the current tag no longer holds, or `None` when it does.
pub fn expired(
    tag: &ActiveTag,
    facts: Option<ChatFacts>,
    policy: &TagPolicy,
    now: i64,
) -> Option<&'static str> {
    let Some(f) = facts else {
        return Some("the chat is gone");
    };
    if f.archived {
        return Some("the chat was archived");
    }
    if now - tag.started_at >= policy.max_age_ms {
        return Some("the time limit passed");
    }
    if f.tokens.saturating_sub(tag.tokens_at_start) >= policy.max_tokens {
        return Some("the token limit passed");
    }
    None
}

/// The chat an inbound message goes to: the tagged one while it holds, else a new one that takes
/// the tag. Returns the chat id and whether it was just created.
pub fn active_chat(
    path: &Path,
    port: &dyn ChatPort,
    policy: &TagPolicy,
    bot: Option<&str>,
    now: i64,
) -> Result<(String, bool), String> {
    if let Some(tag) = load_tag(path) {
        if expired(&tag, port.facts(&tag.chat_id), policy, now).is_none() {
            return Ok((tag.chat_id, false));
        }
    }
    let id = port.create(&chat_title(bot, now))?;
    let tokens_at_start = port.facts(&id).map(|f| f.tokens).unwrap_or(0);
    let tag = ActiveTag {
        chat_id: id.clone(),
        started_at: now,
        tokens_at_start,
    };
    save_tag(path, Some(&tag))
        .map_err(|_| "The Telegram chat tag could not be saved.".to_owned())?;
    Ok((id, true))
}

/// `Telegram <bot name> <YYYY-MM-DD>` (UTC date).
pub fn chat_title(bot: Option<&str>, now: i64) -> String {
    let bot: String = bot
        .unwrap_or("bot")
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    let (y, m, d) = civil_date(now.div_euclid(86_400_000));
    format!("Telegram {bot} {y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 to a calendar date (proleptic Gregorian).
fn civil_date(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

/// The text a Telegram message enters its chat with. A queued message is marked with its age.
pub fn inbound_text(text: &str, age_minutes: Option<i64>) -> String {
    match age_minutes {
        Some(n) if n >= 1 => format!("(sent {n} min ago, queued while Helm was offline)\n{text}"),
        _ => text.to_owned(),
    }
}

/// The submission id for a hub message id: the same message delivered twice (a reconnect before
/// the acknowledgement) maps to the same submission. A UUID, as workers require (version 8, from
/// a SHA-256 of the hub id).
pub fn submission_id(hub_id: &str) -> String {
    use sha2::Digest;
    let h = sha2::Sha256::digest(format!("helm-telegram/{hub_id}"));
    let mut b = [0u8; 16];
    b.copy_from_slice(&h[..16]);
    uuid::Uuid::new_v8(b).to_string()
}

/// The note every Telegram message carries in front of its text, so the agent answers in a form
/// a phone can show. It is guidance only: the message's origin, not this text, is what keeps it
/// from ever counting as consent.
pub const BRIEF_NOTE: &str = "[Sent from Telegram: answer briefly in plain text, with no tables \
or long code. Offer choices as a numbered list. The user can reply \"more\" for detail.]";

/// The text a Telegram message is stored with: the brief note, then the (age-marked) message.
pub fn delivered_text(text: &str, age_minutes: Option<i64>) -> String {
    format!("{BRIEF_NOTE}\n{}", inbound_text(text, age_minutes))
}

/// The longest final answer sent to Telegram before it is cut (Telegram's own cap is 4096).
pub const MAX_FINAL_CHARS: usize = 3000;
/// The longest status message.
pub const MAX_STATUS_CHARS: usize = 400;

fn cut_chars(s: &str, max: usize) -> (String, bool) {
    let mut it = s.chars();
    let out: String = it.by_ref().take(max).collect();
    (out, it.next().is_some())
}

/// The live status line while a turn runs: what is running now and the last few tools, by name
/// only (no arguments, no thinking, no tool output).
pub fn render_status(running: &[String], recent: &[String]) -> String {
    let mut s = String::from("Working…");
    if !running.is_empty() {
        s.push_str(&format!("\nRunning: {}", running.join(", ")));
    }
    if !recent.is_empty() {
        s.push_str(&format!("\nRecent: {}", recent.join(", ")));
    }
    cut_chars(&s, MAX_STATUS_CHARS).0
}

/// The turn's final answer: the last assistant text, trimmed and cut, with a "reply more" hint
/// when it was cut.
pub fn render_final(text: Option<&str>) -> String {
    let t = text.map(str::trim).filter(|t| !t.is_empty());
    let Some(t) = t else {
        return "Done (no text answer).".to_owned();
    };
    let (out, cut) = cut_chars(t, MAX_FINAL_CHARS);
    if cut {
        format!("{out}…\n\n(Cut short: reply \"more\" for the rest.)")
    } else {
        out
    }
}

/// A question the agent asked, as a Telegram message: the question, then its options numbered
/// from 1. Replying with a number picks that option; any other reply is the answer as typed.
pub fn render_question(title: Option<&str>, message: Option<&str>, options: &[String]) -> String {
    let mut s = String::new();
    for part in [title, message].into_iter().flatten() {
        let p = part.trim();
        if !p.is_empty() && !s.contains(p) {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(p);
        }
    }
    if s.is_empty() {
        s.push_str("A question:");
    }
    for (i, o) in options.iter().enumerate() {
        s.push_str(&format!("\n{}. {}", i + 1, o));
    }
    s.push_str("\n\nReply with a number, or type an answer.");
    cut_chars(&s, MAX_FINAL_CHARS).0
}

/// What the phone is told when the turn waits on something only Helm's window may answer (an
/// approval, a plan or a trust prompt), so it is not left reading "Working…".
pub fn render_blocked(title: Option<&str>, message: Option<&str>) -> String {
    let what = [title, message]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|t| !t.is_empty());
    let mut s = String::from("Waiting for you in Helm");
    if let Some(w) = what {
        s.push_str(": ");
        s.push_str(w);
    }
    s.push_str(
        "\n\nThis can only be answered in the app, not from Telegram. /stop stops the turn.",
    );
    cut_chars(&s, MAX_FINAL_CHARS).0
}

/// The reply that tells the phone its message answered the waiting question.
pub fn render_answered(value: &str) -> String {
    let (v, cut) = cut_chars(value.trim(), 200);
    format!("Answered: {v}{}", if cut { "…" } else { "" })
}

/// The answer a Telegram reply gives to a pending question: a number in range picks that option,
/// anything else is the typed answer.
pub fn question_answer(options: &[String], reply: &str) -> String {
    let r = reply.trim();
    match r.parse::<usize>() {
        Ok(n) if (1..=options.len()).contains(&n) => options[n - 1].clone(),
        _ => r.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    const T0: i64 = 1_800_000_000_000; // 2027-01-15

    #[derive(Default)]
    struct Port {
        chats: RefCell<HashMap<String, ChatFacts>>,
        created: RefCell<Vec<String>>,
    }

    impl ChatPort for Port {
        fn facts(&self, id: &str) -> Option<ChatFacts> {
            self.chats.borrow().get(id).copied()
        }
        fn create(&self, title: &str) -> Result<String, String> {
            let id = format!("chat-{}", self.created.borrow().len() + 1);
            self.created.borrow_mut().push(title.to_owned());
            self.chats.borrow_mut().insert(
                id.clone(),
                ChatFacts {
                    archived: false,
                    tokens: 0,
                },
            );
            Ok(id)
        }
    }

    #[test]
    fn the_first_message_starts_a_named_chat_and_later_ones_attach() {
        let dir = tempfile::tempdir().unwrap();
        let path = tag_path(dir.path());
        let port = Port::default();
        let p = TagPolicy::default();
        assert_eq!(
            active_chat(&path, &port, &p, Some("helm_bot"), T0).unwrap(),
            ("chat-1".into(), true)
        );
        assert_eq!(port.created.borrow()[0], "Telegram helm_bot 2027-01-15");
        assert_eq!(
            active_chat(&path, &port, &p, Some("helm_bot"), T0 + 1000).unwrap(),
            ("chat-1".into(), false)
        );
    }

    #[test]
    fn the_tag_drops_on_age_tokens_archive_or_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let path = tag_path(dir.path());
        let port = Port::default();
        let p = TagPolicy {
            max_age_ms: 60_000,
            max_tokens: 100,
        };
        active_chat(&path, &port, &p, None, T0).unwrap();
        // Age.
        assert_eq!(
            active_chat(&path, &port, &p, None, T0 + 60_000).unwrap(),
            ("chat-2".into(), true)
        );
        // Tokens: counted from when the chat was tagged.
        port.chats.borrow_mut().get_mut("chat-2").unwrap().tokens = 99;
        assert!(!active_chat(&path, &port, &p, None, T0 + 60_001).unwrap().1);
        port.chats.borrow_mut().get_mut("chat-2").unwrap().tokens = 100;
        assert_eq!(
            active_chat(&path, &port, &p, None, T0 + 60_002).unwrap().0,
            "chat-3"
        );
        // Archived.
        port.chats.borrow_mut().get_mut("chat-3").unwrap().archived = true;
        assert_eq!(
            active_chat(&path, &port, &p, None, T0 + 60_003).unwrap().0,
            "chat-4"
        );
        // Deleted.
        port.chats.borrow_mut().remove("chat-4");
        assert_eq!(
            active_chat(&path, &port, &p, None, T0 + 60_004).unwrap().0,
            "chat-5"
        );
        assert_eq!(port.created.borrow()[4], "Telegram bot 2027-01-15");
    }

    #[test]
    fn a_token_baseline_is_taken_at_tagging() {
        let tag = ActiveTag {
            chat_id: "c".into(),
            started_at: T0,
            tokens_at_start: 1000,
        };
        let p = TagPolicy {
            max_age_ms: i64::MAX,
            max_tokens: 500,
        };
        assert_eq!(
            expired(
                &tag,
                Some(ChatFacts {
                    archived: false,
                    tokens: 1499
                }),
                &p,
                T0
            ),
            None
        );
        assert!(expired(
            &tag,
            Some(ChatFacts {
                archived: false,
                tokens: 1500
            }),
            &p,
            T0
        )
        .is_some());
    }

    #[test]
    fn civil_dates_are_right() {
        assert_eq!(civil_date(0), (1970, 1, 1));
        assert_eq!(civil_date(19_782), (2024, 2, 29));
        assert_eq!(civil_date(20_744), (2026, 10, 18));
    }

    #[test]
    fn queued_messages_carry_their_age_and_ids_are_stable() {
        assert_eq!(inbound_text("hi", None), "hi");
        assert_eq!(inbound_text("hi", Some(0)), "hi");
        assert_eq!(
            inbound_text("hi", Some(12)),
            "(sent 12 min ago, queued while Helm was offline)\nhi"
        );
        assert_eq!(submission_id("ab-12"), submission_id("ab-12"));
        assert_ne!(submission_id("ab-12"), submission_id("ab-13"));
        assert!(uuid::Uuid::parse_str(&submission_id("a/b\\c")).is_ok());
        let d = delivered_text("hi", Some(3));
        assert!(d.starts_with(BRIEF_NOTE), "{d}");
        assert!(d.ends_with("queued while Helm was offline)\nhi"), "{d}");
    }

    #[test]
    fn the_renderer_is_brief() {
        assert_eq!(render_status(&[], &[]), "Working…");
        assert_eq!(
            render_status(&["Bash".into()], &["Read".into(), "Edit".into()]),
            "Working…\nRunning: Bash\nRecent: Read, Edit"
        );
        assert_eq!(render_final(Some("  All done.  ")), "All done.");
        assert_eq!(render_final(None), "Done (no text answer).");
        let long = "x".repeat(MAX_FINAL_CHARS + 1);
        let f = render_final(Some(&long));
        assert!(f.contains("reply \"more\""), "{f}");
        assert_eq!(f.chars().filter(|c| *c == 'x').count(), MAX_FINAL_CHARS);
        assert!(!render_final(Some(&"x".repeat(MAX_FINAL_CHARS))).contains("more"));
        assert!(render_blocked(Some(" Allow Bash? "), None).starts_with(
            "Waiting for you in Helm: Allow Bash?\n\nThis can only be answered in the app"
        ));
        assert!(render_blocked(None, Some("")).starts_with("Waiting for you in Helm\n"));
        assert_eq!(render_answered(" Blue "), "Answered: Blue");
        assert_eq!(
            render_answered(&"y".repeat(201)),
            format!("Answered: {}…", "y".repeat(200))
        );
    }

    #[test]
    fn limits_and_answered_ids_are_kept_in_the_profile() {
        let dir = tempfile::tempdir().unwrap();
        let p = policy_path(dir.path());
        assert_eq!(load_policy(&p), TagPolicy::default());
        let set = PolicyFile {
            max_age_hours: 6,
            max_tokens: 50_000,
        };
        save_policy(&p, set.check().unwrap()).unwrap();
        assert_eq!(PolicyFile::from(load_policy(&p)), set);
        for (hours, tokens) in [(0, 50_000), (721, 50_000), (6, 9_999), (6, 5_000_001)] {
            let bad = PolicyFile {
                max_age_hours: hours,
                max_tokens: tokens,
            };
            assert!(bad.check().is_err(), "{bad:?}");
        }
        let a = answered_path(dir.path());
        let mut list = load_answered(&a);
        for i in 0..=ANSWERED_KEPT {
            remember_answered(&a, &mut list, &format!("m{i}")).unwrap();
        }
        let back = load_answered(&a);
        assert_eq!(back.len(), ANSWERED_KEPT);
        assert!(!back.contains(&"m0".to_owned()) && back.contains(&format!("m{ANSWERED_KEPT}")));
    }

    #[test]
    fn questions_arrive_numbered_and_a_number_picks_an_option() {
        let opts = vec!["Red".to_owned(), "Blue".to_owned()];
        let q = render_question(Some("Colour?"), Some("Pick one"), &opts);
        assert!(q.starts_with("Colour?\nPick one\n1. Red\n2. Blue"), "{q}");
        assert_eq!(question_answer(&opts, " 2 "), "Blue");
        assert_eq!(question_answer(&opts, "3"), "3");
        assert_eq!(question_answer(&opts, "0"), "0");
        assert_eq!(question_answer(&opts, "green"), "green");
    }

    /// Trust-boundary item e: a Telegram message never counts as the user's consent, even when it
    /// says exactly the words the consent check wants. Runs Helm's real words verifier.
    #[test]
    fn a_telegram_message_never_counts_as_consent() {
        use helm_core::db::{Database, MigrationRegistry};
        use rusqlite::params;
        use serde_json::json;

        fn meta(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS submission_meta (
                     submission_id TEXT PRIMARY KEY REFERENCES submissions(id) ON DELETE CASCADE,
                     origin TEXT NOT NULL DEFAULT 'user', priority TEXT, run_id TEXT);",
            )
        }
        let run = |origin: &str| -> Result<(), String> {
            let dir = tempfile::tempdir().unwrap();
            let mut reg = MigrationRegistry::new();
            reg.initializer("meta", meta);
            helm_consent::register_migrations(&mut reg);
            let db = Database::open(&dir.path().join("t.db"), &reg).unwrap();
            let mut kinds = helm_consent::AllowKinds::new();
            kinds.register(helm_consent::kinds::test_kind());
            let target = json!({ "name": "alpha" });
            db.write::<_, rusqlite::Error>(|tx| {
                tx.execute("INSERT INTO projects(id, name, created_at) VALUES ('p', 'P', 1)", [])?;
                tx.execute("INSERT INTO chats(id, project_id, created_at, updated_at) VALUES ('c', 'p', 1, 1)", [])?;
                Ok(())
            })
            .unwrap();
            db.write::<_, helm_consent::Refusal>(|tx| {
                helm_consent::store::record_ask(tx, &kinds, "c", "test_action", &target, T0)
            })
            .unwrap();
            db.write::<_, rusqlite::Error>(|tx| {
                tx.execute(
                    "INSERT INTO submissions(id, chat_id, text, state, created_at)
                     VALUES ('s1', 'c', ?1, 'complete', ?2)",
                    params![
                        inbound_text("Yes. Run the test action alpha.", None),
                        T0 + 1
                    ],
                )?;
                tx.execute(
                    "INSERT INTO submission_meta(submission_id, origin) VALUES ('s1', ?1)",
                    params![origin],
                )?;
                Ok(())
            })
            .unwrap();
            db.write::<_, helm_consent::Refusal>(|tx| {
                helm_consent::store::spend_words(
                    tx,
                    &kinds,
                    "c",
                    "test_action",
                    &target,
                    "t",
                    T0 + 2,
                )
            })
            .map(|_| ())
            .map_err(|r| r.0)
        };
        assert!(
            run("typed").is_ok(),
            "control: the same words typed do count"
        );
        let e = run(TELEGRAM_ORIGIN.as_str()).unwrap_err();
        assert!(e.contains("not answered"), "{e}");
        assert_eq!(TELEGRAM_ORIGIN.as_str(), "telegram");
    }
}
