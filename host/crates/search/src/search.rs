//! Running a search: title matches first, then ranked text hits, one per chat.

use crate::query::{fts_expression, words};
use crate::rank::{score, snippet};
use crate::signals::{boost, decayed, Facts};
use crate::types::*;
use helm_core::db::Database;
use helm_ipc::ApiError;
use rusqlite::{params, Transaction};
use std::collections::{HashMap, HashSet};

/// Text hits read from the index before they are reduced to one per chat (chats section 15 item 3).
pub const MAX_TEXT_HITS: i64 = 1_000;
/// Text results kept after ranking.
pub const KEEP_TEXT: usize = 60;
/// Title matches listed.
pub const KEEP_TITLES: usize = 30;
/// Title matches re-ranked by the boost before the cut to [`KEEP_TITLES`] (newest first on ties).
pub const TITLE_CANDIDATES: usize = 200;

struct TextHit {
    message_id: i64,
    chat_id: String,
    text: String,
    score: f64,
    title: String,
    project: String,
}

struct TitleHit {
    chat_id: String,
    title: String,
    project: String,
}

fn sql(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

pub fn run(db: &Database, args: &SearchArgs, now_ms: i64) -> Result<SearchResult, ApiError> {
    let query = args.query.trim();
    if query.len() > MAX_QUERY_BYTES {
        return Err(ApiError::bad_args("The search is too long."));
    }
    if query.is_empty() {
        return Ok(SearchResult { hits: Vec::new() });
    }
    let ws = words(query);
    let fts = fts_expression(&ws);
    let project = args.project_id.as_deref();
    let current = args.current_project_id.as_deref();
    let (titles, texts) = db
        .snapshot(|tx| {
            let titles = title_hits(tx, query, args.archived, project, current, now_ms)?;
            let texts = match &fts {
                Some(f) => text_hits(tx, f, args.archived, project, current, now_ms)?,
                None => Vec::new(),
            };
            Ok::<_, rusqlite::Error>((titles, texts))
        })
        .map_err(sql)?;
    Ok(SearchResult {
        hits: combine(titles, texts, &ws),
    })
}

/// The facts the boost reads, as columns after `c.id`: pinned, status, project, opens, opens time.
const FACT_COLUMNS: &str =
    "c.pinned, c.status, c.project_id, COALESCE(o.opens, 0.0), COALESCE(o.updated_at, 0)";
const FACT_JOIN: &str = "LEFT JOIN search_opens o ON o.chat_id = c.id";

fn facts(
    r: &rusqlite::Row<'_>,
    first: usize,
    current: Option<&str>,
    now_ms: i64,
) -> rusqlite::Result<Facts> {
    let pinned: i64 = r.get(first)?;
    let status: String = r.get(first + 1)?;
    let project: String = r.get(first + 2)?;
    let opens: f64 = r.get(first + 3)?;
    let at: i64 = r.get(first + 4)?;
    Ok(Facts {
        pinned: pinned != 0,
        running: matches!(status.as_str(), "starting" | "running" | "stopping"),
        current_project: current == Some(project.as_str()),
        decayed_opens: decayed(opens, at, now_ms),
    })
}

fn title_hits(
    tx: &Transaction<'_>,
    query: &str,
    archived: bool,
    project: Option<&str>,
    current: Option<&str>,
    now_ms: i64,
) -> rusqlite::Result<Vec<TitleHit>> {
    let needle = query.to_lowercase();
    let sql = format!(
        "SELECT c.id, c.title, p.name, {FACT_COLUMNS} FROM chats c
         JOIN projects p ON p.id = c.project_id {FACT_JOIN}
         WHERE c.side_source_id IS NULL AND (?1 OR c.archived = 0) AND (?2 IS NULL OR c.project_id = ?2)
         ORDER BY c.updated_at DESC, c.id"
    );
    let mut st = tx.prepare_cached(&sql)?;
    let mut rows = st.query(params![archived, project])?;
    let mut out: Vec<(f64, TitleHit)> = Vec::new();
    while let Some(r) = rows.next()? {
        let title: String = r.get(1)?;
        if title.to_lowercase().contains(&needle) {
            let b = boost(facts(r, 3, current, now_ms)?);
            out.push((
                b,
                TitleHit {
                    chat_id: r.get(0)?,
                    title,
                    project: r.get(2)?,
                },
            ));
            if out.len() == TITLE_CANDIDATES {
                break;
            }
        }
    }
    // Stable: equal boosts keep the newest-first order they were read in.
    out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out.into_iter().take(KEEP_TITLES).map(|(_, h)| h).collect())
}

