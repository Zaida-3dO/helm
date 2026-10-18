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
//!
//! Delivering into a chat goes through [`ChatPort`]: the real one (the send path into chats)
//! lands with the chats seam; until then this module is the logic and its tests.

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

impl Default for TagPolicy {
    fn default() -> Self {
        TagPolicy {
            max_age_ms: 24 * 60 * 60 * 1000,
            max_tokens: 400_000,
        }
    }
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
        Some(t) => {
            if let Some(dir) = path.parent() {
                helm_core::privfs::create_private_dir_all(dir)?;
            }
            let bytes = serde_json::to_vec(t).map_err(std::io::Error::other)?;
            helm_core::privfs::write_atomic(path, &bytes)
        }
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
/// the acknowledgement) maps to the same submission.
pub fn submission_id(hub_id: &str) -> String {
    let clean: String = hub_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(64)
        .collect();
    format!("tg-{clean}")
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
        assert_eq!(submission_id("a/b\\c"), "tg-abc");
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
