//! The gate's decision table, kept pure so one table can test all of it.
//!
//! [`route`] takes what the model answered and what the card offers, and says what the gate
//! does. Whatever the model says, these rows force the question to the user:
//!
//! - the step cannot be undone, or the model rates its risk high;
//! - the model's confidence is below the bar from the settings;
//! - the choice is not one of the offered labels. A card's free-text box is for the user: the
//!   gate never writes its own words back to the agent as their answer, so a steered judge
//!   cannot hand the agent instructions;
//! - the question, its header, the chosen option or that option's description matches the
//!   hard-to-undo word list ([`hard_to_undo`]);
//! - the chat runs on a local model: its words never go to Claude, and this build has no local
//!   judge, so the card goes to the user as it is.
//!
//! "Merge" is deliberately not on the word list: low-risk work merges and is reviewed after;
//! the landing guard and the review flow still govern the merge itself.
//!
//! [`Limiter`] holds the limits: at most two judgements at once and twenty waiting their turn,
//! sixty an hour; past any of them the card passes to the user. A waiting judgement starts only
//! when a running one ends. Three failures in ten minutes pause the gate for fifteen minutes.

use serde_json::Value;

/// How risky the model judged the step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Risk {
    Low,
    Medium,
    High,
}

impl Risk {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Why a card never reached the judge. Each says so in the audit row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassReason {
    /// Not a question the gate may judge (a permission card, a trust card, a secret input).
    Safety,
    /// Over the running, queue or hourly limits.
    Limit,
    /// The gate is paused after too many failures.
    Paused,
}

/// What the gate does with one card.
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// Straight to the user, with the reason for the audit.
    Pass(PassReason),
    /// Answer for the user with this offered choice.
    Decide {
        choice: String,
        reason: String,
        confidence: f64,
        risk: Risk,
    },
    /// This one needs the user: hand it over with a write-up (the caller scrubs the model's
    /// brief or builds a plain one).
    Escalate,
    /// The judge could not be asked, or answered something unusable: the card becomes normal
    /// with a quiet note.
    Failed,
}

/// What the judge answered. `brief` is kept by the caller for the escalation; the route only
/// looks at the decision itself.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelAnswer {
    /// True when the judge decided; false when it says the user should be asked.
    pub decide: bool,
    pub choice: Option<String>,
    pub reason: String,
    pub confidence: f64,
    pub risk: Risk,
    pub reversible: bool,
    pub brief: Option<Value>,
}

/// The card facts the route needs.
#[derive(Clone, Debug, PartialEq)]
pub struct CardFacts {
    /// The question in the user's terms (title and message joined), for the word list.
    pub question: String,
    /// The card's short topic line, if it has one.
    pub header: String,
    pub options: Vec<String>,
    /// Each option's description, in the options' order (empty where an option has none).
    /// A question often keeps its substance here rather than in its labels.
    pub descriptions: Vec<String>,
}

/// The decision for one answered card.
pub fn route(local_chat: bool, bar: f64, card: &CardFacts, a: &ModelAnswer) -> Decision {
    // A local chat's words never go to Claude, and this build has no local judge.
    if local_chat {
        return Decision::Escalate;
    }
    if !a.decide {
        return Decision::Escalate;
    }
    if !a.reversible || a.risk == Risk::High {
        return Decision::Escalate;
    }
    if a.confidence < bar {
        return Decision::Escalate;
    }
    let Some(choice) = a.choice.as_deref().map(str::trim) else {
        return Decision::Escalate;
    };
    if choice.is_empty() {
        return Decision::Escalate;
    }
    let Some(at) = card.options.iter().position(|o| o.trim() == choice) else {
        return Decision::Escalate;
    };
    let description = card.descriptions.get(at).map(String::as_str).unwrap_or("");
    if [card.question.as_str(), &card.header, choice, description]
        .iter()
        .any(|t| hard_to_undo(t))
    {
        return Decision::Escalate;
    }
    Decision::Decide {
        choice: choice.to_owned(),
        reason: a.reason.clone(),
        confidence: a.confidence,
        risk: a.risk,
    }
}

/// Multi-word phrases checked against the whole text.
const HARD_PHRASES: &[&str] = &[
    "force push",
    "force-push",
    "push --force",
    "rewrite history",
    "rewriting history",
    "reset --hard",
    "api key",
    "api keys",
];

