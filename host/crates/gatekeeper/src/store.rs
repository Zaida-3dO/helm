//! The gatekeeper's own tables: `gate_asks` and `gate_settings`.
//!
//! `gate_asks` is the audit trail of every card the gate looked at. Chat ids carry no foreign
//! key, so a record outlives the chat it came from, and it keeps a title snapshot. Every state
//! change is a compare-and-set: the row moves only from the state the caller expects, so two
//! paths that race (the user answering while the gate decides) leave one winner.
//!
//! Rows live 180 days and are pruned by the crate's upkeep step; a decided row nobody reviewed
//! becomes `kept` after seven days and leaves the review list.

use crate::settings::{read_lenient, GateSettings};
use helm_chats::types::GateBrief;
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

pub const KEEP_AFTER_MS: i64 = 7 * 24 * 3_600_000;
pub const RETENTION_MS: i64 = 180 * 24 * 3_600_000;

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS gate_settings (
             profile_id TEXT NOT NULL PRIMARY KEY,
             value      TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS gate_asks (
             id            TEXT PRIMARY KEY,
             profile_id    TEXT NOT NULL DEFAULT '',
             chat_id       TEXT NOT NULL,
             chat_title    TEXT NOT NULL DEFAULT '',
             channel       TEXT NOT NULL
                 CHECK (channel IN ('question', 'plan', 'flag', 'telegram_ask', 'board_pick')),
             request_ref   TEXT NOT NULL,
             asked_at      INTEGER NOT NULL,
             question      TEXT NOT NULL DEFAULT '',
             question_json TEXT NOT NULL DEFAULT '',
             verdict       TEXT NOT NULL
                 CHECK (verdict IN ('checking', 'decided', 'escalated', 'failed', 'passed')),
             pass_reason   TEXT,
             decided_by    TEXT CHECK (decided_by IN ('local', 'claude', 'rule')),
             model         TEXT,
             confidence    REAL,
             risk          TEXT CHECK (risk IN ('low', 'medium', 'high')),
             reason        TEXT,
             choice        TEXT,
             brief_json    TEXT,
             took_ms       INTEGER,
             state         TEXT NOT NULL
                 CHECK (state IN ('open', 'answered', 'parked', 'withdrawn', 'kept',
                                  'vetoed', 'expired')),
             answer        TEXT,
             answered_via  TEXT CHECK (answered_via IN ('helm', 'telegram', 'board', 'chat')),
             answered_at   INTEGER,
             review        TEXT NOT NULL DEFAULT 'pending'
                 CHECK (review IN ('pending', 'fine', 'should_have_asked', 'didnt_need_me')),
             review_note   TEXT
         );
         CREATE UNIQUE INDEX IF NOT EXISTS gate_asks_ref ON gate_asks(chat_id, request_ref);
         CREATE INDEX IF NOT EXISTS gate_asks_asked ON gate_asks(asked_at DESC);
         CREATE INDEX IF NOT EXISTS gate_asks_chat ON gate_asks(chat_id);
         CREATE INDEX IF NOT EXISTS gate_asks_review
             ON gate_asks(verdict, state, review, asked_at DESC);",
    )?;
    Ok(())
}

