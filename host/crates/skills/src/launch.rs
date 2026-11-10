//! What a chat's launch takes from the knowledge layers.
//!
//! For one chat, [`for_chat`] works out the plugin folders (skills), the merged agents file and the
//! Claude Code setting sources, and [`RulesSection`] is the system-prompt section that carries the
//! rules. Both start from who the chat is for: its profile (the person) and its project, which a
//! [`ChatLookup`] answers. The app installs the lookup once ([`set_lookup`]); without one a chat is
//! treated as the home person's, with no project, so only the Everyone layer applies.

use crate::layers::Layer;
use crate::plugins::{self, Clash};
use crate::rules::{self, RuleEntry};
use crate::{agents, examples};
use helm_core::db::Database;
use helm_hosttools::AddendumSection;
use helm_workers::contrib::LaunchCtx;
use rusqlite::OptionalExtension;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// The rule text a launch carries, before the rest are listed by path instead.
pub const RULES_PROMPT_CAP: usize = 32 * 1024;

/// What the launch asks of the chat's own settings sources when its person is not this PC's home
/// person: the owner's personal Claude Code folder stays out.
pub const GUEST_SETTING_SOURCES: [&str; 2] = ["project", "local"];

/// The guidance that goes with the rules, so an agent files "always do this" in the right place.
pub const GUIDANCE: &str = "When the user asks you to always or never do something, or to remember something: a rule about how to work goes in `app_save_rule`; a fact or lesson goes in `memory_write` if shared memory is connected. Use `scope: \"me\"` (this chat's person) by default. Use `scope: \"everyone\"` only when the rule clearly holds whoever is asking (a convention of a tool, codebase or process) or the user says it is for everyone. Say in one short line where you saved it.";

/// The profile a chat belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileRef {
    pub id: String,
    pub name: String,
    /// The profile of the person who owns this Helm.
    pub is_home: bool,
}

/// Who a chat is for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatKnowledge {
    pub profile: Option<ProfileRef>,
    /// The folder project the chat runs in.
    pub project_id: Option<String>,
}

/// Answers who a chat is for.
pub trait ChatLookup: Send + Sync {
    fn of_chat(&self, chat_id: &str) -> ChatKnowledge;
}

/// The lookup over Helm's database: the chat's profile and its project.
pub struct DbLookup {
    pub db: Arc<Database>,
}

impl ChatLookup for DbLookup {
    fn of_chat(&self, chat_id: &str) -> ChatKnowledge {
        self.db
            .snapshot(|tx| {
                let profile =
                    helm_profiles::store::profile_of_chat(tx, chat_id)?.map(|p| ProfileRef {
                        id: p.id,
                        name: p.name,
                        is_home: p.is_home,
                    });
                let project_id = tx
                    .query_row(
                        "SELECT project_id FROM chats WHERE id = ?1",
                        [chat_id],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .optional()?
                    .flatten()
                    .filter(|p| !p.is_empty());
                Ok::<_, rusqlite::Error>(ChatKnowledge {
                    profile,
                    project_id,
                })
            })
            .unwrap_or_default()
    }
}

static LOOKUP: RwLock<Option<Arc<dyn ChatLookup>>> = RwLock::new(None);

/// Install the lookup launches and tools use. The app does this once; a later call replaces it.
pub fn set_lookup(lookup: Arc<dyn ChatLookup>) {
    *LOOKUP.write().unwrap_or_else(|p| p.into_inner()) = Some(lookup);
}

/// Who `chat_id` is for; nobody in particular when no lookup is installed.
pub fn who(chat_id: &str) -> ChatKnowledge {
    let lookup = LOOKUP.read().unwrap_or_else(|p| p.into_inner()).clone();
    lookup.map(|l| l.of_chat(chat_id)).unwrap_or_default()
}

/// What one chat's launch takes from the layers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    /// Folders for `--plugin-dir`: `helm`, `personal`, `project` as far as each has skills.
    pub plugin_dirs: Vec<String>,
    /// The merged agents file for `--agents`.
    pub agents_file: Option<String>,
    /// Claude Code setting sources to restrict the chat to; `None` leaves the default (all three).
    pub setting_sources: Option<Vec<String>>,
    /// Names two sources gave one plugin; the first is loaded.
    pub clashes: Vec<Clash>,
}

