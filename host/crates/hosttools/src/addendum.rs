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

/// The longest project note the prompt carries, in characters (the note's own limit).
pub const NOTE_PROMPT_CHARS: usize = 4_000;

/// The `knowledge` section's text: the folder's project note, when there is one, and one line
/// pointing at the shared memories, when they are connected. `None` when there is nothing to say,
/// so a chat with neither gets a launch identical to one without the section.
///
/// `hub_server` is the id of the connected memory server, if any.
pub fn knowledge_text(note: Option<&str>, hub_server: Option<&str>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(note) = note.map(str::trim).filter(|n| !n.is_empty()) {
        let note: String = note.chars().take(NOTE_PROMPT_CHARS).collect();
        parts.push(format!(
            "Project note for the folder this chat runs in (shared by every chat working there; \
             keep it current with app_update_project_context):\n{note}"
        ));
    }
    if let Some(id) = hub_server.map(str::trim).filter(|i| !i.is_empty()) {
        parts.push(format!(
            "Shared memories are connected (MCP server {id}). Before non-trivial work, search \
             them with memory_search for lessons and conventions that apply."
        ));
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// Looks up a chat's project note. Returns the text, or `None` when the chat has no note.
pub type NoteLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
/// Returns the id of the connected shared-memory server, if one is on.
pub type HubLookup = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// The `knowledge` section (order 40): the folder's project note and the shared-memories line.
/// The lookups are supplied by the app, which owns the data, so this crate stays free of them.
pub struct KnowledgeSection {
    pub note: NoteLookup,
    pub hub: HubLookup,
}

impl AddendumSection for KnowledgeSection {
    fn id(&self) -> &'static str {
        "knowledge"
    }
    fn order(&self) -> i32 {
        40
    }
    fn text(&self, ctx: &LaunchCtx<'_>) -> Option<String> {
        knowledge_text((self.note)(ctx.chat_id).as_deref(), (self.hub)().as_deref())
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

    #[test]
    fn knowledge_section_says_only_what_applies() {
        let long = "x".repeat(NOTE_PROMPT_CHARS + 50);
        // (note, hub server, expected pieces in order, pieces that must be absent)
        let cases: Vec<(Option<&str>, Option<&str>, Vec<&str>, Vec<&str>)> = vec![
            (None, None, vec![], vec![]),
            (Some("  \n "), Some(" "), vec![], vec![]),
            (
                Some("Use the staging db."),
                None,
                vec!["Project note", "Use the staging db."],
                vec!["memory_search"],
            ),
            (
                None,
                Some("helm_hub"),
                vec!["helm_hub", "memory_search"],
                vec!["Project note"],
            ),
            (
                Some("Note text"),
                Some("hub"),
                vec!["Project note", "Note text", "memory_search"],
                vec![],
            ),
        ];
        for (note, hub, has, lacks) in cases {
            let got = knowledge_text(note, hub);
            if has.is_empty() {
                assert_eq!(got, None, "{note:?} {hub:?}");
                continue;
            }
            let got = got.unwrap_or_else(|| panic!("{note:?} {hub:?} should say something"));
            let mut at = 0;
            for h in has {
                let i = got[at..]
                    .find(h)
                    .unwrap_or_else(|| panic!("{h:?} in {got:?}"));
                at += i;
            }
            for l in lacks {
                assert!(!got.contains(l), "{l:?} in {got:?}");
            }
        }
        // A note over the limit is cut, not dropped.
        let got = knowledge_text(Some(&long), None).unwrap();
        assert_eq!(got.matches('x').count(), NOTE_PROMPT_CHARS);

        // As a section: the lookups get the chat id, and it composes after earlier sections.
        let mut a = Addendum::new();
        a.register_section(Arc::new(KnowledgeSection {
            note: Arc::new(|chat: &str| (chat == "c1").then(|| "Note for c1".to_owned())),
            hub: Arc::new(|| None),
        }))
        .register_section(Arc::new(StaticSection {
            id: "first",
            order: 1,
            text: "First.",
        }));
        assert_eq!(a.ids(), ["first", "knowledge"]);
        let text = a.compose(&ctx()).unwrap();
        assert!(text.starts_with("First.\n\nProject note"), "{text}");
        assert!(text.ends_with("Note for c1"), "{text}");
        let other = LaunchCtx {
            chat_id: "c2",
            cwd: "C:/work",
        };
        assert_eq!(a.compose(&other).as_deref(), Some("First."));
    }
}