/// Whose chat this is, falling back to the machine's current person, then to none. The profiles
/// tables belong to another crate; without them the answer is the empty key.
pub fn chat_profile(tx: &Transaction<'_>, chat_id: &str) -> String {
    let of_chat: Option<String> = tx
        .query_row(
            "SELECT profile_id FROM chats WHERE id = ?1",
            [chat_id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
        .unwrap_or_default();
    if let Some(p) = of_chat.filter(|p| !p.is_empty()) {
        return p;
    }
    helm_core::prefs::read(tx, "activeProfile")
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

pub fn load_settings(tx: &Transaction<'_>, profile_id: &str) -> rusqlite::Result<GateSettings> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM gate_settings WHERE profile_id = ?1",
            [profile_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .map(|v| read_lenient(&v))
        .unwrap_or_default())
}

pub fn save_settings(
    tx: &Transaction<'_>,
    profile_id: &str,
    s: &GateSettings,
) -> rusqlite::Result<()> {
    let v = serde_json::to_string(s).expect("settings serialise");
    tx.execute(
        "INSERT INTO gate_settings (profile_id, value) VALUES (?1, ?2)
         ON CONFLICT(profile_id) DO UPDATE SET value = excluded.value",
        params![profile_id, v],
    )?;
    Ok(())
}

/// One audit row as the UI and the review list see it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GateAsk {
    pub id: String,
    pub profile_id: String,
    pub chat_id: String,
    pub chat_title: String,
    pub channel: String,
    pub request_ref: String,
    #[ts(type = "number")]
    pub asked_at: i64,
    /// The question as asked, in one line.
    pub question: String,
    pub verdict: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pass_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub decided_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub risk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub choice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub brief: Option<GateBrief>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub took_ms: Option<i64>,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub answered_via: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub answered_at: Option<i64>,
    pub review: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub review_note: Option<String>,
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<GateAsk> {
    let brief: Option<String> = r.get(16)?;
    Ok(GateAsk {
        id: r.get(0)?,
        profile_id: r.get(1)?,
        chat_id: r.get(2)?,
        chat_title: r.get(3)?,
        channel: r.get(4)?,
        request_ref: r.get(5)?,
        asked_at: r.get(6)?,
        question: r.get(7)?,
        verdict: r.get(8)?,
        pass_reason: r.get(9)?,
        decided_by: r.get(10)?,
        model: r.get(11)?,
        confidence: r.get(12)?,
        risk: r.get(13)?,
        reason: r.get(14)?,
        choice: r.get(15)?,
        brief: brief.and_then(|b| serde_json::from_str(&b).ok()),
        took_ms: r.get(17)?,
        state: r.get(18)?,
        answer: r.get(19)?,
        answered_via: r.get(20)?,
        answered_at: r.get(21)?,
        review: r.get(22)?,
        review_note: r.get(23)?,
    })
}

const COLS: &str = "id, profile_id, chat_id, chat_title, channel, request_ref, asked_at, \
                    question, verdict, pass_reason, decided_by, model, confidence, risk, \
                    reason, choice, brief_json, took_ms, state, answer, \
                    answered_via, answered_at, review, review_note";

/// Open a card the gate is holding. One row per (chat, request): a re-request of the same id
/// answers `false` and writes nothing, so a worker replay cannot stack rows.
#[allow(clippy::too_many_arguments)]
pub fn open_ask(
    tx: &Transaction<'_>,
    id: &str,
    profile_id: &str,
    chat_id: &str,
    chat_title: &str,
    request_ref: &str,
    question: &str,
    card_json: &str,
    now: i64,
) -> rusqlite::Result<bool> {
    Ok(tx.execute(
        "INSERT INTO gate_asks
             (id, profile_id, chat_id, chat_title, channel, request_ref, asked_at,
              question, question_json, verdict, state)
         VALUES (?1, ?2, ?3, ?4, 'question', ?5, ?6, ?7, ?8, 'checking', 'open')
         ON CONFLICT(chat_id, request_ref) DO NOTHING",
        params![
            id,
            profile_id,
            chat_id,
            chat_title,
            request_ref,
            now,
            question,
            card_json
        ],
    )? > 0)
}

/// Move a checking row to its verdict, once. False when another path already settled it.
#[allow(clippy::too_many_arguments)]
pub fn finish_ask(
    tx: &Transaction<'_>,
    id: &str,
    verdict: &str,
    pass_reason: Option<&str>,
    decided_by: Option<&str>,
    model: Option<&str>,
    confidence: Option<f64>,
    risk: Option<&str>,
    reason: Option<&str>,
    choice: Option<&str>,
    brief: Option<&GateBrief>,
    took_ms: i64,
    state: &str,
    answered_via: Option<&str>,
    now: i64,
) -> rusqlite::Result<bool> {
    let brief_json = brief.map(|b| serde_json::to_string(b).expect("brief serialises"));
    let answered_at = matches!(state, "answered").then_some(now);
    Ok(tx.execute(
        "UPDATE gate_asks SET verdict = ?2, pass_reason = ?3, decided_by = ?4, model = ?5,
                 confidence = ?6, risk = ?7, reason = ?8, choice = ?9, brief_json = ?10,
                 took_ms = ?11, state = ?12, answered_via = ?13, answered_at = ?14
         WHERE id = ?1 AND verdict = 'checking'",
        params![
            id,
            verdict,
            pass_reason,
            decided_by,
            model,
            confidence,
            risk,
            reason,
            choice,
            brief_json,
            took_ms,
            state,
            answered_via,
            answered_at
        ],
    )? > 0)
}

pub fn ask(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<Option<GateAsk>> {
    tx.query_row(
        &format!("SELECT {COLS} FROM gate_asks WHERE id = ?1"),
        [id],
        row,
    )
    .optional()
}

/// Filters for the list: `all`, `decided`, `asked` (the ones that reached the user), `failed`,
/// and `review` — decided rows still waiting for their seven days or their verdict from the
/// user. Search matches the question, the chat title, the choice and the reason.
pub fn list(
    tx: &Transaction<'_>,
    filter: &str,
    query: &str,
    limit: i64,
    now: i64,
) -> rusqlite::Result<Vec<GateAsk>> {
    let where_ = match filter {
        "decided" => "verdict = 'decided'",
        "asked" => "verdict = 'escalated'",
        "failed" => "verdict = 'failed'",
        "review" => "verdict = 'decided' AND state = 'open' AND review = 'pending'",
        _ => "1",
    };
    let q = query.trim();
    let like = format!("%{q}%");
    let sql = format!(
        "SELECT {COLS} FROM gate_asks WHERE {where_} \
         AND (?1 = '' OR question LIKE ?1 OR chat_title LIKE ?1 \
              OR COALESCE(choice, '') LIKE ?1 OR COALESCE(reason, '') LIKE ?1) \
         AND asked_at >= ?3 \
         ORDER BY asked_at DESC LIMIT ?2"
    );
    let mut st = tx.prepare(&sql)?;
    let rows = st.query_map(params![like, limit, now - RETENTION_MS], row)?;
    rows.collect()
}

/// The user's marked examples for one person: at most six "should have asked me" (things the
/// gate decided that it should not have) and six "didn't need me" (things it asked about that
/// it need not have), newest first, each cut to 400 characters. They ride along the next job
/// as few-shot lines.
pub fn examples(tx: &Transaction<'_>, profile_id: &str) -> rusqlite::Result<Vec<Value>> {
    let mut st = tx.prepare(
        "SELECT review, COALESCE(question, ''), COALESCE(review_note, '')
         FROM gate_asks
         WHERE profile_id = ?1 AND review IN ('should_have_asked', 'didnt_need_me')
         ORDER BY asked_at DESC LIMIT 24",
    )?;
    let rows = st.query_map([profile_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    let cut = |s: &str| -> String { s.chars().take(400).collect() };
    let mut out = Vec::new();
    let mut of = [0usize; 2];
    for r in rows {
        let (kind, question, note) = r?;
        let idx = usize::from(kind == "didnt_need_me");
        if of[idx] >= 6 || question.trim().is_empty() {
            continue;
        }
        of[idx] += 1;
        out.push(serde_json::json!({
            "kind": kind,
            "question": cut(&question),
            "note": note.map(|n| cut(&n)).unwrap_or_default(),
        }));
    }
    Ok(out)
}

/// Mark one decided row reviewed. False when the row is gone or was already marked.
pub fn review(
    tx: &Transaction<'_>,
    id: &str,
    review: &str,
    note: Option<&str>,
) -> rusqlite::Result<Option<GateAsk>> {
    let changed = tx.execute(
        "UPDATE gate_asks SET review = ?2, review_note = ?3
         WHERE id = ?1 AND verdict IN ('decided', 'escalated') AND review = 'pending'",
        params![id, review, note],
    )? > 0;
    if !changed {
        return ask(tx, id);
    }
    ask(tx, id)
}

/// The user changed their mind about a decided row: it leaves the review list.
pub fn veto(tx: &Transaction<'_>, id: &str, now: i64) -> rusqlite::Result<bool> {
    Ok(tx.execute(
        "UPDATE gate_asks SET state = 'vetoed', answered_at = ?2
         WHERE id = ?1 AND verdict = 'decided' AND state IN ('open', 'kept')",
        params![id, now],
    )? > 0)
}

/// Decided rows nobody reviewed in seven days leave the review list (they stay in history).
pub fn mark_kept(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<usize> {
    tx.execute(
        "UPDATE gate_asks SET state = 'kept'
         WHERE verdict = 'decided' AND state = 'open' AND review = 'pending' AND asked_at < ?1",
        [now - KEEP_AFTER_MS],
    )
}

/// Rows older than the retention are pruned.
pub fn prune(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<usize> {
    tx.execute(
        "DELETE FROM gate_asks WHERE asked_at < ?1",
        [now - RETENTION_MS],
    )
}

/// A `checking` row the process never finished (a crash between holding and deciding) is
/// settled as failed, so nothing stays "checking" forever.
pub fn settle_orphans(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<usize> {
    tx.execute(
        "UPDATE gate_asks SET verdict = 'failed', reason = 'The check did not finish.'
         WHERE verdict = 'checking' AND asked_at < ?1",
        [now - 5 * 60_000],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::AskFrequency;
    use helm_core::db::{Database, MigrationRegistry};
    use std::sync::Arc;

    const T0: i64 = 1_800_000_000_000;
    const DAY: i64 = 24 * 3_600_000;

    fn brief() -> GateBrief {
        GateBrief {
            problem: "Rebuild or wait?".into(),
            options: vec![helm_chats::types::GateBriefOption {
                label: "Rebuild".into(),
                means: "start again".into(),
                pros: "clean".into(),
                cons: "slow".into(),
                cost: "20 min".into(),
            }],
            recommendation: "Rebuild".into(),
            why: "it is already broken".into(),
        }
    }

    fn env() -> (tempfile::TempDir, Arc<Database>) {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        reg.initializer("gate", init);
        let db = Arc::new(Database::open(&dir.path().join("t.db"), &reg).unwrap());
        (dir, db)
    }

    #[test]
    fn one_table_walks_the_whole_lifecycle() {
        let (_dir, db) = env();
        let chat = db
            .write(|tx| helm_chats::store::create_chat(tx, "C:/w", None, None, T0))
            .unwrap();

        // A held card opens one row; a replay of the same request stacks nothing.
        let opened = db
            .write(|tx| {
                open_ask(
                    tx,
                    "a1",
                    "",
                    &chat,
                    "Fix the report",
                    "req-1",
                    "Which file?",
                    "{}",
                    T0,
                )
            })
            .unwrap();
        assert!(opened);
        let again = db
            .write(|tx| {
                open_ask(
                    tx,
                    "a9",
                    "",
                    &chat,
                    "Fix the report",
                    "req-1",
                    "Which file?",
                    "{}",
                    T0 + 1,
                )
            })
            .unwrap();
        assert!(!again, "the same (chat, request) never gets a second row");
        assert_eq!(
            db.snapshot(|tx| list(tx, "all", "", 50, T0)).unwrap().len(),
            1
        );

        // Every state change is a compare-and-set: the winner's verdict stands.
        let first = db
            .write(|tx| {
                finish_ask(
                    tx,
                    "a1",
                    "decided",
                    None,
                    Some("claude"),
                    Some("sonnet"),
                    Some(0.8),
                    Some("low"),
                    Some("A default fits here"),
                    Some("Yes"),
                    None,
                    1_200,
                    "open",
                    None,
                    T0 + 2_000,
                )
            })
            .unwrap();
        let second = db
            .write(|tx| {
                finish_ask(
                    tx,
                    "a1",
                    "escalated",
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(&brief()),
                    900,
                    "open",
                    None,
                    T0 + 2_100,
                )
            })
            .unwrap();
        assert!(first && !second);
        let row = db.snapshot(|tx| ask(tx, "a1")).unwrap().unwrap();
        assert_eq!(row.verdict, "decided");
        assert_eq!(row.state, "open");
        assert_eq!(row.choice.as_deref(), Some("Yes"));
        assert_eq!(row.confidence, Some(0.8));

        // Review marks stick once; the first mark wins.
        let marked = db
            .write(|tx| review(tx, "a1", "should_have_asked", Some("too clever")))
            .unwrap()
            .unwrap();
        assert_eq!(marked.review, "should_have_asked");
        let retaken = db
            .write(|tx| review(tx, "a1", "fine", None))
            .unwrap()
            .unwrap();
        assert_eq!(retaken.review, "should_have_asked");

        // Marked rows ride the next job as examples, each note cut short.
        let ex = db.snapshot(|tx| examples(tx, "")).unwrap();
        assert_eq!(ex.len(), 1);
        assert_eq!(ex[0]["kind"], "should_have_asked");
        assert_eq!(ex[0]["note"], "too clever");

        // A second decided row, nobody reviews it: after seven days it is kept and leaves
        // the review list, and a later change of mind still works.
        db.write(|tx| {
            open_ask(tx, "a2", "", &chat, "t", "req-2", "Which name?", "{}", T0)?;
            finish_ask(
                tx,
                "a2",
                "decided",
                None,
                Some("claude"),
                Some("sonnet"),
                Some(0.9),
                Some("low"),
                Some("r"),
                Some("Left"),
                None,
                10,
                "open",
                None,
                T0 + 10,
            )?;
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();
        assert_eq!(
            db.snapshot(|tx| list(tx, "review", "", 50, T0 + DAY))
                .unwrap()
                .len(),
            1,
            "only the unreviewed decided row waits"
        );
        assert_eq!(db.write(|tx| mark_kept(tx, T0 + 8 * DAY)).unwrap(), 1);
        assert!(db
            .snapshot(|tx| list(tx, "review", "", 50, T0 + 8 * DAY))
            .unwrap()
            .is_empty());
        assert!(db.write(|tx| veto(tx, "a2", T0 + 8 * DAY + 1)).unwrap());
        assert_eq!(
            db.snapshot(|tx| ask(tx, "a2")).unwrap().unwrap().state,
            "vetoed"
        );

        // An escalated row carries the write-up; answering it lets "didn't need me" mark it.
        db.write(|tx| {
            open_ask(
                tx,
                "a3",
                "",
                &chat,
                "t",
                "req-3",
                "Deploy anyway?",
                "{}",
                T0,
            )?;
            finish_ask(
                tx,
                "a3",
                "escalated",
                None,
                None,
                Some("sonnet"),
                Some(0.4),
                Some("high"),
                Some("this is live"),
                None,
                Some(&brief()),
                900,
                "open",
                None,
                T0 + 900,
            )?;
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();
        let row = db.snapshot(|tx| ask(tx, "a3")).unwrap().unwrap();
        assert_eq!(row.brief.as_ref().unwrap().problem, "Rebuild or wait?");
        assert_eq!(row.brief.as_ref().unwrap().options.len(), 1);
        db.write::<_, rusqlite::Error>(|tx| {
            tx.execute(
                "UPDATE gate_asks SET state = 'answered', answered_via = 'chat', answered_at = ?1
                 WHERE id = 'a3'",
                [T0 + 5_000],
            )?;
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();
        let marked = db
            .write(|tx| review(tx, "a3", "didnt_need_me", None))
            .unwrap()
            .unwrap();
        assert_eq!(marked.review, "didnt_need_me");
        let ex = db.snapshot(|tx| examples(tx, "")).unwrap();
        assert_eq!(ex.len(), 2, "both kinds come back");

        // List filters and search.
        assert_eq!(
            db.snapshot(|tx| list(tx, "decided", "", 50, T0 + DAY))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            db.snapshot(|tx| list(tx, "asked", "", 50, T0 + DAY))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db.snapshot(|tx| list(tx, "all", "Which file", 50, T0 + DAY))
                .unwrap()
                .len(),
            1,
            "search finds the question"
        );

        // Retention prunes; a held card the process never finished settles as failed.
        db.write(|tx| {
            open_ask(
                tx,
                "a4",
                "",
                &chat,
                "old",
                "req-4",
                "q",
                "{}",
                T0 - 200 * DAY,
            )?;
            open_ask(
                tx,
                "a5",
                "",
                &chat,
                "stuck",
                "req-5",
                "q",
                "{}",
                T0 - 10 * 60_000,
            )?;
            Ok::<(), rusqlite::Error>(())
        })
        .unwrap();
        assert_eq!(db.write(|tx| prune(tx, T0)).unwrap(), 1);
        assert!(db.snapshot(|tx| ask(tx, "a4")).unwrap().is_none());
        assert_eq!(db.write(|tx| settle_orphans(tx, T0)).unwrap(), 1);
        assert_eq!(
            db.snapshot(|tx| ask(tx, "a5")).unwrap().unwrap().verdict,
            "failed"
        );

        // Settings save and load per person; another person gets the defaults.
        let s = GateSettings {
            enabled: false,
            ask: AskFrequency::More,
            policy: Some("mine".into()),
            model: "opus".into(),
        };
        db.write(|tx| save_settings(tx, "p1", &s)).unwrap();
        assert_eq!(db.snapshot(|tx| load_settings(tx, "p1")).unwrap(), s);
        assert_eq!(
            db.snapshot(|tx| load_settings(tx, "p2")).unwrap(),
            GateSettings::default()
        );

        // Whose chat: a chat with no person follows the machine's current person; without
        // the profiles tables at all the answer is the empty key.
        assert_eq!(
            db.snapshot(|tx| Ok::<String, rusqlite::Error>(chat_profile(tx, &chat)))
                .unwrap(),
            ""
        );
        db.write(|tx| {
            helm_core::prefs::set_raw(tx, "activeProfile", &Value::String("someone".into()))?;
            Ok::<(), helm_core::prefs::PrefError>(())
        })
        .unwrap();
        assert_eq!(
            db.snapshot(|tx| Ok::<String, rusqlite::Error>(chat_profile(tx, &chat)))
                .unwrap(),
            "someone"
        );
        assert_eq!(
            db.snapshot(|tx| Ok::<String, rusqlite::Error>(chat_profile(tx, "no-such-chat")))
                .unwrap(),
            "someone"
        );
    }
}