/// The layer launch for `who`: collect the skills, merge the agents, build the plugin folders.
/// Never fails: a problem is logged and that part is left out of the launch.
pub fn resolve(profile_dir: &Path, who: &ChatKnowledge) -> Resolved {
    examples::seed(profile_dir);
    let collected = plugins::collect(profile_dir, who);
    let mut roots: Vec<PathBuf> = vec![Layer::Everyone.root(profile_dir)];
    if let Some(p) = &who.profile {
        roots.push(Layer::Profile(p.id.clone()).root(profile_dir));
    }
    let root_refs: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    let agents = agents::launch_json(&root_refs);
    let built = plugins::build(profile_dir, &collected.skills, &agents).unwrap_or_else(|e| {
        log::warn!("skills: the plugin folders were not built: {e}");
        Default::default()
    });
    Resolved {
        plugin_dirs: built.plugin_dirs,
        agents_file: built.agents_file,
        setting_sources: who
            .profile
            .as_ref()
            .is_some_and(|p| !p.is_home)
            .then(|| GUEST_SETTING_SOURCES.map(str::to_owned).to_vec()),
        clashes: collected.clashes,
    }
}

/// [`resolve`] for a chat, through the installed lookup.
pub fn for_chat(profile_dir: &Path, chat_id: &str) -> Resolved {
    resolve(profile_dir, &who(chat_id))
}

/// One rule as the prompt carries it.
struct Loaded {
    entry: RuleEntry,
    file: PathBuf,
    body: String,
}

fn load_rules(root: &Path) -> Vec<Loaded> {
    rules::list_with_text(root)
        .into_iter()
        .filter_map(|(entry, text)| {
            let body = rules::body_of(&text?);
            (!body.is_empty()).then(|| Loaded {
                file: rules::rules_dir(root).join(entry.path.trim_start_matches("rules/")),
                entry,
                body,
            })
        })
        .collect()
}

