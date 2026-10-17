//! The prompt addendum layer: the one owner of the text Helm appends to an
//! agent's system prompt.
//!
//! Crates register [`AddendumSection`]s; [`Addendum`] is a single `LaunchContributor` that orders
//! them and joins their text into one `appendSystemPrompt` fragment. Sections are plain,
//! harness-neutral text (Claude Code gets `--append-system-prompt`; Codex gets its own route later).
//! With no section applying, the contributor adds nothing, so the launch is byte-identical to
//! one without the layer.

use helm_workers::contrib::{LaunchContributor, LaunchCtx, LaunchExtras};
use std::sync::Arc;

/// One section of the addendum. Sections capture whatever they read (the database, a vault list)
/// when they are built.
pub trait AddendumSection: Send + Sync {
    /// Stable id, unique across sections: `verbosity`, `links`, `tools.guidelines`, `itemlinks`...
    fn id(&self) -> &'static str;
    /// Lower comes first; ties keep registration order.
    fn order(&self) -> i32;
    /// The section's text for this launch, or `None` when it does not apply.
    fn text(&self, ctx: &LaunchCtx<'_>) -> Option<String>;
}

/// The registered sections.
#[derive(Clone, Default)]
pub struct Addendum {
    sections: Vec<Arc<dyn AddendumSection>>,
}

impl Addendum {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a section. A second section with the same id panics (a wiring bug).
    pub fn register_section(&mut self, s: Arc<dyn AddendumSection>) -> &mut Self {
        assert!(
            self.sections.iter().all(|x| x.id() != s.id()),
            "addendum section registered twice: {}",
            s.id()
        );
        self.sections.push(s);
        self
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.ordered().iter().map(|s| s.id()).collect()
    }

    fn ordered(&self) -> Vec<&Arc<dyn AddendumSection>> {
        let mut v: Vec<_> = self.sections.iter().collect();
        v.sort_by_key(|s| s.order()); // stable: ties keep registration order
        v
    }

    /// The composed text, or `None` when no section applies.
    pub fn compose(&self, ctx: &LaunchCtx<'_>) -> Option<String> {
        let parts: Vec<String> = self
            .ordered()
            .into_iter()
            .filter_map(|s| s.text(ctx))
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect();
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }
}

impl LaunchContributor for Addendum {
    fn contribute(&self, ctx: &LaunchCtx<'_>) -> LaunchExtras {
        LaunchExtras {
            system_prompt: self.compose(ctx).into_iter().collect(),
            ..Default::default()
        }
    }
}

/// A fixed-text section, for text that does not depend on the launch.
pub struct StaticSection {
    pub id: &'static str,
    pub order: i32,
    pub text: &'static str,
}

impl AddendumSection for StaticSection {
    fn id(&self) -> &'static str {
        self.id
    }
    fn order(&self) -> i32 {
        self.order
    }
    fn text(&self, _ctx: &LaunchCtx<'_>) -> Option<String> {
        Some(self.text.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> LaunchCtx<'static> {
        LaunchCtx {
            chat_id: "c1",
            cwd: "C:/work",
        }
    }

    struct OnlyIn(&'static str);
    impl AddendumSection for OnlyIn {
        fn id(&self) -> &'static str {
            "only"
        }
        fn order(&self) -> i32 {
            5
        }
        fn text(&self, ctx: &LaunchCtx<'_>) -> Option<String> {
            (ctx.cwd == self.0).then(|| "only here".to_owned())
        }
    }

    #[test]
    fn an_empty_layer_contributes_nothing() {
        let a = Addendum::new();
        assert!(a.contribute(&ctx()).is_empty());
        let mut b = Addendum::new();
        b.register_section(Arc::new(OnlyIn("elsewhere")));
        assert!(
            b.contribute(&ctx()).is_empty(),
            "a section that does not apply adds nothing"
        );
    }

    #[test]
    fn sections_compose_in_order_as_one_fragment() {
        let mut a = Addendum::new();
        a.register_section(Arc::new(StaticSection {
            id: "late",
            order: 20,
            text: "Second.",
        }))
        .register_section(Arc::new(OnlyIn("C:/work")))
        .register_section(Arc::new(StaticSection {
            id: "early",
            order: 1,
            text: "  First.\n",
        }));
        assert_eq!(a.ids(), ["early", "only", "late"]);
        let e = a.contribute(&ctx());
        assert_eq!(e.system_prompt, ["First.\n\nonly here\n\nSecond."]);
        assert!(e.env.is_empty() && e.path_prepend.is_empty());
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn duplicate_ids_panic() {
        let mut a = Addendum::new();
        let s = || {
            Arc::new(StaticSection {
                id: "x",
                order: 0,
                text: "t",
            })
        };
        a.register_section(s()).register_section(s());
    }
}
