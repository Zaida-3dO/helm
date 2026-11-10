//! What Helm's Windows toasts say, and what a click on one does. Pure: the service decides when,
//! the app shows them and carries out a click.
//!
//! - **A finished turn** shows the chat's title and the start of the reply in plain text, with a
//!   reply box and Continue and Open buttons. Reply and Continue are the user's own sends, so they
//!   queue behind a running turn like any send.
//! - **A chat that needs the user** (a question, an approval, a plan, a flag) shows "<chat> needs
//!   you" and the question, with Open only. An approval is never answered from a toast: Helm
//!   shows the full request first.

use helm_chats::types::{AttentionKind, AttentionRow, AttentionSummary, Completion};
use helm_platform::toast::{tag_for, Activation, ReplyBox, ToastAction, ToastSpec, GROUP};
use helm_platform::trayicon::TrayDot;
use std::collections::HashSet;

/// Characters of the body.
pub const BODY_MAX: usize = 160;
/// The longest reply a toast sends, in bytes (the composer's own limit).
pub const REPLY_MAX_BYTES: usize = 16 * 1024;
/// What the Continue button sends.
pub const CONTINUE_TEXT: &str = "Continue";

/// The text of a reply with its Markdown marks taken out, folded to one line and cut at `max`
/// characters with an ellipsis.
pub fn plain(text: &str, max: usize) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("```") {
            continue;
        }
        let t = t.trim_start_matches('#').trim_start();
        let t = t.strip_prefix('>').map(str::trim_start).unwrap_or(t);
        let t = ["- ", "* ", "+ "]
            .iter()
            .find_map(|m| t.strip_prefix(m))
            .unwrap_or(t);
        let digits = t.chars().take_while(char::is_ascii_digit).count();
        let t = match t[digits..].strip_prefix(". ") {
            Some(rest) if digits > 0 => rest,
            _ => t,
        };
        lines.push(unlink(t));
    }
    let joined = lines.join(" ");
    let mut cleaned = String::with_capacity(joined.len());
    let chars: Vec<char> = joined.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '`' => {}
            '*' | '_' | '~' if next == Some(c) => i += 1,
            '*' | '_' => {
                // A lone mark touching a word is emphasis; one standing alone ("2 * 3") is text.
                let word_side = |o: Option<char>| o.is_some_and(char::is_alphanumeric);
                let before = i.checked_sub(1).and_then(|p| chars.get(p)).copied();
                if !(word_side(before) || word_side(next))
                    || (c == '_' && word_side(before) && word_side(next))
                {
                    cleaned.push(c);
                }
            }
            _ => cleaned.push(c),
        }
        i += 1;
    }
    let one: String = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > max {
        let cut: String = one.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    } else {
        one
    }
}

