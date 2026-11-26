//! The `rule` and `skill` namespaces of `app_help`.
//!
//! The prompt lists a long rule by one line and a skill by name; the whole text is read here, when
//! it applies, straight from the layers (never a copy, so an edit shows at once). Both answer for
//! the chat that asks: its person's layer and, for skills, its project's linked folders.

use crate::launch::{rule_groups, shown_at_launch, ChatKnowledge, ChatLookup};
use crate::plugins;
use crate::triggers::Facts;
use helm_hosttools::help::{HelpProvider, HelpProviders, MAX_SUMMARY_CHARS};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Add the `rule` and `skill` namespaces.
pub fn register(providers: &HelpProviders, profile_dir: PathBuf, lookup: Arc<dyn ChatLookup>) {
    providers.register(Arc::new(RuleHelp {
        profile_dir: profile_dir.clone(),
        lookup: lookup.clone(),
    }));
    providers.register(Arc::new(SkillHelp {
        profile_dir,
        lookup,
    }));
}

struct RuleHelp {
    profile_dir: PathBuf,
    lookup: Arc<dyn ChatLookup>,
}

impl HelpProvider for RuleHelp {
    fn namespace(&self) -> &'static str {
        "rule"
    }

    fn about(&self) -> &'static str {
        "The rules that apply to you, in full (the prompt lists the long ones by a line)"
    }

    /// The index lists only the rules this chat was actually shown at launch (whole or on a
    /// line); one its trigger keeps back stays out of the list. `topic` still reads it.
    fn index(&self, chat_id: &str) -> Vec<(String, String)> {
        let who = self.lookup.of_chat(chat_id);
        let facts = Facts::of(&who, None);
        rule_groups(&self.profile_dir, &who)
            .into_iter()
            .flat_map(|(_, rules)| rules)
            .filter(|r| shown_at_launch(r, &facts))
            .map(|r| {
                (
                    r.topic.clone(),
                    format!("{}: {}", r.entry.title, r.summary()),
                )
            })
            .collect()
    }

    fn topic(&self, chat_id: &str, name: &str) -> Option<String> {
        let who = self.lookup.of_chat(chat_id);
        rule_groups(&self.profile_dir, &who)
            .into_iter()
            .flat_map(|(_, rules)| rules)
            .find(|r| r.topic == name)
            .map(|r| format!("{}\n\n{}", r.entry.title, r.body))
    }
}

struct SkillHelp {
    profile_dir: PathBuf,
    lookup: Arc<dyn ChatLookup>,
}

impl SkillHelp {
    fn skills(&self, chat_id: &str) -> Vec<plugins::PluginSkill> {
        let who: ChatKnowledge = self.lookup.of_chat(chat_id);
        plugins::collect(&self.profile_dir, &who).skills
    }
}

/// A skill file's text without its front matter.
fn without_front(text: &str) -> String {
    let t = text.trim_start_matches('\u{feff}').trim_start();
    match t.strip_prefix("---") {
        Some(rest) => match rest.split_once("\n---") {
            Some((_, body)) => body.trim_start_matches(['\r', '\n']).trim().to_owned(),
            None => t.trim().to_owned(),
        },
        None => t.trim().to_owned(),
    }
}

/// The section files beside a skill's `SKILL.md`: the `.md` files of its own folder.
fn sections(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let stem = name.strip_suffix(".md")?;
            (stem != "SKILL"
                && !stem.starts_with('.')
                && stem
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                && e.file_type().is_ok_and(|t| t.is_file()))
            .then(|| stem.to_owned())
        })
        .collect();
    v.sort();
    v
}