/// The best message of each chat, best chat first.
fn text_hits(
    tx: &Transaction<'_>,
    fts: &str,
    archived: bool,
    project: Option<&str>,
    current: Option<&str>,
    now_ms: i64,
) -> rusqlite::Result<Vec<TextHit>> {
    let sql = format!(
        "SELECT s.message_id, s.chat_id, s.text, bm25(message_search), c.updated_at, c.title, p.name, {FACT_COLUMNS}
         FROM message_search f
         JOIN chat_search_source s ON s.message_id = f.rowid
         JOIN chats c ON c.id = s.chat_id
         JOIN projects p ON p.id = c.project_id {FACT_JOIN}
         WHERE message_search MATCH ?1 AND (?2 OR s.archived = 0) AND (?3 IS NULL OR c.project_id = ?3)
         ORDER BY bm25(message_search) LIMIT ?4"
    );
    let mut st = tx.prepare_cached(&sql)?;
    let rows = st.query_map(params![fts, archived, project, MAX_TEXT_HITS], |r| {
        let bm25: f64 = r.get(3)?;
        let updated: i64 = r.get(4)?;
        Ok(TextHit {
            message_id: r.get(0)?,
            chat_id: r.get(1)?,
            text: r.get(2)?,
            // Multiplied by (1 + B); B is never added.
            score: score(-bm25, updated, now_ms) * (1.0 + boost(facts(r, 7, current, now_ms)?)),
            title: r.get(5)?,
            project: r.get(6)?,
        })
    })?;
    let mut best: HashMap<String, TextHit> = HashMap::new();
    for h in rows {
        let h = h?;
        match best.get(&h.chat_id) {
            Some(cur) if cur.score >= h.score => {}
            _ => {
                best.insert(h.chat_id.clone(), h);
            }
        }
    }
    let mut out: Vec<TextHit> = best.into_values().collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.chat_id.cmp(&b.chat_id))
    });
    Ok(out)
}

