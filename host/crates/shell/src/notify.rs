//! The rules behind the badge, the taskbar flash and notifications.
//! Pure state machines; the service feeds them events and acts on the answers.

use helm_chats::types::{AttentionKind, AttentionSummary, Completion};
use std::collections::{HashMap, HashSet, VecDeque};

/// How many completion ids are remembered as announced.
pub const ANNOUNCED_MAX: usize = 1024;
/// A completion older than this is never announced (no stale results after a restart).
pub const FRESH_MS: i64 = 30_000;

/// The overlay text for a badge count: none at 0, "1"-"9", then "9+".
pub fn badge_label(count: u32) -> Option<String> {
    match count {
        0 => None,
        1..=9 => Some(count.to_string()),
        _ => Some("9+".to_owned()),
    }
}

/// Decides which completions get a notification and when it is withdrawn.
#[derive(Default)]
pub struct Announcer {
    order: VecDeque<String>,
    seen: HashSet<String>,
    /// chat id → the completion id currently posted for it (one per chat).
    posted: HashMap<String, String>,
}

impl Announcer {
    pub fn new() -> Self {
        Self::default()
    }

    fn remember(&mut self, id: &str) -> bool {
        if self.seen.contains(id) {
            return false;
        }
        self.seen.insert(id.to_owned());
        self.order.push_back(id.to_owned());
        while self.order.len() > ANNOUNCED_MAX {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }

    /// True when this completion should be posted now. A newer one for the same chat replaces
    /// the older (the caller posts under the chat's own identifier).
    pub fn on_completion(&mut self, c: &Completion, now_ms: i64) -> bool {
        if c.read || c.dismissed {
            return false;
        }
        if !self.remember(&c.id) {
            return false;
        }
        if now_ms - c.completed_at > FRESH_MS {
            return false;
        }
        self.posted.insert(c.chat_id.clone(), c.id.clone());
        true
    }

    /// Chats whose posted notification must be withdrawn: they are no longer unread in the
    /// summary (read, dismissed, started new work, archived). Withdraw-on-read reads the chats crate's
    /// attention summary, as the brief says.
    pub fn withdrawals(&mut self, s: &AttentionSummary) -> Vec<String> {
        let unread: HashSet<&str> = s.unread.iter().map(|r| r.chat_id.as_str()).collect();
        let mut gone: Vec<String> = self
            .posted
            .keys()
            .filter(|c| !unread.contains(c.as_str()))
            .cloned()
            .collect();
        gone.sort();
        for c in &gone {
            self.posted.remove(c);
        }
        gone
    }

    /// Forget every posted notification (the mode changed); returns their chats.
    pub fn withdraw_all(&mut self) -> Vec<String> {
        let mut all: Vec<String> = self.posted.drain().map(|(k, _)| k).collect();
        all.sort();
        all
    }

    pub fn is_posted(&self, chat_id: &str) -> bool {
        self.posted.contains_key(chat_id)
    }
}

/// One taskbar flash per newly arriving blocking request.
#[derive(Default)]
pub struct FlashTracker {
    pending: HashSet<(String, String)>,
    primed: bool,
}

fn blocking(kind: AttentionKind) -> bool {
    matches!(
        kind,
        AttentionKind::Approval | AttentionKind::Question | AttentionKind::Plan
    )
}

impl FlashTracker {
    /// True when the summary holds a blocking request that the previous one did not. The first
    /// summary (launch) only primes the tracker: old requests do not flash.
    pub fn observe(&mut self, s: &AttentionSummary) -> bool {
        let now: HashSet<(String, String)> = s
            .needs_you
            .iter()
            .filter(|r| blocking(r.kind))
            .map(|r| (r.chat_id.clone(), format!("{:?}", r.kind)))
            .collect();
        let fresh = self.primed && now.iter().any(|k| !self.pending.contains(k));
        self.pending = now;
        self.primed = true;
        fresh
    }
}

/// Keep the machine awake while any chat is busy and the setting is on.
pub fn want_keep_awake(busy: u32, setting: bool, shutting_down: bool) -> bool {
    busy > 0 && setting && !shutting_down
}

#[cfg(test)]
mod tests {
    use super::*;
    use helm_chats::types::AttentionRow;