/// `[text](target)` and `![alt](target)` read as their text.
fn unlink(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        let start = if chars[i] == '!' && chars.get(i + 1) == Some(&'[') {
            i + 1
        } else {
            i
        };
        if chars[start] == '[' {
            if let Some(close) = chars[start..]
                .iter()
                .position(|c| *c == ']')
                .map(|p| start + p)
            {
                if chars.get(close + 1) == Some(&'(') {
                    if let Some(end) = chars[close..]
                        .iter()
                        .position(|c| *c == ')')
                        .map(|p| close + p)
                    {
                        out.extend(&chars[start + 1..close]);
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn chat_title(title: &str, blank: &str) -> String {
    let t = title.trim();
    if t.is_empty() {
        blank.to_owned()
    } else {
        t.to_owned()
    }
}

/// The toast for a chat that finished a turn.
pub fn finished_toast(c: &Completion) -> ToastSpec {
    ToastSpec {
        tag: tag_for(&c.chat_id),
        group: GROUP.into(),
        title: chat_title(&c.title, "Task finished"),
        body: plain(&c.preview, BODY_MAX),
        actions: vec![
            ToastAction {
                label: "Continue".into(),
                arguments: format!("continue:{}", c.chat_id),
            },
            ToastAction {
                label: "Open".into(),
                arguments: format!("open:{}", c.chat_id),
            },
        ],
        reply_box: Some(ReplyBox {
            placeholder: "Reply…".into(),
            send_label: "Send".into(),
            arguments: format!("reply:{}", c.chat_id),
        }),
        launch: format!("open:{}", c.chat_id),
    }
}

/// The toast for a chat that needs the user: Open and nothing else.
pub fn needs_you_toast(r: &AttentionRow) -> ToastSpec {
    ToastSpec {
        tag: tag_for(&r.chat_id),
        group: GROUP.into(),
        title: format!("{} needs you", chat_title(&r.title, "A chat")),
        body: plain(&r.reason, BODY_MAX),
        actions: vec![ToastAction {
            label: "Open".into(),
            arguments: format!("open:{}", r.chat_id),
        }],
        reply_box: None,
        launch: format!("open:{}", r.chat_id),
    }
}

/// What a click asks Helm to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Click {
    /// Show Helm and open this chat.
    Open(String),
    /// Send `text` to this chat as the user's own message from a notification.
    Send { chat_id: String, text: String },
    /// Nothing to do: an unknown button, an empty reply, a chat id that is not one.
    Ignore,
}

/// Read a click. The buttons carry `open:<chat>`, `continue:<chat>` and `reply:<chat>`; a reply
/// also carries what was typed.
pub fn parse_click(a: &Activation) -> Click {
    let Some((verb, chat)) = a.arguments.split_once(':') else {
        return Click::Ignore;
    };
    if chat.is_empty() || chat.len() > 128 || chat.chars().any(char::is_control) {
        return Click::Ignore;
    }
    let chat_id = chat.to_owned();
    match verb {
        "open" => Click::Open(chat_id),
        "continue" => Click::Send {
            chat_id,
            text: CONTINUE_TEXT.into(),
        },
        "reply" => match a.reply_text.as_deref().map(str::trim) {
            Some(t) if !t.is_empty() && t.len() <= REPLY_MAX_BYTES => Click::Send {
                chat_id,
                text: t.to_owned(),
            },
            _ => Click::Ignore,
        },
        _ => Click::Ignore,
    }
}

/// A fresh id for the message a click sends.
pub fn submission_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn blocking(kind: AttentionKind) -> bool {
    matches!(
        kind,
        AttentionKind::Approval | AttentionKind::Question | AttentionKind::Plan
    )
}

/// Which chats have just started needing the user, and which toast of that kind to take back.
#[derive(Default)]
pub struct NeedsTracker {
    seen: HashSet<(String, String)>,
    primed: bool,
    posted: HashSet<String>,
}

impl NeedsTracker {
    /// The blocking requests in this summary that the last one did not hold. The first summary
    /// (launch) only primes the tracker: requests already waiting then raise nothing.
    pub fn fresh(&mut self, s: &AttentionSummary) -> Vec<AttentionRow> {
        let now: Vec<&AttentionRow> = s.needs_you.iter().filter(|r| blocking(r.kind)).collect();
        let keys: HashSet<(String, String)> = now
            .iter()
            .map(|r| (r.chat_id.clone(), format!("{:?}", r.kind)))
            .collect();
        let fresh = if self.primed {
            now.iter()
                .filter(|r| {
                    !self
                        .seen
                        .contains(&(r.chat_id.clone(), format!("{:?}", r.kind)))
                })
                .map(|r| (*r).clone())
                .collect()
        } else {
            Vec::new()
        };
        self.seen = keys;
        self.primed = true;
        fresh
    }

    /// A needs-you toast went up for this chat.
    pub fn posted(&mut self, chat_id: &str) {
        self.posted.insert(chat_id.to_owned());
    }

    /// Chats whose needs-you toast should come down: they no longer need the user.
    pub fn resolved(&mut self, s: &AttentionSummary) -> Vec<String> {
        let waiting: HashSet<&str> = s
            .needs_you
            .iter()
            .filter(|r| blocking(r.kind))
            .map(|r| r.chat_id.as_str())
            .collect();
        let mut gone: Vec<String> = self
            .posted
            .iter()
            .filter(|c| !waiting.contains(c.as_str()))
            .cloned()
            .collect();
        gone.sort();
        for c in &gone {
            self.posted.remove(c);
        }
        gone
    }

    /// Forget every posted toast (the setting was turned off); returns their chats.
    pub fn withdraw_all(&mut self) -> Vec<String> {
        let mut all: Vec<String> = self.posted.drain().collect();
        all.sort();
        all
    }
}

/// The state dot for the tray icon: red when anything needs the user, amber while something runs,
/// none when idle. Muted chats are not in the summary's needs-you and flagged groups, so they never
/// raise it; keep-awake's busy count includes them, but a quiet chat running is not news.
pub fn tray_dot(s: &AttentionSummary) -> TrayDot {
    if !s.needs_you.is_empty() || !s.flagged.is_empty() {
        TrayDot::Red
    } else if !s.running.is_empty() {
        TrayDot::Amber
    } else {
        TrayDot::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(chat: &str, kind: AttentionKind, reason: &str) -> AttentionRow {
        AttentionRow {
            chat_id: chat.into(),
            title: "Fix the build".into(),
            project: "P".into(),
            kind,
            reason: reason.into(),
        }
    }

    fn summary(needs: &[AttentionRow], running: usize, flagged: usize) -> AttentionSummary {
        AttentionSummary {
            badge: 0,
            busy: running as u32,
            needs_you: needs.to_vec(),
            running: (0..running)
                .map(|i| row(&format!("r{i}"), AttentionKind::Running, ""))
                .collect(),
            unread: vec![],
            stalled: vec![],
            flagged: (0..flagged)
                .map(|i| row(&format!("f{i}"), AttentionKind::Triage, ""))
                .collect(),
            muted: vec![],
            triage: vec![],
            revision: 1,
        }
    }

    #[test]
    fn replies_read_as_plain_text_of_at_most_160_characters() {
        for (md, want) in [
            (
                "**Done.** See [the log](http://x/y) and `run`.",
                "Done. See the log and run.",
            ),
            ("## Plan\n- one\n- two\n1. three", "Plan one two three"),
            ("```rust\nlet x = 1;\n```\nafter", "let x = 1; after"),
            ("![a chart](http://x/c.png) is ready", "a chart is ready"),
            ("> quoted *text* here", "quoted text here"),
            (
                "2 * 3 = 6 and snake_case stays",
                "2 * 3 = 6 and snake_case stays",
            ),
            ("  spaced \n\n out  ", "spaced out"),
            ("", ""),
        ] {
            assert_eq!(plain(md, BODY_MAX), want, "{md:?}");
        }
        let long = plain(&"word ".repeat(100), BODY_MAX);
        assert_eq!(long.chars().count(), BODY_MAX);
        assert!(long.ends_with('…'));
        assert_eq!(plain("exactly", 7), "exactly");
    }

    #[test]
    fn a_finished_chat_gets_reply_continue_and_open_and_a_needing_one_gets_open_only() {
        let c = Completion {
            id: "k".into(),
            chat_id: "c1".into(),
            title: "  ".into(),
            project: "P".into(),
            preview: "**All** done".into(),
            completed_at: 1,
            read: false,
            dismissed: false,
        };
        let t = finished_toast(&c);
        assert_eq!(
            (t.title.as_str(), t.body.as_str()),
            ("Task finished", "All done")
        );
        assert_eq!(t.tag, tag_for("c1"));
        let labels: Vec<&str> = t.actions.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(labels, ["Continue", "Open"]);
        assert_eq!(t.reply_box.as_ref().unwrap().arguments, "reply:c1");
        assert_eq!(t.launch, "open:c1");

        for kind in [
            AttentionKind::Approval,
            AttentionKind::Question,
            AttentionKind::Plan,
        ] {
            let n = needs_you_toast(&row("c2", kind, "Waiting for your approval"));
            assert_eq!(n.title, "Fix the build needs you");
            assert_eq!(n.body, "Waiting for your approval");
            assert_eq!(n.actions.len(), 1);
            assert_eq!(n.actions[0].arguments, "open:c2");
            assert!(
                n.reply_box.is_none(),
                "{kind:?}: nothing is answered from a toast"
            );
        }
    }

    #[test]
    fn clicks_open_continue_and_reply_and_nothing_else() {
        let click = |args: &str, text: Option<&str>| {
            parse_click(&Activation {
                arguments: args.into(),
                reply_text: text.map(Into::into),
            })
        };
        let send = |t: &str| Click::Send {
            chat_id: "c1".into(),
            text: t.into(),
        };
        assert_eq!(click("open:c1", None), Click::Open("c1".into()));
        assert_eq!(click("continue:c1", None), send("Continue"));
        assert_eq!(click("reply:c1", Some("  yes, go  ")), send("yes, go"));
        let big = "x".repeat(REPLY_MAX_BYTES + 1);
        for (args, text) in [
            ("reply:c1", None),
            ("reply:c1", Some("   ")),
            ("reply:c1", Some(big.as_str())),
            ("approve:c1", None),
            ("open:", None),
            ("open", None),
            ("", None),
            ("open:c1\n", None),
        ] {
            assert_eq!(click(args, text), Click::Ignore, "{args:?}");
        }
        assert!(submission_id().len() >= 32);
    }

    #[test]
    fn new_requests_toast_once_and_launch_requests_do_not() {
        let mut t = NeedsTracker::default();
        let a = row("c1", AttentionKind::Approval, "");
        let q = row("c2", AttentionKind::Question, "");
        assert!(
            t.fresh(&summary(std::slice::from_ref(&a), 0, 0)).is_empty(),
            "already waiting at launch"
        );
        assert!(t.fresh(&summary(std::slice::from_ref(&a), 0, 0)).is_empty());
        let fresh = t.fresh(&summary(&[a.clone(), q.clone()], 0, 0));
        assert_eq!(
            fresh.iter().map(|r| r.chat_id.as_str()).collect::<Vec<_>>(),
            ["c2"]
        );
        // A different kind of request in the same chat is new again; a running row is not a request.
        let plan = row("c1", AttentionKind::Plan, "");
        let fresh = t.fresh(&summary(&[plan, q], 0, 0));
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].kind, AttentionKind::Plan);
        // Toasts come down when the request is answered.
        t.posted("c1");
        t.posted("c2");
        assert_eq!(
            t.resolved(&summary(&[row("c2", AttentionKind::Question, "")], 0, 0)),
            ["c1"]
        );
        assert!(t
            .resolved(&summary(&[row("c2", AttentionKind::Question, "")], 0, 0))
            .is_empty());
        assert_eq!(t.withdraw_all(), ["c2"]);
    }

    #[test]
    fn the_tray_dot_is_red_for_a_request_amber_for_work_and_none_when_idle() {
        let ask = row("c", AttentionKind::Approval, "");
        for (s, want) in [
            (summary(&[], 0, 0), TrayDot::None),
            (summary(&[], 2, 0), TrayDot::Amber),
            (summary(std::slice::from_ref(&ask), 0, 0), TrayDot::Red),
            (summary(&[ask], 3, 0), TrayDot::Red),
            (summary(&[], 1, 1), TrayDot::Red),
        ] {
            assert_eq!(tray_dot(&s), want);
        }
    }
}
