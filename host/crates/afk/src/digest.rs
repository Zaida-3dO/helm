//! The "While you were out" digest: the prompt, and the pure step that
//! decides what to do with one delivery row. The service carries the step out.

use chrono::{Local, TimeZone};

/// At most this many tries per digest, backing off between them.
pub const MAX_ATTEMPTS: u32 = 5;
pub const BACKOFF_SECS: [i64; 4] = [5, 15, 60, 300];

/// The built-in template, in Helm's own wording.
pub const DEFAULT_TEMPLATE: &str = "The user was away from {start} to {end} ({duration}) and is back now.

Write a short \"While you were out\" update for them, in this order:

1. Needs you: only what the user alone can act on now. For each, give the options and your recommendation.
2. Veto check: the judgement calls you made while they were away, and how to undo each one.
3. Done, with evidence (links, commits, test results).
4. In progress.
5. Left to do.
6. Anything else worth flagging.

What happened while they were away that waited for them:
{deferred}

Reply with the update only. Do not start new work, and never mark a pull request ready for review.";

pub fn hhmm(ms: i64) -> String {
    match Local.timestamp_millis_opt(ms).single() {
        Some(t) => t.format("%H:%M").to_string(),
        None => "??:??".into(),
    }
}

fn date_hhmm(ms: i64) -> String {
    match Local.timestamp_millis_opt(ms).single() {
        Some(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        None => "??:??".into(),
    }
}

fn same_day(a: i64, b: i64) -> bool {
    match (
        Local.timestamp_millis_opt(a).single(),
        Local.timestamp_millis_opt(b).single(),
    ) {
        (Some(x), Some(y)) => x.date_naive() == y.date_naive(),
        _ => true,
    }
}

pub fn duration_text(ms: i64) -> String {
    let mins = ms.max(0) / 60_000;
    if mins < 1 {
        "under a minute".into()
    } else if mins < 60 {
        format!("{mins}m")
    } else {
        format!("{}h {}m", mins / 60, mins % 60)
    }
}

/// Substitute `{start}`, `{end}`, `{duration}` and `{deferred}` in one pass: a value that itself
/// contains a placeholder stays literal.
pub fn substitute(template: &str, vals: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    'outer: while !rest.is_empty() {
        for (k, v) in vals {
            if let Some(after) = rest.strip_prefix(k) {
                out.push_str(v);
                rest = after;
                continue 'outer;
            }
        }
        let c = rest.chars().next().expect("non-empty");
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// The `{deferred}` lines for one chat: what AFK answered or left waiting, then the Allow requests
/// nobody decided.
pub fn deferred_lines(events: &[crate::store::EventRow], allows: &[String]) -> Vec<String> {
    let short = |s: &str| -> String { s.chars().take(200).collect() };
    let mut out: Vec<String> = events
        .iter()
        .map(|e| {
            let t = short(&e.title);
            let at = hhmm(e.at);
            match e.kind.as_str() {
                "question" => format!("- {at} Answered a question automatically: \"{t}\""),
                "plan_approved" => format!("- {at} Approved the plan automatically."),
                "plan_deferred" => format!("- {at} Waiting for you: plan approval. {t}"),
                _ => format!("- {at} Waiting for you: {t}"),
            }
        })
        .collect();
    out.extend(
        allows
            .iter()
            .map(|a| format!("- Allow request waiting for you: {}", short(a))),
    );
    out
}

/// The whole digest message for one chat and span.
pub fn compose(
    template: Option<&str>,
    span_id: &str,
    start: i64,
    end: i64,
    deferred: &[String],
) -> String {
    let template = template.unwrap_or(DEFAULT_TEMPLATE);
    let (s, e) = if same_day(start, end) {
        (hhmm(start), hhmm(end))
    } else {
        (date_hhmm(start), date_hhmm(end))
    };
    let dur = duration_text(end - start);
    let list = if deferred.is_empty() {
        "None recorded.".to_owned()
    } else {
        deferred.join("\n")
    };
    let mut body = substitute(
        template,
        &[
            ("{start}", &s),
            ("{end}", &e),
            ("{duration}", &dur),
            ("{deferred}", &list),
        ],
    );
    if !template.contains("{deferred}") {
        body.push_str("\n\n");
        body.push_str(&list);
    }
    format!("[Helm · Back from AFK · span {span_id}]\n{body}")
}

/// What to do with one live delivery row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing now (in flight, or not due yet).
    Wait,
    /// Move to this terminal or holding state, with an error text for `failed`.
    Mark(&'static str, Option<&'static str>),
    /// Send (again): a new submission with this attempt number.
    Send { attempt: u32 },
}

/// Decide a row's next step from its current submission's state.
pub fn step(
    state: &str,
    attempts: u32,
    next_attempt_at: Option<i64>,
    submission: Option<&str>,
    now: i64,
) -> Step {
    match submission {
        Some("accepted" | "delivered" | "complete") => return Step::Mark("sent", None),
        Some("queued") => {
            return if state == "queued" {
                Step::Wait
            } else {
                Step::Mark("queued", None)
            }
        }
        Some("dispatching" | "saved") => return Step::Wait,
        Some("withdrawn" | "superseded") => return Step::Mark("dismissed", None),
        Some("uncertain") => {
            return Step::Mark(
                "failed",
                Some("Delivery could not be confirmed; check the chat, then retry."),
            )
        }
        // Rejected, or no submission: (re)send below.
        _ => {}
    }
    if attempts >= MAX_ATTEMPTS {
        return Step::Mark("failed", Some("Gave up after 5 tries."));
    }
    if next_attempt_at.is_some_and(|t| t > now) {
        return Step::Wait;
    }
    Step::Send {
        attempt: attempts + 1,
    }
}

/// When to try again after the `attempts`-th failed send.
pub fn backoff_ms(attempts: u32) -> i64 {
    let i = (attempts.max(1) as usize - 1).min(BACKOFF_SECS.len() - 1);
    BACKOFF_SECS[i] * 1000
}

/// A deterministic submission id per (span, chat, attempt), so a retry of the same attempt is
/// idempotent and a resend never reuses a dead submission. Workers take only UUIDs, so it is a
/// name-based UUID (version 8, from a SHA-256 of the key).
pub fn submission_id(span_id: &str, chat_id: &str, attempt: u32) -> String {
    use sha2::Digest;
    let h = sha2::Sha256::digest(format!("helm-afk-digest/{span_id}/{chat_id}/{attempt}"));
    let mut b = [0u8; 16];
    b.copy_from_slice(&h[..16]);
    uuid::Uuid::new_v8(b).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitution_is_one_pass() {
        let out = substitute("a {x} b {y}", &[("{x}", "{y}"), ("{y}", "Y")]);
        assert_eq!(out, "a {y} b Y");
    }

    #[test]
    fn durations() {
        assert_eq!(duration_text(59_999), "under a minute");
        assert_eq!(duration_text(60_000), "1m");
        assert_eq!(duration_text(59 * 60_000), "59m");
        assert_eq!(duration_text(125 * 60_000), "2h 5m");
    }

    #[test]
    fn compose_has_header_and_appends_missing_deferred() {
        let m = compose(Some("Back after {duration}."), "s1", 0, 120 * 60_000, &[]);
        assert!(m.starts_with("[Helm · Back from AFK · span s1]\n"));
        assert!(m.contains("Back after 2h 0m."));
        assert!(m.ends_with("None recorded."), "{m}");
        let d = compose(None, "s2", 0, 60_000, &["- [question] x (10:00)".into()]);
        assert!(d.contains("- [question] x (10:00)"));
        assert!(!d.contains("{deferred}"));
        assert!(d.contains("never mark a pull request ready"));
    }

    #[test]
    fn delivery_steps() {
        assert_eq!(step("pending", 0, None, None, 0), Step::Send { attempt: 1 });
        assert_eq!(
            step("pending", 1, None, Some("delivered"), 0),
            Step::Mark("sent", None)
        );
        assert_eq!(
            step("pending", 1, None, Some("queued"), 0),
            Step::Mark("queued", None)
        );
        assert_eq!(step("queued", 1, None, Some("queued"), 0), Step::Wait);
        assert_eq!(
            step("queued", 1, None, Some("complete"), 0),
            Step::Mark("sent", None)
        );
        assert_eq!(step("pending", 1, None, Some("dispatching"), 0), Step::Wait);
        assert_eq!(
            step("pending", 1, None, Some("withdrawn"), 0),
            Step::Mark("dismissed", None)
        );
        assert!(matches!(
            step("pending", 1, None, Some("uncertain"), 0),
            Step::Mark("failed", Some(_))
        ));
        assert_eq!(
            step("pending", 2, None, Some("rejected"), 0),
            Step::Send { attempt: 3 }
        );
        assert_eq!(step("pending", 2, Some(10), None, 9), Step::Wait);
        assert_eq!(
            step("pending", 2, Some(10), None, 10),
            Step::Send { attempt: 3 }
        );
        assert_eq!(
            step("pending", 5, None, None, 0),
            Step::Mark("failed", Some("Gave up after 5 tries."))
        );
        assert_eq!(step("pending", 4, None, None, 0), Step::Send { attempt: 5 });
    }

    #[test]
    fn submission_ids_are_stable_uuids() {
        let a = submission_id("s", "c", 1);
        assert!(uuid::Uuid::parse_str(&a).is_ok(), "{a}");
        assert_eq!(a, submission_id("s", "c", 1));
        assert_ne!(a, submission_id("s", "c", 2));
        assert_ne!(a, submission_id("s", "d", 1));
    }

    #[test]
    fn backoff_schedule() {
        assert_eq!(backoff_ms(1), 5_000);
        assert_eq!(backoff_ms(2), 15_000);
        assert_eq!(backoff_ms(3), 60_000);
        assert_eq!(backoff_ms(4), 300_000);
        assert_eq!(backoff_ms(9), 300_000);
    }
}