impl HelpProvider for SkillHelp {
    fn namespace(&self) -> &'static str {
        "skill"
    }

    fn about(&self) -> &'static str {
        "Your skills, in full, and the sections of the long ones"
    }

    fn index(&self, chat_id: &str) -> Vec<(String, String)> {
        self.skills(chat_id)
            .into_iter()
            .map(|s| {
                let d: String = s.description.chars().take(MAX_SUMMARY_CHARS).collect();
                (s.name, d)
            })
            .collect()
    }

    fn topic(&self, chat_id: &str, name: &str) -> Option<String> {
        let (skill, section) = name.split_once('/').unwrap_or((name, ""));
        let s = self.skills(chat_id).into_iter().find(|s| s.name == skill)?;
        let secs = sections(&s.dir);
        if section.is_empty() {
            let text = fs::read_to_string(s.dir.join("SKILL.md")).ok()?;
            let mut out = without_front(&text);
            if !secs.is_empty() {
                let names: Vec<String> =
                    secs.iter().map(|x| format!("skill/{skill}/{x}")).collect();
                out.push_str(&format!("\n\nSections: {}", names.join(", ")));
            }
            return Some(out);
        }
        secs.iter().any(|x| x == section).then_some(())?;
        let text = fs::read_to_string(s.dir.join(format!("{section}.md"))).ok()?;
        Some(without_front(&text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::ProfileRef;
    use crate::rules;

    struct Fixed(ChatKnowledge);
    impl ChatLookup for Fixed {
        fn of_chat(&self, _: &str) -> ChatKnowledge {
            self.0.clone()
        }
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn long_rules_are_listed_by_a_line_and_read_in_full_and_a_header_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::layers::Layer::Everyone.root(dir.path());
        write(
            &root.join("rules/short.md"),
            "# Short one\n\nDo the small thing.\n",
        );
        let long = format!(
            "# Long one\n\nStart here. {}",
            "More words follow. ".repeat(60)
        );
        write(&root.join("rules/long.md"), &long);
        write(
            &root.join("rules/pinned.md"),
            &format!(
                "---\nalways: true\nsummary: Never skipped.\n---\n# Pinned\n\n{}",
                "Body text goes on. ".repeat(60)
            ),
        );
        // A chat with no folder and no orchestrator is a household chat, so this coding-only
        // rule is kept back: out of the prompt and the index, still readable by topic.
        write(
            &root.join("rules/work-only.md"),
            "---\nkinds: coding\n---\n# Work only\n\nRead the diff first.\n",
        );
        let who = ChatKnowledge {
            profile: Some(ProfileRef {
                id: "p1".into(),
                name: "Pat".into(),
                is_home: true,
            }),
            project_id: None,
            ..ChatKnowledge::default()
        };
        let text = crate::launch::rules_text(dir.path(), &who, crate::launch::RULES_PROMPT_CAP);
        assert!(
            text.contains("### Short one\nDo the small thing."),
            "a short rule goes in whole"
        );
        assert!(
            text.contains("### Pinned\nBody text goes on."),
            "always: true keeps a long rule whole"
        );
        assert!(!text.contains("### Long one"), "a long rule is a line");
        assert!(
            text.contains("- Long one: Start here. More words follow. More words follow. (app_help rule/long)"),
            "{text}"
        );
        // The coding-only trigger hides it from this household chat's prompt…
        assert!(
            !text.contains("Work only") && text.contains(crate::launch::HELD_BACK),
            "{text}"
        );
        // The same rules answer app_help, whole.
        let help = RuleHelp {
            profile_dir: dir.path().to_owned(),
            lookup: Arc::new(Fixed(who.clone())),
        };
        let index = help.index("c");
        assert!(index
            .iter()
            .any(|(n, s)| n == "long" && s.starts_with("Long one: Start here.")));
        // …and from the index, yet it stays readable by topic.
        assert!(!index.iter().any(|(n, _)| n == "work-only"), "{index:?}");
        assert!(help
            .topic("c", "work-only")
            .unwrap()
            .contains("Read the diff first."));
        assert!(help
            .topic("c", "long")
            .unwrap()
            .contains("More words follow."));
        assert!(help.topic("c", "nope").is_none());
        // A rewrite keeps the header; passing `always` explicitly sets or clears just that flag.
        let e = rules::write(
            &root,
            Some("rules/pinned.md"),
            "Pinned",
            "New body.",
            None,
            None,
            None,
            4096,
        )
        .unwrap();
        let (front, _) =
            rules::split_front(&fs::read_to_string(root.join("rules/pinned.md")).unwrap());
        assert!(
            front.always && front.summary.as_deref() == Some("Never skipped."),
            "{e:?}"
        );
        // A rule without a header gains one only when asked for, and loses it again.
        rules::write(
            &root,
            Some("rules/long.md"),
            "Long one",
            "Still a line rule.",
            None,
            None,
            None,
            4096,
        )
        .unwrap();
        assert!(
            !fs::read_to_string(root.join("rules/long.md"))
                .unwrap()
                .starts_with("---"),
            "a rewrite without `always` gains no header"
        );
        rules::write(
            &root,
            Some("rules/long.md"),
            "Long one",
            "Carried whole now.",
            None,
            Some(true),
            None,
            4096,
        )
        .unwrap();
        assert!(
            fs::read_to_string(root.join("rules/long.md"))
                .unwrap()
                .starts_with("---\nalways: true\n---\n"),
            "always: Some(true) writes the header"
        );
        rules::write(
            &root,
            Some("rules/long.md"),
            "Long one",
            "Back to a line.",
            None,
            Some(false),
            None,
            4096,
        )
        .unwrap();
        assert!(
            !fs::read_to_string(root.join("rules/long.md"))
                .unwrap()
                .contains("always"),
            "always: Some(false) clears it"
        );
        assert_eq!(
            rules::title_of(
                &fs::read_to_string(root.join("rules/pinned.md")).unwrap(),
                "x"
            ),
            "Pinned"
        );
    }

    #[test]
    fn a_skill_is_read_whole_with_its_sections() {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::layers::Layer::Everyone.root(dir.path());
        write(
            &root.join("skills/deploy/SKILL.md"),
            "---\nname: deploy\ndescription: Ship it.\n---\nStep one.\n",
        );
        write(
            &root.join("skills/deploy/checks.md"),
            "---\nx: y\n---\nCheck the logs.\n",
        );
        let help = SkillHelp {
            profile_dir: dir.path().to_owned(),
            lookup: Arc::new(Fixed(ChatKnowledge::default())),
        };
        assert_eq!(
            help.index("c"),
            [("deploy".to_owned(), "Ship it.".to_owned())]
        );
        let t = help.topic("c", "deploy").unwrap();
        assert!(
            t.starts_with("Step one.") && t.contains("skill/deploy/checks"),
            "{t}"
        );
        assert_eq!(help.topic("c", "deploy/checks").unwrap(), "Check the logs.");
        assert!(help.topic("c", "deploy/missing").is_none() && help.topic("c", "nope").is_none());
    }
}