fn combine(titles: Vec<TitleHit>, texts: Vec<TextHit>, words: &[String]) -> Vec<SearchHit> {
    let by_chat: HashMap<&str, &TextHit> = texts.iter().map(|t| (t.chat_id.as_str(), t)).collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut hits = Vec::new();
    for t in &titles {
        seen.insert(t.chat_id.clone());
        let text = by_chat.get(t.chat_id.as_str());
        hits.push(SearchHit {
            chat_id: t.chat_id.clone(),
            title: t.title.clone(),
            project: t.project.clone(),
            message_id: text.map(|h| h.message_id.to_string()),
            snippet: text.map(|h| snippet(&h.text, words)).unwrap_or_default(),
            title_only: text.is_none(),
        });
    }
    for h in texts
        .iter()
        .filter(|h| !seen.contains(&h.chat_id))
        .take(KEEP_TEXT)
    {
        hits.push(SearchHit {
            chat_id: h.chat_id.clone(),
            title: h.title.clone(),
            project: h.project.clone(),
            message_id: Some(h.message_id.to_string()),
            snippet: snippet(&h.text, words),
            title_only: false,
        });
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_core::db::MigrationRegistry;

    pub(crate) fn db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        crate::register_migrations(&mut reg);
        let db = Database::open(&dir.path().join("t.db"), &reg).unwrap();
        (dir, db)
    }

    /// A chat in folder `cwd` with the given title, last updated at `updated`, and messages.
    fn chat(
        db: &Database,
        cwd: &str,
        title: &str,
        updated: i64,
        msgs: &[(&str, &str, i64)],
    ) -> String {
        db.write(|tx| {
            let id = helm_chats::store::create_chat(tx, cwd, None, None, updated)?;
            tx.execute(
                "UPDATE chats SET title = ?1, updated_at = ?2 WHERE id = ?3",
                params![title, updated, id],
            )?;
            for (i, (role, text, active)) in msgs.iter().enumerate() {
                tx.execute(
                    "INSERT INTO messages(chat_id, source_key, role, text, timestamp, active)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, format!("k{i}"), role, text, updated, active],
                )?;
            }
            Ok::<_, rusqlite::Error>(id)
        })
        .unwrap()
    }

    fn args(q: &str) -> SearchArgs {
        SearchArgs {
            query: q.into(),
            project_id: None,
            archived: false,
            current_project_id: None,
        }
    }

    const NOW: i64 = 2_000_000_000_000;

    fn ids(r: &SearchResult) -> Vec<&str> {
        r.hits.iter().map(|h| h.title.as_str()).collect()
    }

    #[test]
    fn text_hits_match_prefixes_and_are_one_per_chat() {
        let (_d, db) = db();
        chat(
            &db,
            "C:/a",
            "Login work",
            NOW,
            &[
                ("user", "please fix the login form", 1),
                ("assistant", "login form fixed", 1),
            ],
        );
        chat(&db, "C:/a", "Other", NOW, &[("user", "nothing here", 1)]);
        let r = run(&db, &args("logi fo"), NOW).unwrap();
        assert_eq!(
            ids(&r),
            ["Login work"],
            "prefix words, ANDed, one hit for the chat"
        );
        assert!(!r.hits[0].title_only);
        assert!(r.hits[0].snippet.to_lowercase().contains("login"));
        assert!(r.hits[0].message_id.is_some());
    }

    #[test]
    fn all_words_must_match() {
        let (_d, db) = db();
        chat(&db, "C:/a", "A", NOW, &[("user", "alpha beta", 1)]);
        chat(&db, "C:/a", "B", NOW, &[("user", "alpha gamma", 1)]);
        assert_eq!(ids(&run(&db, &args("alpha beta"), NOW).unwrap()), ["A"]);
    }

    #[test]
    fn title_matches_come_first_and_are_labelled_when_the_chat_has_no_matching_text() {
        let (_d, db) = db();
        chat(
            &db,
            "C:/a",
            "Unrelated",
            NOW,
            &[("user", "the zebra crossing", 1)],
        );
        chat(
            &db,
            "C:/a",
            "Zebra plans",
            NOW - 1000,
            &[("user", "nothing", 1)],
        );
        let r = run(&db, &args("zebra"), NOW).unwrap();
        assert_eq!(ids(&r), ["Zebra plans", "Unrelated"]);
        assert!(
            r.hits[0].title_only && r.hits[0].message_id.is_none() && r.hits[0].snippet.is_empty()
        );
        assert!(!r.hits[1].title_only);
    }

    #[test]
    fn a_chat_matching_by_title_and_text_is_listed_once_with_its_snippet() {
        let (_d, db) = db();
        chat(
            &db,
            "C:/a",
            "Zebra",
            NOW,
            &[("user", "all about zebra stripes", 1)],
        );
        let r = run(&db, &args("zebra"), NOW).unwrap();
        assert_eq!(r.hits.len(), 1);
        assert!(!r.hits[0].title_only);
        assert!(r.hits[0].snippet.contains("zebra"));
    }

    #[test]
    fn title_match_is_a_case_insensitive_substring_of_the_whole_query() {
        let (_d, db) = db();
        chat(&db, "C:/a", "Fix Login Flow", NOW, &[]);
        assert_eq!(
            ids(&run(&db, &args("login fl"), NOW).unwrap()),
            ["Fix Login Flow"]
        );
        assert!(
            run(&db, &args("flow login"), NOW).unwrap().hits.is_empty(),
            "word order matters for titles"
        );
    }

    #[test]
    fn at_most_30_title_matches_newest_first() {
        let (_d, db) = db();
        for i in 0..35 {
            chat(&db, "C:/a", &format!("note {i}"), NOW - i * 1000, &[]);
        }
        let r = run(&db, &args("note"), NOW).unwrap();
        assert_eq!(r.hits.len(), 30);
        assert_eq!(r.hits[0].title, "note 0");
        assert_eq!(r.hits[29].title, "note 29");
    }

    #[test]
    fn text_results_are_trimmed_to_60() {
        let (_d, db) = db();
        for i in 0..70 {
            chat(
                &db,
                "C:/a",
                &format!("chat {i}"),
                NOW - i,
                &[("user", "common phrase", 1)],
            );
        }
        assert_eq!(run(&db, &args("common"), NOW).unwrap().hits.len(), 60);
    }

    #[test]
    fn a_newer_chat_outranks_an_older_one_with_the_same_text() {
        let (_d, db) = db();
        let week = 7 * 24 * 60 * 60 * 1000;
        chat(
            &db,
            "C:/a",
            "old",
            NOW - 52 * week,
            &[("user", "the quarterly report", 1)],
        );
        chat(
            &db,
            "C:/a",
            "new",
            NOW,
            &[("user", "the quarterly report", 1)],
        );
        assert_eq!(
            ids(&run(&db, &args("quarterly"), NOW).unwrap()),
            ["new", "old"]
        );
    }

    #[test]
    fn inactive_branches_side_chats_and_archived_chats_stay_out_unless_asked() {
        let (_d, db) = db();
        chat(&db, "C:/a", "branch", NOW, &[("user", "kiwi abandoned", 0)]);
        let side = chat(&db, "C:/a", "side", NOW, &[("assistant", "kiwi side", 1)]);
        let arch = chat(
            &db,
            "C:/a",
            "archived",
            NOW,
            &[("assistant", "kiwi archived", 1)],
        );
        let main = chat(&db, "C:/a", "main", NOW, &[("assistant", "kiwi main", 1)]);
        db.write(|tx| {
            tx.execute(
                "UPDATE chats SET side_source_id = ?1 WHERE id = ?2",
                [&main, &side],
            )?;
            tx.execute("UPDATE chats SET archived = 1 WHERE id = ?1", [&arch])
        })
        .unwrap();
        assert_eq!(ids(&run(&db, &args("kiwi"), NOW).unwrap()), ["main"]);
        let mut a = args("kiwi");
        a.archived = true;
        let mut got = ids(&run(&db, &a, NOW).unwrap())
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        got.sort();
        assert_eq!(got, ["archived", "main"]);
        // Title search respects the same scopes.
        assert!(run(&db, &args("archived"), NOW).unwrap().hits.is_empty());
        assert!(run(&db, &args("side"), NOW).unwrap().hits.is_empty());
    }

    #[test]
    fn the_project_filter_applies_to_text_and_titles() {
        let (_d, db) = db();
        chat(&db, "C:/one", "Mango one", NOW, &[("user", "mango", 1)]);
        chat(&db, "C:/two", "Mango two", NOW, &[("user", "mango", 1)]);
        let project: String = db
            .snapshot(|tx| {
                tx.query_row("SELECT id FROM projects WHERE path = 'C:/one'", [], |r| {
                    r.get(0)
                })
            })
            .unwrap();
        let mut a = args("mango");
        a.project_id = Some(project);
        assert_eq!(ids(&run(&db, &a, NOW).unwrap()), ["Mango one"]);
    }

    #[test]
    fn tool_output_is_not_searched() {
        let (_d, db) = db();
        chat(
            &db,
            "C:/a",
            "t",
            NOW,
            &[("toolResult", "secretword in output", 1)],
        );
        assert!(run(&db, &args("secretword"), NOW).unwrap().hits.is_empty());
    }

    #[test]
    fn blank_hostile_and_oversized_queries() {
        let (_d, db) = db();
        chat(&db, "C:/a", "plain", NOW, &[("user", "hello world", 1)]);
        assert!(run(&db, &args("   "), NOW).unwrap().hits.is_empty());
        for q in [
            "\"", "\"\"\"", "NEAR(", "a OR", "*", "- &", "(((", "x:y", "a\"b",
        ] {
            run(&db, &args(q), NOW).unwrap_or_else(|e| panic!("{q:?} failed: {e}"));
        }
        let big = "x".repeat(513);
        assert_eq!(run(&db, &args(&big), NOW).unwrap_err().code, "bad_args");
    }

    #[test]
    fn a_pinned_chat_outranks_an_equal_one_and_the_boost_multiplies() {
        let (_d, db) = db();
        let plain = chat(
            &db,
            "C:/a",
            "plain",
            NOW,
            &[("user", "the quarterly report", 1)],
        );
        let pinned = chat(
            &db,
            "C:/a",
            "pinned",
            NOW,
            &[("user", "the quarterly report", 1)],
        );
        let _ = plain;
        db.write(|tx| tx.execute("UPDATE chats SET pinned = 1 WHERE id = ?1", [&pinned]))
            .unwrap();
        assert_eq!(
            ids(&run(&db, &args("quarterly"), NOW).unwrap()),
            ["pinned", "plain"]
        );
    }

    #[test]
    fn running_current_project_and_opens_each_lift_a_chat() {
        // Same text and age; chat "b" gets one boost factor per scenario and must come first.
        for scenario in ["running", "project", "opens"] {
            let (_dir, db) = db();
            let a = chat(&db, "C:/one", "a", NOW, &[("user", "shared phrase", 1)]);
            let b = chat(&db, "C:/two", "b", NOW, &[("user", "shared phrase", 1)]);
            let mut q = args("shared");
            match scenario {
                "running" => {
                    db.write(|tx| {
                        tx.execute("UPDATE chats SET status = 'running' WHERE id = ?1", [&b])
                    })
                    .unwrap();
                }
                "project" => {
                    let p: String = db
                        .snapshot(|tx| {
                            tx.query_row("SELECT project_id FROM chats WHERE id = ?1", [&b], |r| {
                                r.get(0)
                            })
                        })
                        .unwrap();
                    q.current_project_id = Some(p);
                }
                _ => crate::signals::record_open(&db, &b, NOW).unwrap(),
            }
            let _ = a;
            assert_eq!(ids(&run(&db, &q, NOW).unwrap()), ["b", "a"], "{scenario}");
        }
    }

    #[test]
    fn title_matches_are_reranked_by_the_boost_with_newest_first_on_ties() {
        let (_d, db) = db();
        let old = chat(&db, "C:/a", "note old", NOW - 5000, &[]);
        chat(&db, "C:/a", "note new", NOW, &[]);
        chat(&db, "C:/a", "note newer", NOW + 1, &[]);
        db.write(|tx| tx.execute("UPDATE chats SET pinned = 1 WHERE id = ?1", [&old]))
            .unwrap();
        assert_eq!(
            ids(&run(&db, &args("note"), NOW + 1).unwrap()),
            ["note old", "note newer", "note new"]
        );
    }

    #[test]
    fn only_the_first_twelve_words_count() {
        let (_d, db) = db();
        chat(
            &db,
            "C:/a",
            "t",
            NOW,
            &[("user", "w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12", 1)],
        );
        // Word 13 does not appear in the text, but is ignored.
        let q = "w1 w2 w3 w4 w5 w6 w7 w8 w9 w10 w11 w12 nothere";
        assert_eq!(run(&db, &args(q), NOW).unwrap().hits.len(), 1);
    }
}
