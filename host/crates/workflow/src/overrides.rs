//! Soft refusals and overrides.
//!
//! - **Reason tier**: the command's first line is `# helm-override: <reason>`.
//! - **Quote tier**: the same line ends with `[user: "<their words>"]`, and the words match one of
//!   the user's own typed messages in this chat: the whole message, or a fragment of at least 2
//!   words and 8 letters, after normalising (straight quotes, collapsed whitespace, lower case),
//!   with negations and questions rejected.
//! - Each override writes exactly one `guard_override` row.
//!
//! The marker token is Helm's own (`helm-override`).

use crate::guard::{Tier, Verdict};
use crate::store::{self, Event, Run};
use rusqlite::Transaction;
use serde_json::json;

pub const MARKER: &str = "# helm-override:";
pub const MAX_COMMAND: usize = 2000;
pub const MAX_REASON: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    pub reason: String,
    pub quote: Option<String>,
}

/// The override marker on the command's first line, if any.
pub fn marker(cmd: &str) -> Option<Marker> {
    let first = cmd.lines().next()?.trim();
    let rest = first.strip_prefix(MARKER)?.trim();
    let (reason, quote) = match rest.find("[user:") {
        Some(i) if rest.ends_with(']') => {
            let q = rest[i + 6..rest.len() - 1].trim();
            let q = q
                .strip_prefix('"')
                .and_then(|q| q.strip_suffix('"'))
                .unwrap_or(q);
            (rest[..i].trim(), Some(q.to_owned()))
        }
        _ => (rest, None),
    };
    let reason: String = reason.chars().take(MAX_REASON).collect();
    (!reason.is_empty()).then_some(Marker { reason, quote })
}

pub fn normalise(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' => '\'',
            '\u{201c}' | '\u{201d}' => '"',
            c => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

const NEGATIONS: [&str; 12] = [
    "no", "not", "don't", "dont", "never", "stop", "wait", "abort", "halt", "cancel", "hold",
    "nope",
];

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| c.is_whitespace() || ",.;:!".contains(c))
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Where `q` occurs in `m` as whole words, if it does.
fn find_words(m: &str, q: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = m[from..].find(q).map(|i| i + from) {
        let before = m[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        let after = m[i + q.len()..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric());
        if before && after {
            return Some(i);
        }
        from = i + q.len().max(1);
    }
    None
}

fn negated(ws: &[String], skip_leading_no: bool) -> bool {
    ws.iter().enumerate().any(|(i, w)| {
        // Only a leading "No," followed by more words is an interjection; a bare "No" is a no.
        let interjection = i == 0 && skip_leading_no && w == "no" && ws.len() > 1;
        !interjection && (NEGATIONS.contains(&w.as_str()) || w.ends_with("n't"))
    })
}

/// Whether `quote` is backed by one of the user's own messages.
pub fn quote_ok(quote: &str, user_messages: &[String]) -> bool {
    let q = normalise(quote).trim_matches('"').trim().to_owned();
    let letters = q.chars().filter(|c| c.is_alphabetic()).count();
    let qw = words(&q);
    if q.is_empty() || q.contains('?') {
        return false;
    }
    user_messages.iter().any(|m| {
        let m = normalise(m);
        if m == q {
            // A whole message counts unless it negates (a leading "No," is an interjection).
            return !negated(&words(&m), m.starts_with("no,"));
        }
        if qw.len() < 2 || letters < 8 {
            return false;
        }
        let Some(at) = find_words(&m, &q) else {
            return false;
        };
        // A fragment carries no negation, and neither does the clause it sits in (before or
        // after it); a question never counts.
        if negated(&qw, false) {
            return false;
        }
        let clause_end = m[at..]
            .find(['.', '!', ';', '\n'])
            .map(|i| at + i)
            .unwrap_or(m.len());
        let clause_start = m[..at]
            .rfind(['.', '!', ';', '\n'])
            .map(|i| i + 1)
            .unwrap_or(0);
        let clause = &m[clause_start..clause_end];
        !clause.contains('?')
            && !m[at..].trim_end().ends_with('?')
            && !negated(&words(clause), false)
    })
}

/// What happens to a refused command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Let through by an override; record it.
    Overridden {
        tier: Tier,
        verified: bool,
        reason: String,
        quote: Option<String>,
    },
    Refuse(String),
}