/// Single words that mark a step as hard to undo. Checked against word tokens, so "dropdown"
/// and "delivery" pass while "delete" and "live" do not. The list errs toward asking: an
/// over-escalated question costs a moment, a wrongly decided one costs the user work.
const HARD_WORDS: &[&str] = &[
    // deleting, wiping or dropping data
    "delete",
    "deleted",
    "deletes",
    "deleting",
    "deletion",
    "deletions",
    "wipe",
    "wiped",
    "wipes",
    "wiping",
    "drop",
    "drops",
    "dropping",
    "dropped",
    "erase",
    "erased",
    "erases",
    "erasing",
    "overwrite",
    "overwrites",
    "overwriting",
    "overwritten",
    // force-pushing or rewriting history
    "forcepush",
    "rebase",
    "rebasing",
    "uncommit",
    // paying, buying or subscribing
    "pay",
    "pays",
    "paid",
    "paying",
    "payment",
    "payments",
    "buy",
    "buys",
    "buying",
    "bought",
    "purchase",
    "purchases",
    "purchasing",
    "subscribe",
    "subscribes",
    "subscribing",
    "subscription",
    "subscriptions",
    "invoice",
    "invoices",
    "billing",
    // sending something to other people
    "email",
    "emails",
    "emailing",
    "publish",
    "publishes",
    "publishing",
    "published",
    "publication",
    "publications",
    "post",
    "posts",
    "posting",
    "posted",
    "send",
    "sends",
    "sending",
    "sent",
    "tweet",
    "tweets",
    // credentials
    "password",
    "passwords",
    "passphrase",
    "passphrases",
    "token",
    "tokens",
    "secret",
    "secrets",
    "credential",
    "credentials",
    "apikey",
    "apikeys",
    "accesskey",
    "accesskeys",
    // deploying, or anything live or in production
    "deploy",
    "deploys",
    "deploying",
    "deployed",
    "deployment",
    "deployments",
    "production",
    "live",
];

/// True when the text matches the hard-to-undo list.
pub fn hard_to_undo(text: &str) -> bool {
    let lower = text.to_lowercase();
    if HARD_PHRASES.iter().any(|p| lower.contains(p)) {
        return true;
    }
    lower
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| HARD_WORDS.contains(&w))
}

/// The limits and the failure pause. Not pure (it counts), but table-testable: every call
/// carries `now`.
#[derive(Default)]
pub struct Limiter {
    /// Judgements running now and waiting their turn (the running ones first).
    pub running: usize,
    pub waiting: usize,
    /// When each judgement started this session (only the last hour is kept).
    pub starts: Vec<i64>,
    /// When each judgement failed (only the last ten minutes are kept).
    pub failures: Vec<i64>,
    /// Until when the gate is paused (it passes everything).
    pub paused_until: i64,
}

pub const MAX_RUNNING: usize = 2;
pub const MAX_WAITING: usize = 20;
pub const MAX_PER_HOUR: usize = 60;
pub const FAILURE_WINDOW_MS: i64 = 10 * 60_000;
pub const FAILURE_TRIP: usize = 3;
pub const PAUSE_MS: i64 = 15 * 60_000;

impl Limiter {
    /// Whether a judgement may start, and in which slot. The slot must be given back with
    /// [`Limiter::release`] when the judgement ends.
    pub fn admit(&mut self, now: i64) -> Result<Slot, PassReason> {
        self.failures.retain(|t| now - t < FAILURE_WINDOW_MS);
        if now < self.paused_until {
            return Err(PassReason::Paused);
        }
        self.starts.retain(|t| now - t < 3_600_000);
        if self.starts.len() >= MAX_PER_HOUR {
            return Err(PassReason::Limit);
        }
        let slot = if self.running < MAX_RUNNING {
            self.running += 1;
            Slot::Running
        } else if self.waiting < MAX_WAITING {
            self.waiting += 1;
            Slot::Waiting
        } else {
            return Err(PassReason::Limit);
        };
        self.starts.push(now);
        Ok(slot)
    }

    /// A waiting job asks to run. True when it may: a running slot was free, and it moved
    /// from the waiting count to the running one. False leaves it waiting. A job admitted
    /// straight into a running slot may always run.
    pub fn start_running(&mut self, slot: Slot) -> bool {
        match slot {
            Slot::Running => true,
            Slot::Waiting if self.running < MAX_RUNNING => {
                self.waiting = self.waiting.saturating_sub(1);
                self.running += 1;
                true
            }
            Slot::Waiting => false,
        }
    }

