//! The two ready-made schedules (a 30-minute check-in and a
//! noon digest). They are a catalogue, not rows: nothing exists, and so nothing fires, until the
//! user enables one for a chat of their choice.

use crate::types::{Template, Timing};

pub const CHECK_IN: &str = "check-in";
pub const NOON_DIGEST: &str = "noon-digest";

pub fn templates() -> Vec<Template> {
    vec![
        Template {
            id: CHECK_IN.into(),
            title: "30-minute check-in".into(),
            summary: "Every 30 minutes the agent says what it finished, what it is doing and what blocks it.".into(),
            message: "Check-in. In a few lines, tell me what you have finished since the last \
check-in, what you are doing now and anything that is blocking you. If you are waiting on a \
decision from me, say so first. If nothing has changed, reply with one line saying so."
                .into(),
            timing: Timing::Interval { seconds: 1800, start_at: None },
        },
        Template {
            id: NOON_DIGEST.into(),
            title: "Noon digest".into(),
            summary: "Every day at 12:00 local time the agent summarises the day so far.".into(),
            message: "Noon digest. Write a short digest of this conversation's progress so far \
today: what changed, what is still open and what you plan next. Keep it under 150 words and lead \
with anything that needs my attention."
                .into(),
            timing: Timing::Weekly { hour: 12, minute: 0, days: vec![0, 1, 2, 3, 4, 5, 6] },
        },
    ]
}

pub fn by_id(id: &str) -> Option<Template> {
    templates().into_iter().find(|t| t.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_templates_are_well_formed() {
        let all = templates();
        assert_eq!(all.len(), 2);
        for t in &all {
            assert!(crate::timing::check(&t.timing).is_none(), "{} timing", t.id);
            assert!(
                crate::store::check_text(&t.title, &t.message).is_none(),
                "{} text",
                t.id
            );
        }
        let c = by_id(CHECK_IN).unwrap();
        assert_eq!(
            c.timing,
            Timing::Interval {
                seconds: 1800,
                start_at: None
            }
        );
        let n = by_id(NOON_DIGEST).unwrap();
        assert_eq!(
            n.timing,
            Timing::Weekly {
                hour: 12,
                minute: 0,
                days: vec![0, 1, 2, 3, 4, 5, 6]
            }
        );
        assert!(by_id("nope").is_none());
    }
}