    fn completion(id: &str, chat: &str, at: i64) -> Completion {
        Completion {
            id: id.into(),
            chat_id: chat.into(),
            title: "T".into(),
            project: "P".into(),
            preview: "done".into(),
            completed_at: at,
            read: false,
            dismissed: false,
        }
    }

    fn summary(unread: &[&str], needs: &[(&str, AttentionKind)]) -> AttentionSummary {
        let row = |c: &str, k| AttentionRow {
            chat_id: c.into(),
            title: "T".into(),
            project: "P".into(),
            kind: k,
            reason: String::new(),
        };
        AttentionSummary {
            badge: 0,
            busy: 0,
            needs_you: needs.iter().map(|(c, k)| row(c, *k)).collect(),
            running: vec![],
            unread: unread
                .iter()
                .map(|c| row(c, AttentionKind::Unread))
                .collect(),
            stalled: vec![],
            flagged: vec![],
            revision: 1,
        }
    }

    #[test]
    fn badge_labels() {
        assert_eq!(badge_label(0), None);
        assert_eq!(badge_label(1).as_deref(), Some("1"));
        assert_eq!(badge_label(9).as_deref(), Some("9"));
        assert_eq!(badge_label(10).as_deref(), Some("9+"));
    }

    #[test]
    fn posts_once_per_id_only_while_fresh() {
        let mut a = Announcer::new();
        assert!(a.on_completion(&completion("k1", "c1", 1_000), 1_000 + FRESH_MS));
        assert!(
            !a.on_completion(&completion("k1", "c1", 1_000), 1_001),
            "announced once"
        );
        assert!(
            !a.on_completion(&completion("k2", "c2", 0), FRESH_MS + 1),
            "stale"
        );
        let mut read = completion("k3", "c3", 0);
        read.read = true;
        assert!(!a.on_completion(&read, 0));
    }

    #[test]
    fn the_announced_memory_is_bounded() {
        let mut a = Announcer::new();
        for i in 0..=ANNOUNCED_MAX {
            a.on_completion(&completion(&format!("k{i}"), "c", 0), 0);
        }
        assert!(
            a.on_completion(&completion("k0", "c", 0), 0),
            "the oldest id fell out"
        );
        assert!(!a.on_completion(&completion("k5", "c", 0), 0));
    }

    #[test]
    fn withdraws_when_the_chat_is_no_longer_unread() {
        let mut a = Announcer::new();
        a.on_completion(&completion("k1", "c1", 0), 0);
        a.on_completion(&completion("k2", "c2", 0), 0);
        assert!(a.withdrawals(&summary(&["c1", "c2"], &[])).is_empty());
        assert_eq!(a.withdrawals(&summary(&["c2"], &[])), ["c1"]);
        assert!(!a.is_posted("c1"));
        assert!(a.withdrawals(&summary(&["c2"], &[])).is_empty(), "once");
        assert_eq!(a.withdraw_all(), ["c2"]);
    }

    #[test]
    fn flashes_once_per_new_blocking_request_and_not_at_launch() {
        let mut f = FlashTracker::default();
        assert!(!f.observe(&summary(&[], &[("c1", AttentionKind::Approval)])));
        assert!(!f.observe(&summary(&[], &[("c1", AttentionKind::Approval)])));
        assert!(f.observe(&summary(
            &[],
            &[
                ("c1", AttentionKind::Approval),
                ("c2", AttentionKind::Question)
            ]
        )));
        assert!(!f.observe(&summary(&[], &[("c2", AttentionKind::Question)])));
        assert!(!f.observe(&summary(&[], &[("c3", AttentionKind::Running)])));
    }

    #[test]
    fn keep_awake_rule() {
        assert!(want_keep_awake(1, true, false));
        assert!(!want_keep_awake(0, true, false));
        assert!(!want_keep_awake(2, false, false));
        assert!(!want_keep_awake(2, true, true));
    }
}