    /// A judgement ended: give the slot back.
    pub fn release(&mut self, slot: Slot) {
        match slot {
            Slot::Running => self.running = self.running.saturating_sub(1),
            Slot::Waiting => self.waiting = self.waiting.saturating_sub(1),
        }
    }

    /// A judgement failed. Three in ten minutes pause the gate for fifteen minutes.
    pub fn note_failure(&mut self, now: i64) -> bool {
        self.failures.push(now);
        self.failures.retain(|t| now - t < FAILURE_WINDOW_MS);
        if self.failures.len() >= FAILURE_TRIP {
            self.paused_until = now + PAUSE_MS;
            self.failures.clear();
            true
        } else {
            false
        }
    }

    pub fn paused_until(&self, now: i64) -> Option<i64> {
        (now < self.paused_until).then_some(self.paused_until)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Running,
    Waiting,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> CardFacts {
        CardFacts {
            question: "Which name should the new folder use?".into(),
            header: "Folder".into(),
            options: vec!["reports".into(), "archive".into()],
            descriptions: vec!["Next to the other reports".into(), String::new()],
        }
    }

    fn answer() -> ModelAnswer {
        ModelAnswer {
            decide: true,
            choice: Some("reports".into()),
            reason: "The chat already writes its output there.".into(),
            confidence: 0.8,
            risk: Risk::Low,
            reversible: true,
            brief: None,
        }
    }

    #[test]
    fn the_route() {
        let bar = 0.75;
        type Edit = fn(&mut ModelAnswer, &mut CardFacts);
        let cases: Vec<(&str, Edit, Decision)> = vec![
            (
                "a safe answer of an offered label decides",
                |_, _| {},
                Decision::Decide {
                    choice: "reports".into(),
                    reason: "The chat already writes its output there.".into(),
                    confidence: 0.8,
                    risk: Risk::Low,
                },
            ),
            (
                "the judge says ask",
                |a, _| a.decide = false,
                Decision::Escalate,
            ),
            (
                "the step cannot be undone",
                |a, _| a.reversible = false,
                Decision::Escalate,
            ),
            ("high risk", |a, _| a.risk = Risk::High, Decision::Escalate),
            (
                "confidence just under the bar",
                |a, _| a.confidence = 0.749,
                Decision::Escalate,
            ),
            (
                "confidence exactly at the bar",
                |a, _| a.confidence = 0.75,
                Decision::Decide {
                    choice: "reports".into(),
                    reason: "The chat already writes its output there.".into(),
                    confidence: 0.75,
                    risk: Risk::Low,
                },
            ),
            (
                "a choice that is not an offered label",
                |a, _| a.choice = Some("something else".into()),
                Decision::Escalate,
            ),
            (
                "free text never decides, even where the card has a box for it",
                |a, _| a.choice = Some("write it down instead".into()),
                Decision::Escalate,
            ),
            (
                "the chosen option's description names dropping",
                |a, c| {
                    c.question = "Which cleanup?".into();
                    c.options = vec!["Option A".into(), "Option B".into()];
                    c.descriptions = vec!["drops the users table".into(), "keeps it".into()];
                    a.choice = Some("Option A".into());
                },
                Decision::Escalate,
            ),
            (
                "the header names production",
                |_, c| c.header = "Production".into(),
                Decision::Escalate,
            ),
            (
                "a hard word in an option not chosen leaves the choice alone",
                |a, c| {
                    c.descriptions = vec![String::new(), "deletes the old one".into()];
                    a.choice = Some("reports".into());
                },
                Decision::Decide {
                    choice: "reports".into(),
                    reason: "The chat already writes its output there.".into(),
                    confidence: 0.8,
                    risk: Risk::Low,
                },
            ),
            (
                "no choice at all",
                |a, _| a.choice = None,
                Decision::Escalate,
            ),
            (
                "the question names deleting",
                |_, c| c.question = "May I delete the old backups?".into(),
                Decision::Escalate,
            ),
            (
                "the chosen option names deploying",
                |a, c| {
                    a.choice = Some("deploy it".into());
                    c.options.push("deploy it".into());
                },
                Decision::Escalate,
            ),
        ];
        for (name, edit, want) in cases {
            let mut c = card();
            let mut a = answer();
            edit(&mut a, &mut c);
            assert_eq!(route(false, bar, &c, &a), want, "{name}");
        }
        // Merge is deliberately off the word list: an offered "merge" decides.
        let mut c = card();
        c.options.push("merge it".into());
        let mut a = answer();
        a.choice = Some("merge it".into());
        assert!(matches!(route(false, bar, &c, &a), Decision::Decide { .. }));
        // A local chat's card goes to the user as it is, whatever the answer says.
        let c = card();
        let a = answer();
        assert_eq!(route(true, bar, &c, &a), Decision::Escalate);
    }

    #[test]
    fn the_hard_to_undo_list() {
        for t in [
            "delete the folder",
            "May I wipe the cache?",
            "drop the old table",
            "overwrite the config",
            "force push the branch",
            "push --force to main",
            "rewrite history",
            "reset --hard to the last commit",
            "pay the £58.99 bill",
            "buy the extra seats",
            "subscribe to the plan",
            "email the report to the client",
            "publish the site",
            "post the reply",
            "send the announcement",
            "rotate the API key",
            "change the password",
            "read the token file",
            "store this secret",
            "deploy the new build",
            "this touches production",
            "is it live now?",
        ] {
            assert!(hard_to_undo(t), "{t}");
        }
        for t in [
            "Which name should the folder use?",
            "Merge the low-risk change?",
            "The dropdown shows two colours.",
            "Delivery went on Tuesday.",
            "Should the tests run first, or afterwards?",
            "Which colour for the banner?",
        ] {
            assert!(!hard_to_undo(t), "{t}");
        }
    }

    #[test]
    fn the_limiter_rows() {
        let t0 = 1_000_000_000;
        let mut l = Limiter::default();
        assert_eq!(l.admit(t0), Ok(Slot::Running));
        assert_eq!(l.admit(t0), Ok(Slot::Running));
        assert_eq!(l.admit(t0), Ok(Slot::Waiting), "a third job waits its turn");
        for _ in 0..(MAX_WAITING - 1) {
            assert_eq!(l.admit(t0), Ok(Slot::Waiting));
        }
        assert_eq!(
            l.admit(t0),
            Err(PassReason::Limit),
            "and twenty is the most waiting"
        );
        assert!(
            !l.start_running(Slot::Waiting),
            "a waiting job never runs past the limit"
        );
        assert_eq!((l.running, l.waiting), (MAX_RUNNING, MAX_WAITING));
        assert!(l.start_running(Slot::Running), "a running slot just runs");
        l.release(Slot::Running);
        assert!(
            l.start_running(Slot::Waiting),
            "a running job ended: the next waiting one takes its place"
        );
        assert_eq!((l.running, l.waiting), (MAX_RUNNING, MAX_WAITING - 1));
        l.running = 0;
        l.waiting = 0;
        l.starts.clear();
        // The hourly ceiling counts every judgement started, however few at once.
        l.starts = (0..MAX_PER_HOUR as i64).map(|i| t0 + i).collect();
        assert_eq!(l.admit(t0 + 60), Err(PassReason::Limit));
        l.starts.clear();
        assert_eq!(l.admit(t0 + 3_600_001), Ok(Slot::Running));
        // Three failures in ten minutes pause fifteen; the pause passes everything.
        let mut l = Limiter::default();
        assert!(!l.note_failure(t0));
        assert!(!l.note_failure(t0 + 60_000));
        assert!(
            l.note_failure(t0 + 120_000),
            "three failures in ten minutes trip the pause"
        );
        assert_eq!(l.paused_until(t0 + 120_000), Some(t0 + 120_000 + PAUSE_MS));
        assert_eq!(l.admit(t0 + 130_000), Err(PassReason::Paused));
        assert_eq!(l.admit(t0 + 120_000 + PAUSE_MS), Ok(Slot::Running));
        let mut l = Limiter::default();
        l.note_failure(t0);
        l.note_failure(t0 + 60_000);
        assert!(
            !l.note_failure(t0 + FAILURE_WINDOW_MS),
            "the window slides: the oldest has aged out"
        );
        let mut l = Limiter::default();
        l.note_failure(t0);
        l.note_failure(t0 + 60_000);
        assert_eq!(
            l.admit(t0 + FAILURE_WINDOW_MS + 1),
            Ok(Slot::Running),
            "two failures and an hour apart change nothing"
        );
    }
}
