//! The addendum sections Helm registers itself.
//!
//! - `verbosity` (order 10): how long replies should be: the chat's own level, else its profile's
//!   choice for new chats, else the install-wide `responseVerbosity` (low by default; high adds
//!   nothing).
//! - `prose` (order 12): the work shows as collapsible records, so visible prose should not narrate
//!   it; always on.
//! - `progress` (order 13): the progress heartbeat is a tool call, not prose; always on.
//! - `links` (order 20): the link teaching (vault notes and `file:line`, the panel crate's
//!   fragment), adopted unchanged through [`AdoptedSection`] so it is taught once.

use crate::addendum::AddendumSection;
use helm_core::db::Database;
use helm_workers::contrib::{LaunchContributor, LaunchCtx};
use serde_json::Value;
use std::sync::Arc;

pub const VERBOSITY_LOW: &str = "Reply length: keep answers short. Lead with the result in one or two sentences, use a short list only when it helps, and leave out preamble, restating the question and closing summaries. Give more detail only when the user asks for it.";
pub const VERBOSITY_MEDIUM: &str = "Reply length: be reasonably concise. Lead with the result, then the detail that matters; leave out preamble and recaps.";

/// The `prose` section (order 12): the work itself is shown to the user as collapsible records,
/// so the agent's visible prose should not narrate it. One line, harness-neutral, always on.
pub const PROSE_ADDENDUM: &str = "Prose: your reasoning and tool activity already reach the user as collapsible records. Keep user-visible text to results and the few words that carry them: do not narrate each tool call, restate what a call returned, or think out loud in the reply.";

/// The `progress` section (order 13): where a work-in-progress line belongs. It rides every
/// launch (a Claude chat and a chat on a local model server both get the addendum), so the
/// teaching is the same for both. One heartbeat tool, one line of rules about it.
pub const PROGRESS_ADDENDUM: &str = "Progress: to say where you are while you work, call app_progress with one short line; it shows as a single live line for the turn, replaced by the next one and never kept as a message, so the reader may not see it at all. Never make it the only place a fact appears, and write the final reply to stand on its own, as if the reader saw no progress line. Real messages to the user stay milestones only: raise the bar for between-steps prose.";

/// The reply length a chat follows when it has none of its own: the one its profile chose for new
/// chats (the profile acting now, for a chat with no owner), else the install-wide one, `low`
/// when unset or unknown.
pub fn verbosity(db: &Database, chat_id: &str) -> String {
    let valid = |v: &Value| match v {
        Value::String(s) if matches!(s.as_str(), "low" | "medium" | "high") => Some(s.clone()),
        _ => None,
    };
    db.snapshot(|tx| {
        let owner = tx
            .query_row(
                "SELECT NULLIF(profile_id, '') FROM chats WHERE id = ?1",
                [chat_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten()
            .or_else(|| helm_core::prefs::acting_profile(tx));
        Ok::<_, rusqlite::Error>(
            helm_core::prefs::profile_default(tx, owner.as_deref(), "responseVerbosity")
                .as_ref()
                .and_then(valid)
                .or_else(|| {
                    helm_core::prefs::read(tx, "responseVerbosity")
                        .as_ref()
                        .and_then(valid)
                }),
        )
    })
    .ok()
    .flatten()
    .unwrap_or_else(|| "low".into())
}

pub struct VerbositySection {
    pub db: Arc<Database>,
}

impl AddendumSection for VerbositySection {
    fn id(&self) -> &'static str {
        "verbosity"
    }
    fn order(&self) -> i32 {
        10
    }
    fn text(&self, ctx: &LaunchCtx<'_>) -> Option<String> {
        // The chat's own level when it has one, else the new-chat default.
        match crate::chat_verbosity::level_for(&self.db, ctx.chat_id).as_str() {
            "low" => Some(VERBOSITY_LOW.into()),
            "medium" => Some(VERBOSITY_MEDIUM.into()),
            _ => None,
        }
    }
}

/// An existing fragment contributor adopted as a section: its system-prompt text, nothing else.
pub struct AdoptedSection {
    pub id: &'static str,
    pub order: i32,
    pub inner: Arc<dyn LaunchContributor>,
}

impl AddendumSection for AdoptedSection {
    fn id(&self) -> &'static str {
        self.id
    }
    fn order(&self) -> i32 {
        self.order
    }
    fn text(&self, ctx: &LaunchCtx<'_>) -> Option<String> {
        let parts = self.inner.contribute(ctx).system_prompt;
        let t = parts.join("\n\n");
        (!t.trim().is_empty()).then_some(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addendum::Addendum;
    use helm_core::db::MigrationRegistry;
    use helm_workers::contrib::LaunchExtras;

    struct Frag;
    impl LaunchContributor for Frag {
        fn contribute(&self, _: &LaunchCtx<'_>) -> LaunchExtras {
            LaunchExtras {
                system_prompt: vec!["Links open in Helm's side panel.".into()],
                ..Default::default()
            }
        }
    }

    #[test]
    fn verbosity_defaults_low_and_high_adds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db =
            Arc::new(Database::open(&dir.path().join("t.db"), &MigrationRegistry::new()).unwrap());
        let mut a = Addendum::new();
        a.register_section(Arc::new(VerbositySection { db: db.clone() }))
            .register_section(Arc::new(AdoptedSection {
                id: "links",
                order: 20,
                inner: Arc::new(Frag),
            }));
        let ctx = LaunchCtx {
            chat_id: "c",
            cwd: "C:/w",
        };
        let low = a.compose(&ctx).unwrap();
        assert!(low.starts_with(VERBOSITY_LOW), "low is the default");
        assert!(
            low.ends_with("Links open in Helm's side panel."),
            "the link teaching is adopted once"
        );
        assert_eq!(low.matches("side panel").count(), 1);
        let set = |v: &str| {
            db.write(|tx| {
                helm_core::prefs::set_raw(tx, "responseVerbosity", &serde_json::json!(v))
                    .map_err(|_| rusqlite::Error::InvalidQuery)
            })
            .unwrap()
        };
        set("medium");
        assert!(a.compose(&ctx).unwrap().starts_with(VERBOSITY_MEDIUM));
        set("high");
        assert_eq!(a.compose(&ctx).unwrap(), "Links open in Helm's side panel.");
        set("bogus");
        assert!(a.compose(&ctx).unwrap().starts_with(VERBOSITY_LOW));
    }
}