/// The `rules` section's text: the Everyone rules, then the person's, then the guidance. Rules go
/// in whole until `cap` bytes of rule text are used; the rest are listed by path to read when they
/// apply.
pub fn rules_text(profile_dir: &Path, who: &ChatKnowledge, cap: usize) -> String {
    let mut groups: Vec<(String, Vec<Loaded>)> = vec![(
        "Rules for everyone".to_owned(),
        load_rules(&Layer::Everyone.root(profile_dir)),
    )];
    if let Some(p) = &who.profile {
        groups.push((
            format!("Rules for {}", p.name),
            load_rules(&Layer::Profile(p.id.clone()).root(profile_dir)),
        ));
    }
    let mut used = 0usize;
    let mut parts: Vec<String> = Vec::new();
    let mut overflow: Vec<String> = Vec::new();
    for (heading, rules) in groups {
        let mut block = String::new();
        for r in rules {
            let text = format!("### {}\n{}", r.entry.title, r.body);
            if used + text.len() <= cap {
                used += text.len();
                block.push_str(&format!("\n\n{text}"));
            } else {
                overflow.push(r.file.to_string_lossy().into_owned());
            }
        }
        if !block.is_empty() {
            parts.push(format!("{heading}:{block}"));
        }
    }
    if !overflow.is_empty() {
        parts.push(format!(
            "More rules did not fit here. Read these when they apply:\n{}",
            overflow
                .iter()
                .map(|p| format!("- {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    parts.push(GUIDANCE.to_owned());
    parts.join("\n\n")
}

/// The `rules` system-prompt section (order 6, after who the chat is for).
pub struct RulesSection {
    profile_dir: PathBuf,
    lookup: Arc<dyn ChatLookup>,
}

impl RulesSection {
    pub fn new(profile_dir: PathBuf, lookup: Arc<dyn ChatLookup>) -> Self {
        Self {
            profile_dir,
            lookup,
        }
    }
}

impl AddendumSection for RulesSection {
    fn id(&self) -> &'static str {
        "rules"
    }

    fn order(&self) -> i32 {
        6
    }

    fn text(&self, ctx: &LaunchCtx<'_>) -> Option<String> {
        examples::seed(&self.profile_dir);
        Some(rules_text(
            &self.profile_dir,
            &self.lookup.of_chat(ctx.chat_id),
            RULES_PROMPT_CAP,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::{self, Projects, SkillSource};
    use std::fs;

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn skill(dir: &Path, name: &str, body: &str) {
        write(
            &dir.join(name).join("SKILL.md"),
            &format!("---\nname: {name}\ndescription: {body}\n---\nDo it.\n"),
        );
    }

    fn person(id: &str, name: &str, home: bool) -> Option<ProfileRef> {
        Some(ProfileRef {
            id: id.into(),
            name: name.into(),
            is_home: home,
        })
    }

    fn link(profile: &Path, folder: &Path, layer: &str, projects: Projects) -> String {
        let s = sources::save(
            profile,
            SkillSource {
                id: String::new(),
                path: folder.to_string_lossy().into_owned(),
                layer: layer.into(),
                projects,
                enabled: true,
            },
            None,
        )
        .unwrap();
        s.sources.last().unwrap().id.clone()
    }

    /// The plugin folders a launch gets, as `plugin:skill` names.
    fn loaded(r: &Resolved) -> Vec<String> {
        let mut out = Vec::new();
        for dir in &r.plugin_dirs {
            let dir = Path::new(dir);
            let plugin = dir.file_name().unwrap().to_string_lossy().into_owned();
            let manifest: serde_json::Value =
                serde_json::from_slice(&fs::read(dir.join(".claude-plugin/plugin.json")).unwrap())
                    .unwrap();
            assert_eq!(manifest["name"], plugin.as_str());
            let mut names: Vec<String> = fs::read_dir(dir.join("skills"))
                .unwrap()
                .flatten()
                .map(|e| format!("{plugin}:{}", e.file_name().to_string_lossy()))
                .collect();
            names.sort();
            out.extend(names);
        }
        out
    }

    #[test]
    fn a_launch_resolves_layers_projects_clashes_and_caps() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let outside = tmp.path().join("outside");
        let examples_n = examples::SKILLS.len();
        let everyone = Layer::Everyone.root(&data);
        let alex = Layer::Profile("alex".into()).root(&data);

        // Layer folders: one shared skill, one personal, and the same name in both layers (no
        // clash: the plugins differ).
        skill(&everyone.join("skills"), "shared", "everyone");
        skill(&everyone.join("skills"), "both", "everyone");
        skill(&alex.join("skills"), "mine", "alex");
        skill(&alex.join("skills"), "both", "alex");
        write(
            &everyone.join("rules/b-style.md"),
            "# Style\n\nKeep it short.\n",
        );
        write(
            &alex.join("rules/a-habits.md"),
            "# Habits\n\nWrite tests.\n",
        );

        let home = ChatKnowledge {
            profile: person("alex", "Alex", true),
            project_id: Some("p1".into()),
        };
        let r = resolve(&data, &home);
        let names = loaded(&r);
        assert!(names.contains(&"helm:shared".into()) && names.contains(&"helm:both".into()));
        assert!(names.contains(&"personal:mine".into()) && names.contains(&"personal:both".into()));
        assert_eq!(
            names.iter().filter(|n| n.starts_with("helm:")).count(),
            examples_n + 2,
            "the example skills are in the Everyone layer"
        );
        assert!(
            r.plugin_dirs.iter().all(|d| !d.ends_with("project")),
            "empty plugins are left out"
        );
        assert_eq!(
            r.setting_sources, None,
            "the home person keeps the default sources"
        );
        let agents: serde_json::Value =
            serde_json::from_slice(&fs::read(r.agents_file.as_ref().unwrap()).unwrap()).unwrap();
        assert!(agents.get("planner").is_some());

        // The same inputs reuse the same folder; a changed skill builds another.
        assert_eq!(resolve(&data, &home).plugin_dirs, r.plugin_dirs);
        write(
            &alex.join("skills/mine/SKILL.md"),
            "---\ndescription: edited\n---\nNew.\n",
        );
        assert_ne!(resolve(&data, &home).plugin_dirs, r.plugin_dirs);

        // A guest profile gets the same layers but the owner's personal settings stay out; with no
        // profile at all only the Everyone layer loads.
        let guest = ChatKnowledge {
            profile: person("sam", "Sam", false),
            project_id: None,
        };
        let r = resolve(&data, &guest);
        assert_eq!(
            r.setting_sources,
            Some(vec!["project".into(), "local".into()])
        );
        assert!(
            !loaded(&r).iter().any(|n| n.starts_with("personal:")),
            "Sam has no layer yet"
        );
        let nobody = resolve(&data, &ChatKnowledge::default());
        assert!(loaded(&nobody).iter().all(|n| n.starts_with("helm:")));

        // Linked folders: all-projects joins the layer's plugin, a project list only loads in that
        // project, and a name clash inside one plugin keeps the layer's own skill.
        let tools = outside.join("tools");
        skill(&tools, "linked-all", "x");
        skill(&tools, "shared", "clashing");
        let only = outside.join("only");
        skill(&only, "proj-skill", "y");
        fs::create_dir_all(&data).unwrap();
        link(&data, &tools, "everyone", Projects::All);
        link(
            &data,
            &only,
            "profile:alex",
            Projects::Only(vec!["p1".into()]),
        );
        let in_p1 = resolve(&data, &home);
        let names = loaded(&in_p1);
        assert!(names.contains(&"helm:linked-all".into()));
        assert!(names.contains(&"project:proj-skill".into()));
        assert_eq!(in_p1.clashes.len(), 1);
        assert_eq!(
            (
                in_p1.clashes[0].plugin.as_str(),
                in_p1.clashes[0].name.as_str(),
                in_p1.clashes[0].kept.as_str()
            ),
            ("helm", "shared", "folder")
        );
        let in_p2 = resolve(
            &data,
            &ChatKnowledge {
                project_id: Some("p2".into()),
                ..home.clone()
            },
        );
        assert!(!loaded(&in_p2).iter().any(|n| n.starts_with("project:")));
        // Switching the source off drops its skills from the next launch.
        let snap = sources::load(&data).unwrap();
        let mut off = snap.sources[0].clone();
        off.enabled = false;
        sources::save(&data, off, Some(&snap.revision)).unwrap();
        assert!(!loaded(&resolve(&data, &home)).contains(&"helm:linked-all".into()));

        // Rules of both layers reach the prompt, the person's headed with their name.
        let text = rules_text(&data, &home, RULES_PROMPT_CAP);
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{needle} in {text}"))
        };
        assert!(
            at("Rules for everyone:") < at("### Style") && at("### Style") < at("Rules for Alex:")
        );
        assert!(at("Rules for Alex:") < at("### Habits") && at("### Habits") < at(GUIDANCE));
        assert!(text.contains("Keep it short.") && !text.contains("did not fit"));
        // Beyond the cap the rest are listed by path, not dropped; with no rules only the
        // guidance is left.
        let tight = rules_text(&data, &home, 30);
        assert!(tight.contains("### Style") && !tight.contains("### Habits"));
        assert!(tight.contains("Read these when they apply") && tight.contains("a-habits.md"));
        let bare = rules_text(&tmp.path().join("empty"), &ChatKnowledge::default(), 100);
        assert_eq!(bare, GUIDANCE);
    }
}
