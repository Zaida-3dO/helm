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
use crate::triggers::{self, Decision, Facts};
use crate::{agents, examples};
use helm_core::db::Database;
use helm_hosttools::AddendumSection;
use helm_workers::contrib::LaunchCtx;
use rusqlite::OptionalExtension;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

/// The rule text a launch carries, before the rest are listed by path instead.
pub const RULES_PROMPT_CAP: usize = 32 * 1024;

/// What the launch asks of the chat's own settings sources when its person is not this PC's home
/// person: the owner's personal Claude Code folder stays out.
pub const GUEST_SETTING_SOURCES: [&str; 2] = ["project", "local"];

/// The guidance that goes with the rules, so an agent files "always do this" in the right place.
pub const GUIDANCE: &str = "When the user asks you to always or never do something, or to remember something: a rule about how to work goes in `app_save_rule`; a fact or lesson goes in `memory_write` if shared memory is connected, and a memory for someone else in the household goes in `app_save_memory`. Use `scope: \"me\"` (this chat's person) by default. Use `scope: \"everyone\"` only when the rule clearly holds whoever is asking (a convention of a tool, codebase or process) or the user says it is for everyone. Say in one short line where you saved it.";

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
    /// The folder the chat runs in (its project's path); None for chats with none. Rule triggers
    /// and the kind of chat are read from it.
    pub cwd: Option<String>,
    /// True when the Orchestrator is in effect for this chat; the app decides, since only it can
    /// load that setting.
    pub orchestrating: bool,
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
                // The chat's own profile, else the home profile. (Read here rather than through the
                // profiles crate, which sits above the chats crate that loads this one.)
                let row = |r: &rusqlite::Row<'_>| {
                    Ok::<_, rusqlite::Error>(ProfileRef {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        is_home: r.get(2)?,
                    })
                };
                let own = tx
                    .query_row(
                        "SELECT id, name, is_home FROM profiles
                         WHERE id = (SELECT NULLIF(profile_id, '') FROM chats WHERE id = ?1)",
                        [chat_id],
                        row,
                    )
                    .optional()?;
                let profile = match own {
                    Some(p) => Some(p),
                    None => tx
                        .query_row(
                            "SELECT id, name, is_home FROM profiles WHERE is_home = 1",
                            [],
                            row,
                        )
                        .optional()?,
                };
                let project_id = tx
                    .query_row(
                        "SELECT project_id FROM chats WHERE id = ?1",
                        [chat_id],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .optional()?
                    .flatten()
                    .filter(|p| !p.is_empty());
                // The folder the chat runs in: its project's path, when it has one.
                // Tolerated on error: an older schema without projects simply says no folder.
                let cwd = tx
                    .query_row(
                        "SELECT p.path FROM chats c JOIN projects p ON p.id = c.project_id WHERE c.id = ?1",
                        [chat_id],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .optional()
                    .unwrap_or(None)
                    .filter(|p| !p.is_empty());
                Ok::<_, rusqlite::Error>(ChatKnowledge {
                    profile,
                    project_id,
                    cwd,
                    orchestrating: false,
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
    /// A Claude Code hook matcher (a regex) for the tool calls any rule with a moment trigger
    /// cares about, so its worker registers the PreToolUse hook at all. `None` when no rule needs
    /// one; it is fixed at worker start and only that chat's own layers feed it.
    pub rule_matcher: Option<String>,
}

/// The matcher for one chat's layers: every `tools:` glob of a rule with a moment trigger, plus
/// the tools that carry `commands:` (shells) or look at paths (`folders`/`hosts` rules).
fn build_rule_matcher(layers: &[(&Path, &str)]) -> Option<String> {
    let mut alts: BTreeSet<String> = BTreeSet::new();
    for (root, layer_key) in layers {
        for r in load_rules(root, layer_key) {
            let w = &r.front.when;
            if !w.has_moment() {
                continue;
            }
            for p in &w.tools {
                let re = triggers::glob_to_regex(p);
                if !re.is_empty() {
                    alts.insert(re);
                }
            }
            // Commands only arrive through a shell; paths only through the file tools.
            if !w.commands.is_empty() {
                alts.extend(
                    ["Bash", "PowerShell", "Monitor"]
                        .map(str::to_owned)
                        .into_iter(),
                );
            }
            if !w.folders.is_empty() || !w.hosts.is_empty() {
                alts.extend(
                    [
                        "Read",
                        "Write",
                        "Edit",
                        "MultiEdit",
                        "NotebookEdit",
                        "Glob",
                        "Grep",
                        "Bash",
                        "PowerShell",
                    ]
                    .map(str::to_owned)
                    .into_iter(),
                );
            }
        }
    }
    (!alts.is_empty()).then(|| alts.into_iter().collect::<Vec<_>>().join("|"))
}

/// The layer launch for `who`: collect the skills, merge the agents, build the plugin folders.
/// Never fails: a problem is logged and that part is left out of the launch.
pub fn resolve(profile_dir: &Path, who: &ChatKnowledge) -> Resolved {
    examples::seed(profile_dir);
    let collected = plugins::collect(profile_dir, who);
    let mut layers: Vec<Layer> = vec![Layer::Everyone];
    if let Some(p) = &who.profile {
        layers.push(Layer::Profile(p.id.clone()));
    }
    let roots: Vec<PathBuf> = layers.iter().map(|l| l.root(profile_dir)).collect();
    let keys: Vec<String> = layers.iter().map(Layer::key).collect();
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
        rule_matcher: build_rule_matcher(
            &roots
                .iter()
                .zip(&keys)
                .map(|(r, k)| (r.as_path(), k.as_str()))
                .collect::<Vec<_>>(),
        ),
    }
}

/// [`resolve`] for a chat, through the installed lookup.
pub fn for_chat(profile_dir: &Path, chat_id: &str) -> Resolved {
    resolve(profile_dir, &who(chat_id))
}

/// A rule at most this long goes into the prompt whole; a longer one is listed by a line and read
/// with `app_help` when it applies, unless its header says `always: true`.
pub const INLINE_RULE_CHARS: usize = 800;

/// One rule as the prompt carries it.
#[derive(Clone)]
pub struct Loaded {
    pub entry: RuleEntry,
    pub file: PathBuf,
    pub body: String,
    pub front: rules::Front,
    /// Its name as a help topic (`rule/<name>`): the file's name, kept apart across layers.
    pub topic: String,
    /// What identifies this rule across layers and edits for the "already sent" record:
    /// `<layer key>/<file>.md`.
    pub key: String,
}

impl Loaded {
    /// What the rule is for, in one line of at most 100 characters: its header's summary, else its
    /// first sentences (not the ones that only repeat its title) until they say enough.
    pub fn summary(&self) -> String {
        let one = match &self.front.summary {
            Some(s) => s.clone(),
            None => lead_sentences(&self.body, &self.entry.title),
        };
        if one.chars().count() <= helm_hosttools::help::MAX_SUMMARY_CHARS {
            return one;
        }
        let head: String = one
            .chars()
            .take(helm_hosttools::help::MAX_SUMMARY_CHARS - 1)
            .collect();
        format!("{}…", head.trim_end())
    }
}

/// The first sentences of a rule's text, skipping any that begin with its title, until they hold at
/// least 40 characters. A line that ends in a colon leads into a list and ends the summary.
fn lead_sentences(body: &str, title: &str) -> String {
    let flat = body
        .lines()
        .map(|l| l.trim().trim_start_matches(['-', '*', ' ']).trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let title = title.to_lowercase();
    let mut out = String::new();
    let mut rest = flat.as_str();
    while !rest.is_empty() && out.chars().count() < 40 {
        let end = rest
            .char_indices()
            .find(|(i, c)| {
                (matches!(c, '.' | '!' | '?') || (*c == ':' && *i >= 30))
                    && rest[i + 1..].starts_with(' ')
            })
            .map(|(i, c)| (i + 1, c))
            .unwrap_or((rest.len(), '.'));
        let sentence = rest[..end.0].trim();
        rest = rest[end.0..].trim_start();
        if sentence.to_lowercase().starts_with(&title) {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(sentence.trim_end_matches(':'));
        if end.1 == ':' {
            break;
        }
    }
    out
}

/// A layer's rules, read once and kept until the folder says otherwise: hundreds of rule files
/// must not be re-read at every launch, budget check or tool call. An entry is rebuilt when two
/// seconds have passed and the sorted (name, size, mtime) list differs from last time; inside
/// those two seconds a change waits for the next walk. The hidden-character screen runs here, so
/// what reaches a prompt is already screened.
const RULE_INDEX_FRESH_SECS: u64 = 2;

struct RuleIndexEntry {
    at: std::time::Instant,
    sig: Vec<(String, u64, i64)>,
    rules: Vec<Loaded>,
}

static RULE_INDEX: Mutex<Option<HashMap<String, RuleIndexEntry>>> = Mutex::new(None);

/// Test seam: drop every cached layer index. Production relies on the fresh window to notice a
/// changed folder; tests write rules and read them back in the same breath.
#[cfg(test)]
pub(crate) fn clear_rule_index() {
    if let Some(map) = RULE_INDEX
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_mut()
    {
        map.clear();
    }
}

/// The (name, size, mtime) list of the rule folder, sorted; a folder that cannot be read lists
/// nothing.
fn rule_signature(root: &Path) -> Vec<(String, u64, i64)> {
    let Ok(read) = std::fs::read_dir(rules::rules_dir(root)) else {
        return Vec::new();
    };
    let mut sig: Vec<(String, u64, i64)> = read
        .flatten()
        .filter(|e| rules::file_name_of(&e.file_name().to_string_lossy()).is_ok())
        .filter_map(|e| {
            let meta = fs_symlink_meta(&e.path())?;
            Some((
                e.file_name().to_string_lossy().into_owned(),
                meta.len(),
                meta.modified()
                    .ok()
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0),
            ))
        })
        .collect();
    sig.sort();
    sig
}

fn fs_symlink_meta(path: &Path) -> Option<std::fs::Metadata> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if helm_platform::fs::is_link_like(&meta) || !meta.is_file() {
        return None;
    }
    Some(meta)
}

fn load_rules(root: &Path, layer_key: &str) -> Vec<Loaded> {
    let sig = rule_signature(root);
    let mut guard = RULE_INDEX.lock().unwrap_or_else(|p| p.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    let key = root.to_string_lossy().to_lowercase();
    if let Some(e) = map.get(&key) {
        // Inside the fresh window a change waits; outside it, no signature change keeps the old.
        if e.at.elapsed().as_secs() < RULE_INDEX_FRESH_SECS || e.sig == sig {
            return e.rules.clone();
        }
    }
    let rules = read_rules(root, layer_key);
    map.insert(
        key,
        RuleIndexEntry {
            at: std::time::Instant::now(),
            sig,
            rules: rules.clone(),
        },
    );
    rules
}

fn read_rules(root: &Path, layer_key: &str) -> Vec<Loaded> {
    rules::list_with_text(root)
        .into_iter()
        .filter_map(|(entry, text)| {
            let text = text?;
            // A rule is read by the model as instructions: one that hides text with invisible or
            // direction-changing characters (from the hub, or put there by hand) is left out.
            if helm_core::hidden::hides(&text) || helm_core::hidden::hides(&entry.title) {
                log::warn!(
                    "rules: {} holds hidden characters and was left out",
                    entry.path
                );
                return None;
            }
            let body = rules::body_of(&text);
            let front = rules::split_front(&text).0;
            let file = entry.path.trim_start_matches("rules/").to_owned();
            let stem = file.strip_suffix(".md").unwrap_or(&file).to_owned();
            (!body.is_empty()).then(|| Loaded {
                file: rules::rules_dir(root).join(&file),
                key: format!("{layer_key}/{file}"),
                entry,
                body,
                front,
                topic: stem,
            })
        })
        .collect()
}

/// The rules that apply to a chat, a group at a time: ("Rules for everyone", ...), then the
/// person's. A person's rule whose file name an Everyone rule also has is named `me-<name>` as a
/// help topic.
pub fn rule_groups(profile_dir: &Path, who: &ChatKnowledge) -> Vec<(String, Vec<Loaded>)> {
    let mut groups: Vec<(String, Vec<Loaded>)> = vec![(
        "Rules for everyone".to_owned(),
        load_rules(&Layer::Everyone.root(profile_dir), &Layer::Everyone.key()),
    )];
    if let Some(p) = &who.profile {
        let layer = Layer::Profile(p.id.clone());
        let mut own = load_rules(&layer.root(profile_dir), &layer.key());
        for r in &mut own {
            if groups[0].1.iter().any(|e| e.topic == r.topic) {
                r.topic = format!("me-{}", r.topic);
            }
        }
        groups.push((format!("Rules for {}", p.name), own));
    }
    groups
}

/// What the prompt does with one rule: carry it whole, list it on one line, or (once the cap is
/// reached) leave it out of the text and name its file at the end. A rule this chat has no part
/// in — a failing chat condition, or a moment trigger that has not fired — is Hidden: it appears
/// nowhere, and arrives later by note if it ever applies.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fit {
    Whole,
    Line,
    Over,
    Hidden,
}

/// A whole rule as the prompt carries it.
fn whole_rule(r: &Loaded) -> String {
    format!("### {}\n{}", r.entry.title, r.body)
}

/// The one walk the prompt text and the budget share: for each group, in rule order, how the
/// prompt treats each rule for this chat. A trigger decides Full, Line or Hidden first; a short
/// whole rule (or one marked `always`) then goes in until `cap` bytes of whole-rule text are
/// used, longer ones stay one line each, and whole rules past the cap fall to the file list.
fn rule_fits(groups: &[(String, Vec<Loaded>)], cap: usize, facts: &Facts) -> Vec<Vec<Fit>> {
    let mut used = 0usize;
    let mut out = Vec::with_capacity(groups.len());
    for (_, rules) in groups {
        let mut fits = Vec::with_capacity(rules.len());
        for r in rules {
            match triggers::at_launch(&r.front.when, r.front.always, r.body.chars().count(), facts)
            {
                Decision::Hidden => fits.push(Fit::Hidden),
                Decision::Line => fits.push(Fit::Line),
                Decision::Full => {
                    let len = whole_rule(r).len();
                    if used + len <= cap {
                        used += len;
                        fits.push(Fit::Whole);
                    } else {
                        fits.push(Fit::Over);
                    }
                }
            }
        }
        out.push(fits);
    }
    out
}

/// How much of the whole-rule cap one chat's rules use: (bytes used, cap). Settings reads this so
/// the figure it warns about is exactly what a chat's prompt goes through. Triggered rules that
/// this chat never gets count as nothing.
pub fn rules_budget(profile_dir: &Path, who: &ChatKnowledge) -> (usize, usize) {
    let facts = Facts::of(who, None);
    let groups = rule_groups(profile_dir, who);
    let mut used = 0usize;
    for ((_, rules), fits) in groups
        .iter()
        .zip(rule_fits(&groups, RULES_PROMPT_CAP, &facts))
    {
        for (r, fit) in rules.iter().zip(fits) {
            if fit == Fit::Whole {
                used += whole_rule(r).len();
            }
        }
    }
    (used, RULES_PROMPT_CAP)
}

/// The line shown when the chat's prompt left triggered rules out entirely.
pub const HELD_BACK: &str = "More rules are kept back and arrive with the message or tool call \
                             they cover, marked [Helm rule]; follow them as you would these.";

/// What a `Fit` means for the sent-record: what the chat was actually given.
fn fit_decision(fit: Fit) -> Decision {
    match fit {
        Fit::Whole => Decision::Full,
        Fit::Hidden => Decision::Hidden,
        _ => Decision::Line,
    }
}

/// The `rules` section's text plus the per-rule decisions it showed (rule key, decision and
/// revision), which the launch records so later notes can diff against it. A rule with no
/// trigger walks exactly as before; a triggered one shows what its trigger promises for this
/// chat, and rules kept back add one line.
fn render_rules(
    profile_dir: &Path,
    who: &ChatKnowledge,
    cap: usize,
) -> (String, Vec<(String, Decision, String)>) {
    let facts = Facts::of(who, None);
    let groups = rule_groups(profile_dir, who);
    let fits = rule_fits(&groups, cap, &facts);
    let mut parts: Vec<String> = Vec::new();
    let mut overflow: Vec<String> = Vec::new();
    let mut decisions: Vec<(String, Decision, String)> = Vec::new();
    let mut held_back = false;
    for ((heading, rules), fits) in groups.iter().zip(&fits) {
        let mut block = String::new();
        let mut listed = String::new();
        for (r, fit) in rules.iter().zip(fits) {
            decisions.push((r.key.clone(), fit_decision(*fit), r.entry.revision.clone()));
            match fit {
                Fit::Hidden => held_back = true,
                Fit::Line => listed.push_str(&format!(
                    "\n- {}: {} (app_help rule/{})",
                    r.entry.title,
                    r.summary(),
                    r.topic
                )),
                Fit::Whole => block.push_str(&format!("\n\n{}", whole_rule(r))),
                Fit::Over => overflow.push(r.file.to_string_lossy().into_owned()),
            }
        }
        if !listed.is_empty() {
            block.push_str(&format!(
                "\n\nLonger rules, one line each; read one in full with app_help before the work it covers:{listed}"
            ));
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
    if held_back {
        parts.push(HELD_BACK.to_owned());
    }
    parts.push(GUIDANCE.to_owned());
    (parts.join("\n\n"), decisions)
}

/// The `rules` section's text: the Everyone rules, then the person's, then the guidance. A short
/// rule (or one marked `always`) goes in whole until `cap` bytes of rule text are used; the others
/// are one line each, with the topic that has the rest. Rules a trigger keeps from this chat do
/// not appear at all.
pub fn rules_text(profile_dir: &Path, who: &ChatKnowledge, cap: usize) -> String {
    render_rules(profile_dir, who, cap).0
}

/// The decisions one chat's launch would be shown now, as the message path diffs against them.
pub fn launch_decisions(
    profile_dir: &Path,
    who: &ChatKnowledge,
) -> Vec<(String, Decision, String)> {
    render_rules(profile_dir, who, RULES_PROMPT_CAP).1
}

/// The decision one rule reaches for a chat at launch; the `app_help rule` index uses it to list
/// exactly what the prompt showed.
pub fn shown_at_launch(r: &Loaded, facts: &Facts) -> bool {
    triggers::at_launch(&r.front.when, r.front.always, r.body.chars().count(), facts)
        != Decision::Hidden
}

/// The `rules` system-prompt section. Order 6 puts it first among the addendum's sections, ahead
/// of verbosity (10); the line saying who the chat is for is its own launch contributor and comes
/// before the whole addendum.
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
        // The worker reports the folder it really runs in; prefer it over the recorded one.
        let mut who = self.lookup.of_chat(ctx.chat_id);
        if !ctx.cwd.is_empty() {
            who.cwd = Some(ctx.cwd.to_owned());
        }
        let (text, decisions) = render_rules(&self.profile_dir, &who, RULES_PROMPT_CAP);
        // What this chat was just shown: later messages and tool calls diff against this.
        triggers::record_launch(ctx.chat_id, decisions);
        Some(text)
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
            ..ChatKnowledge::default()
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
            ..ChatKnowledge::default()
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
        // The budget goes through the same walk: everything that fits whole at the real cap is
        // counted, and a layer with nothing in it says zero.
        let whole_sum = rule_groups(&data, &home)
            .iter()
            .flat_map(|(_, rs)| rs.iter().map(whole_rule))
            .map(|t| t.len())
            .sum::<usize>();
        assert!(whole_sum > 0 && whole_sum <= RULES_PROMPT_CAP);
        assert_eq!(rules_budget(&data, &home), (whole_sum, RULES_PROMPT_CAP));
        assert_eq!(
            rules_budget(&tmp.path().join("empty"), &ChatKnowledge::default()),
            (0, RULES_PROMPT_CAP)
        );

        // Triggers. This chat has no folder and no orchestrator, so it is a household chat: a
        // coding-only rule is kept back, one with only moment conditions stays back until its
        // folder matches, and the prompt adds one line saying so.
        let plain = rules_text(&data, &home, RULES_PROMPT_CAP);
        write(
            &everyone.join("rules/coding-only.md"),
            "---\nkinds: coding\n---\n# Diff first\n\nRead the diff before editing.\n",
        );
        write(
            &alex.join("rules/in-repo.md"),
            &format!(
                "---\nfolders: {}\ntools: app_save_rule, mcp__*browser*\ncommands: git push\n---\n# Repo work\n\nStay in the repo.\n",
                tmp.path().join("repo").display()
            ),
        );
        clear_rule_index();
        let trig = rules_text(&data, &home, RULES_PROMPT_CAP);
        assert!(plain.ends_with(GUIDANCE) && !plain.contains(HELD_BACK));
        assert!(
            !trig.contains("Diff first") && trig.contains(HELD_BACK),
            "{trig}"
        );
        assert!(
            !trig.contains("Stay in the repo"),
            "folder unmatched at launch"
        );
        // Everything a chat got before triggers existed comes through byte-identically; only
        // the kept-back line is inserted.
        let restored = trig.replace(&format!("{HELD_BACK}\n\n"), "");
        assert_eq!(restored, plain);

        // In the folder the rule names it fires at launch and goes in whole; the budget of a
        // chat with no folder counts only what that chat gets.
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        let in_repo = ChatKnowledge {
            cwd: Some(repo.to_string_lossy().into_owned()),
            ..home.clone()
        };
        let shown = rules_text(&data, &in_repo, RULES_PROMPT_CAP);
        assert!(shown.contains("### Repo work") && shown.contains("Stay in the repo."));
        // The budget of the chat with no folder still counts only what that chat gets: the two
        // whole rules, with both triggered rules (hidden for it) contributing nothing.
        assert_eq!(rules_budget(&data, &home), (whole_sum, RULES_PROMPT_CAP));

        // The matcher this chat's worker would start with: tool globs converted, shells added
        // for commands, and `None` when no rule watches a moment.
        let m = resolve(&data, &home).rule_matcher.unwrap();
        assert!(
            m.contains("(^|__)app_save_rule$") && m.contains("mcp__.*browser.*"),
            "{m}"
        );
        assert!(
            m.contains("Bash") && m.contains("PowerShell") && m.contains("Monitor"),
            "{m}"
        );
        assert_eq!(
            resolve(&tmp.path().join("empty"), &ChatKnowledge::default()).rule_matcher,
            None
        );

        // What the launch records is one entry per rule with its decision and revision; a hidden
        // trigger lands as Hidden.
        let dec = launch_decisions(&data, &home);
        assert!(dec
            .iter()
            .any(|(k, d, _)| { k == "everyone/coding-only.md" && *d == Decision::Hidden }));
        assert!(dec
            .iter()
            .any(|(k, d, _)| k == "everyone/b-style.md" && *d == Decision::Full));
    }
}
