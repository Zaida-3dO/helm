//! The host's pass over the judge's write-up.
//!
//! The brief is written for the user in their own terms, but it comes out of a machine
//! conversation, so the host checks it: every UUID and every run of seven or more hex
//! characters is stripped (ids and commit hashes), every field is capped, and the shape is
//! verified. A brief that is still unusable is replaced with a plain one built from the
//! original card ([`fallback_brief`]).

use helm_chats::types::{GateBrief, GateBriefOption, Interaction};
use serde_json::Value;

pub const MAX_PROBLEM: usize = 400;
pub const MAX_LABEL: usize = 80;
pub const MAX_LINE: usize = 240;
pub const MAX_RECOMMENDATION: usize = 160;
pub const MAX_OPTIONS: usize = 4;

/// The length of a `8-4-4-4-12` hex UUID starting at `i`, if one starts there.
fn uuid_len_at(chars: &[char], i: usize) -> Option<usize> {
    let hex = |i: usize, n: usize| -> Option<usize> {
        (chars[i..].len() >= n && chars[i..i + n].iter().all(|c| c.is_ascii_hexdigit()))
            .then_some(i + n)
    };
    let mut at = hex(i, 8)?;
    if chars.get(at) != Some(&'-') {
        return None;
    }
    at += 1;
    for n in [4, 4, 4] {
        at = hex(at, n)?;
        if chars.get(at) != Some(&'-') {
            return None;
        }
        at += 1;
    }
    at = hex(at, 12)?;
    // A longer tail of hex is not an id we recognise.
    if chars
        .get(at)
        .is_some_and(|c| c.is_ascii_hexdigit() || *c == '-')
    {
        return None;
    }
    Some(at - i)
}