/// Apply the override marker (if any) to a verdict.
pub fn decide(cmd: &str, v: &Verdict, user_messages: &[String]) -> Decision {
    let Verdict::Deny { tier, why, .. } = v else {
        return Decision::Allow;
    };
    let m = marker(cmd);
    let recipe = match tier {
        Tier::Reason => format!("To go ahead anyway, make the first line `{MARKER} <reason>`."),
        Tier::Quote => format!("This needs the user's permission: ask them, then make the first line `{MARKER} <reason> [user: \"<their exact words>\"]`."),
    };
    match (tier, m) {
        (Tier::Reason, Some(m)) => {
            let verified = m.quote.as_deref().is_some_and(|q| quote_ok(q, user_messages));
            Decision::Overridden { tier: Tier::Reason, verified, reason: m.reason, quote: m.quote }
        }
        (Tier::Quote, Some(m)) if m.quote.as_deref().is_some_and(|q| quote_ok(q, user_messages)) => {
            Decision::Overridden { tier: Tier::Quote, verified: true, reason: m.reason, quote: m.quote }
        }
        (Tier::Quote, Some(Marker { quote: Some(_), .. })) => Decision::Refuse(format!(
            "Helm refused: {why}. The quoted words do not match anything the user typed in this chat. {recipe}"
        )),
        _ => Decision::Refuse(format!("Helm refused: {why}. {recipe}")),
    }
}

