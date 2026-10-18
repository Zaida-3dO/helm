//! The addendum sections Helm registers itself.
//!
//! - `verbosity` (order 10): how long replies should be, from the user-only `responseVerbosity`
//!   (low by default; high adds nothing).
//! - `links` (order 20): the link teaching (vault notes and `file:line`, the panel crate's
//!   fragment), adopted unchanged through [`AdoptedSection`] so it is taught once.
//! - `tools.guidelines` (order 30): usage guidelines registered with host tools.

use crate::addendum::AddendumSection;
use crate::registry::HostToolRegistry;
use helm_core::db::Database;
use helm_workers::contrib::{LaunchContributor, LaunchCtx};
use serde_json::Value;
use std::sync::Arc;

pub const VERBOSITY_LOW: &str = "Reply length: keep answers short. Lead with the result in one or two sentences, use a short list only when it helps, and leave out preamble, restating the question and closing summaries. Give more detail only when the user asks for it.";
pub const VERBOSITY_MEDIUM: &str = "Reply length: be reasonably concise. Lead with the result, then the detail that matters; leave out preamble and recaps.";

/// The stored verbosity, `low` when unset or unknown.
pub fn verbosity(db: &Database) -> String {
    db.snapshot(|tx| Ok::<_, rusqlite::Error>(helm_core::prefs::read(tx, "responseVerbosity")))
        .ok()
        .flatten()
        .and_then(|v| match v {
            Value::String(s) if matches!(s.as_str(), "low" | "medium" | "high") => Some(s),
            _ => None,
        })
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
    fn text(&self, _ctx: &LaunchCtx<'_>) -> Option<String> {
        match verbosity(&self.db).as_str() {
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

/// The host tools' usage guidelines, one paragraph per tool that has them.
pub struct GuidelinesSection {
    pub registry: Arc<HostToolRegistry>,
}

impl AddendumSection for GuidelinesSection {
    fn id(&self) -> &'static str {
        "tools.guidelines"
    }
    fn order(&self) -> i32 {
        30
    }
    fn text(&self, _ctx: &LaunchCtx<'_>) -> Option<String> {
        let lines: Vec<String> = self
            .registry
            .names()
            .filter_map(|n| {
                let g = self.registry.get(n)?.spec.guidelines.clone()?;
                Some(format!("{n}: {g}"))
            })
            .collect();
        (!lines.is_empty()).then(|| format!("Helm's tools:\n{}", lines.join("\n")))
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