/// Strip UUIDs and runs of seven or more hex characters, and squeeze the spaces left behind.
pub fn scrub(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if let Some(len) = uuid_len_at(&chars, i) {
            i += len;
            continue;
        }
        if chars[i].is_ascii_hexdigit() {
            let mut j = i;
            while j < chars.len() && chars[j].is_ascii_hexdigit() {
                j += 1;
            }
            if j - i < 7 {
                // Short enough to be part of a word; long enough is an id or a hash.
                out.extend(&chars[i..j]);
            }
            i = j;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    let mut spaces = String::with_capacity(out.len());
    let mut last_space = false;
    for c in out.chars() {
        let space = c == ' ' || c == '\t';
        if space && last_space {
            continue;
        }
        last_space = space;
        spaces.push(c);
    }
    spaces.trim().to_owned()
}

fn cap(s: &str, max: usize) -> String {
    scrub(s).chars().take(max).collect()
}

/// The judge's brief, checked and cleaned: 1–4 options, a real problem line, everything
/// scrubbed and capped. `None` when it is unusable.
pub fn clean_brief(raw: &serde_json::Value) -> Option<GateBrief> {
    let o = raw.as_object()?;
    let problem = cap(o.get("problem")?.as_str()?, MAX_PROBLEM);
    if problem.is_empty() {
        return None;
    }
    let raw_options = o.get("options")?.as_array()?;
    if raw_options.is_empty() || raw_options.len() > MAX_OPTIONS {
        return None;
    }
    let mut options = Vec::new();
    for r in raw_options {
        let label = cap(r.get("label")?.as_str()?, MAX_LABEL);
        if label.is_empty() {
            return None;
        }
        let field = |k: &str| -> String {
            r.get(k)
                .and_then(Value::as_str)
                .map(|s| cap(s, MAX_LINE))
                .unwrap_or_default()
        };
        options.push(GateBriefOption {
            label,
            means: field("means"),
            pros: field("pros"),
            cons: field("cons"),
            cost: field("cost"),
        });
    }
    Some(GateBrief {
        problem,
        options,
        recommendation: cap(o.get("recommendation")?.as_str()?, MAX_RECOMMENDATION),
        why: cap(o.get("why")?.as_str()?, MAX_LINE),
    })
}

/// The card's question in one line: its title and message joined.
pub fn question_of(i: &Interaction) -> String {
    let title = i.title.clone().unwrap_or_default();
    let message = i.message.clone().unwrap_or_default();
    let both = if message.trim().is_empty() {
        title
    } else if title.trim().is_empty() {
        message
    } else {
        format!("{title} — {message}")
    };
    let one = both.split_whitespace().collect::<Vec<_>>().join(" ");
    one.chars().take(4 * MAX_PROBLEM).collect()
}

/// A plain write-up built from the card itself, used when the judge's is unusable: the
/// question as asked, and its own options.
pub fn fallback_brief(i: &Interaction) -> GateBrief {
    GateBrief {
        problem: cap(&question_of(i), MAX_PROBLEM),
        options: i
            .options
            .clone()
            .unwrap_or_default()
            .into_iter()
            .take(MAX_OPTIONS)
            .map(|label| GateBriefOption {
                label: cap(&label, MAX_LABEL),
                means: String::new(),
                pros: String::new(),
                cons: String::new(),
                cost: String::new(),
            })
            .collect(),
        recommendation: String::new(),
        why: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scrubbing_ids_and_caps() {
        assert_eq!(
            scrub("Commit d3adb17c3d5e7f80 was pushed as deadbee (see 1234567)."),
            "Commit was pushed as (see )."
        );
        assert_eq!(
            scrub("chat 12ab34cd-56ef-78ab-90cd-1234567890ab waiting"),
            "chat waiting"
        );
        assert_eq!(
            scrub("short hex like cafe or beef stays"),
            "short hex like cafe or beef stays"
        );
        assert_eq!(scrub("delegated 8192 items"), "delegated 8192 items");
        assert_eq!(scrub("id 12345678 gone"), "id gone");
        let b = clean_brief(&json!({
            "problem": "Delete 9fe12a4b and rebuild?",
            "options": [{
                "label": "Yes",
                "means": "Removes the old copies <x>".repeat(40),
                "pros": null,
                "cons": "no way back",
                "cost": "5 min"
            }, {
                "label": "No",
                "means": "leave them",
                "pros": "safe",
                "cons": "they stay",
                "cost": ""
            }],
            "recommendation": "No",
            "why": "it costs nothing to wait"
        }))
        .unwrap();
        assert_eq!(b.problem, "Delete and rebuild?");
        assert!(b.options[0].means.chars().count() <= MAX_LINE);
        assert_eq!(b.options[0].pros, "", "a missing field reads empty");
        assert_eq!(b.recommendation, "No");
        assert_eq!(b.options.len(), 2);
        assert!(
            clean_brief(&json!({"problem": "", "options": [{"label": "x"}],
            "recommendation": "", "why": ""}))
            .is_none()
        );
        assert!(
            clean_brief(&json!({"problem": "p", "options": [], "recommendation": "", "why": ""}))
                .is_none(),
            "no options is unusable"
        );
        assert!(clean_brief(
            &json!({"problem": "p", "options": [{"label": "a"},{"label":"b"},{"label":"c"},{"label":"d"},{"label":"e"}],
                   "recommendation": "", "why": ""})
        )
        .is_none());
        assert!(clean_brief(&json!("junk")).is_none());
    }

    #[test]
    fn the_fallback_comes_from_the_card() {
        let mut i = Interaction {
            chat_id: "c".into(),
            id: "r".into(),
            method: helm_chats::types::InteractionMethod::Select,
            attention: Some(helm_chats::types::InteractionAttention::Question),
            title: Some("Which port?".into()),
            header: None,
            message: Some("Two are free.".into()),
            context: None,
            options: Some(vec!["8080".into(), "9090".into()]),
            option_descriptions: None,
            allow_other: None,
            secret: None,
            step: None,
            steps: None,
            answer: None,
            prefill: None,
            gate: None,
        };
        let b = fallback_brief(&i);
        assert_eq!(b.problem, "Which port? — Two are free.");
        assert_eq!(b.options.len(), 2);
        assert_eq!(b.options[0].label, "8080");
        assert_eq!(b.recommendation, "", "the fallback recommends nothing");
        i.options = None;
        assert!(fallback_brief(&i).options.is_empty());
        assert_eq!(question_of(&i), "Which port? — Two are free.");
    }
}