/// Write the one `guard_override` row an override leaves (gate `bash`, `mcp` or `start`).
#[allow(clippy::too_many_arguments)]
pub fn record(
    tx: &Transaction<'_>,
    chat_id: &str,
    run: Option<&Run>,
    gate: &str,
    path: &str,
    d: &Decision,
    command: &str,
    waived: &str,
    now: i64,
) -> rusqlite::Result<Result<i64, String>> {
    if !matches!(gate, "bash" | "mcp" | "start") {
        return Ok(Err(format!("{gate} is not an overridable gate.")));
    }
    if !matches!(path, "resend" | "comment" | "tool") {
        return Ok(Err(format!("{path} is not an override path.")));
    }
    let Decision::Overridden {
        tier,
        verified,
        reason,
        quote,
    } = d
    else {
        return Ok(Err("Only an override is recorded.".into()));
    };
    // The start gate's shape: the tool path, reason tier, unverified, no quote.
    if waived.is_empty() || waived.len() > 64 {
        return Ok(Err("An override names the rule it waived.".into()));
    }
    if gate == "start" && (path != "tool" || *tier != Tier::Reason || *verified || quote.is_some())
    {
        return Ok(Err(
            "A start override is the tool path at the reason tier, unverified, with no quote."
                .into(),
        ));
    }
    let body = json!({
        "gate": gate,
        "path": path,
        "tier": match tier { Tier::Reason => "reason", Tier::Quote => "quote" },
        "terminal": false,
        "command": command.chars().take(MAX_COMMAND).collect::<String>(),
        "reason": reason.chars().take(MAX_REASON).collect::<String>(),
        "userQuote": quote,
        "verified": verified,
        "waived": [waived],
    })
    .to_string();
    let id = store::event(
        tx,
        chat_id,
        run,
        &Event {
            kind: "guard_override",
            verdict: Some(waived),
            body: Some(&body),
            ..Default::default()
        },
        now,
    )?;
    Ok(Ok(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deny(tier: Tier) -> Verdict {
        Verdict::Deny {
            tier,
            rule: "task_repo",
            why: "landing on main".into(),
        }
    }

    #[test]
    fn the_marker_reads_a_reason_and_an_optional_quote() {
        assert_eq!(marker("git push"), None);
        assert_eq!(
            marker("# helm-override: hotfix\ngit push origin main"),
            Some(Marker {
                reason: "hotfix".into(),
                quote: None
            })
        );
        let m = marker("# helm-override: hotfix [user: \"push it to main\"]\ngit push").unwrap();
        assert_eq!(m.quote.as_deref(), Some("push it to main"));
        assert_eq!(
            marker("# helm-override:   \ngit push"),
            None,
            "a reason is required"
        );
        assert_eq!(
            marker("echo hi\n# helm-override: x"),
            None,
            "only the first line counts"
        );
    }

    #[test]
    fn quotes_must_be_the_users_own_affirmative_words() {
        let said = vec![
            "Yes, push the hotfix to main now.".to_owned(),
            "No, go ahead.".to_owned(),
            "should we push to main?".to_owned(),
        ];
        assert!(quote_ok("push the hotfix to main", &said));
        assert!(quote_ok("“Push  the HOTFIX to main”", &said), "normalised");
        assert!(
            quote_ok("No, go ahead.", &said),
            "a leading No, is an interjection"
        );
        assert!(!quote_ok("push to main", &said), "inside a question");
        assert!(!quote_ok("push it", &said), "not something the user said");
        assert!(!quote_ok("hotfix", &said), "too short a fragment");
        assert!(!quote_ok("No.", &["No.".to_owned()]), "a bare No is a no");
        let later = vec!["merge it into main once CI passes, but not now".to_owned()];
        assert!(
            !quote_ok("merge it into main", &later),
            "taken back in the same clause"
        );
        let sub = vec!["please submit to main today".to_owned()];
        assert!(!quote_ok("it to main today", &sub), "whole words only");
        let neg = vec!["do not push the hotfix to main".to_owned()];
        assert!(!quote_ok("do not push the hotfix to main", &neg));
        assert!(!quote_ok("not push the hotfix", &neg));
    }

    #[test]
    fn the_tiers_need_their_own_proof() {
        let said = vec!["merge it into main please".to_owned()];
        assert_eq!(decide("git push", &Verdict::Allow, &said), Decision::Allow);
        assert!(matches!(
            decide("git push", &deny(Tier::Reason), &said),
            Decision::Refuse(_)
        ));
        assert!(matches!(
            decide(
                "# helm-override: docs only\ngit push",
                &deny(Tier::Reason),
                &said
            ),
            Decision::Overridden {
                tier: Tier::Reason,
                verified: false,
                ..
            }
        ));
        // A reason alone never covers the quote tier.
        let r = decide(
            "# helm-override: docs only\ngit push",
            &deny(Tier::Quote),
            &said,
        );
        assert!(
            matches!(&r, Decision::Refuse(t) if t.contains("[user:")),
            "{r:?}"
        );
        let bad = decide(
            "# helm-override: x [user: \"ship it now\"]\ngit push",
            &deny(Tier::Quote),
            &said,
        );
        assert!(
            matches!(&bad, Decision::Refuse(t) if t.contains("do not match")),
            "{bad:?}"
        );
        let ok = decide(
            "# helm-override: asked [user: \"merge it into main\"]\ngit push",
            &deny(Tier::Quote),
            &said,
        );
        assert!(matches!(
            ok,
            Decision::Overridden {
                tier: Tier::Quote,
                verified: true,
                ..
            }
        ));
    }

    #[test]
    fn an_override_writes_one_row_and_only_for_known_gates() {
        let (_d, db) = crate::mode::tests::db(&[("c", "claude")]);
        let d = Decision::Overridden {
            tier: Tier::Reason,
            verified: false,
            reason: "r".into(),
            quote: None,
        };
        db.write::<_, rusqlite::Error>(|tx| {
            assert!(record(
                tx,
                "c",
                None,
                "edit",
                "comment",
                &d,
                "git push",
                "task_repo",
                1
            )?
            .is_err());
            assert!(record(
                tx,
                "c",
                None,
                "bash",
                "comment",
                &Decision::Allow,
                "x",
                "task_repo",
                1
            )?
            .is_err());
            assert!(record(
                tx,
                "c",
                None,
                "bash",
                "comment",
                &d,
                &"x".repeat(5000),
                "task_repo",
                1
            )?
            .is_ok());
            let (n, body): (i64, String) = tx.query_row(
                "SELECT count(*), max(body) FROM workflow_events WHERE kind = 'guard_override'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(n, 1);
            let v: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(v["command"].as_str().unwrap().len(), 2000);
            assert_eq!(v["tier"], "reason");
            Ok(())
        })
        .unwrap();
    }
}
