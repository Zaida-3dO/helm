//! When each rule reaches a chat: at launch, and at a tool call that touches it.
//!
//! Everything here is a plain function over one rule's trigger ([`crate::rules::RuleWhen`]), the
//! chat's [`Facts`] and one [`Moment`] (a tool call), plus [`SentStore`], which remembers what a
//! context has already been sent so nothing arrives twice. The callers — the launch, the message
//! path and the worker line — live elsewhere; this file only answers their questions. A rule with
//! no trigger is unaffected: it reaches every chat whole or on one line, as always.

use crate::launch::INLINE_RULE_CHARS;
use crate::rules::{parse_date, RuleWhen};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// The one kind of chat each chat has. Decided by the caller: orchestrating if the Orchestrator
/// is in effect for it, household if it came in through a channel or runs in the home folder,
/// coding if its folder sits inside a git work tree, else household.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatKind {
    Coding,
    Orchestrating,
    Household,
}

impl ChatKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChatKind::Coding => "coding",
            ChatKind::Orchestrating => "orchestrating",
            ChatKind::Household => "household",
        }
    }

    /// Parse a `kinds:` value; None for anything else.
    pub fn parse(name: &str) -> Option<ChatKind> {
        match name.to_ascii_lowercase().as_str() {
            "coding" => Some(ChatKind::Coding),
            "orchestrating" => Some(ChatKind::Orchestrating),
            "household" => Some(ChatKind::Household),
            _ => None,
        }
    }

    /// True when the kind is one of the names (an empty list accepts all). Case-insensitive:
    /// the header keeps whatever spelling a person wrote.
    fn in_list(self, names: &[String]) -> bool {
        names.iter().any(|n| n.eq_ignore_ascii_case(self.as_str()))
    }
}

/// What a chat is, as far as triggers look: where it runs, what kind it is, whose it is, which
/// PC it runs on, and today's local date. `today` is a field so a whole decision walk can be
/// tested without moving the clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    /// The chat's folder; None for chats with none (a phone or speaker chat, say).
    pub cwd: Option<PathBuf>,
    pub kind: ChatKind,
    /// The profile name of the person whose chat it is.
    pub person: Option<String>,
    /// This machine's PC name.
    pub pc: String,
    pub today: NaiveDate,
}

/// One tool call, as far as triggers look. `inner` carries the tool named inside a proxy call
/// (the `name` of an mcp proxy line) so patterns can match it too.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Moment {
    pub tool: String,
    pub inner: Option<String>,
    pub command: Option<String>,
    /// Paths the call touches: its path fields, the hook's folder and absolute-looking words
    /// in a command.
    pub paths: Vec<PathBuf>,
}

/// The input fields that name a single path.
const PATH_FIELDS: [&str; 4] = ["file_path", "notebook_path", "path", "cwd"];

impl Moment {
    /// Read one tool call: its name, the tool inside a proxy call, and every path it touches
    /// (its fields, the hook's folder `cwd`, and absolute-looking words in a command).
    pub fn from_call(tool: &str, input: &serde_json::Value, cwd: Option<&Path>) -> Moment {
        let mut m = Moment {
            tool: tool.to_owned(),
            inner: match tool.rsplit_once("__").map(|(_, n)| n) {
                Some("app_mcp_call") => input
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                _ => None,
            },
            command: input
                .get("command")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            paths: Vec::new(),
        };
        for field in PATH_FIELDS {
            if let Some(p) = input.get(field).and_then(|v| v.as_str()) {
                m.push_path(p);
            }
        }
        if let Some(list) = input.get("paths").and_then(|v| v.as_array()) {
            for p in list.iter().filter_map(|v| v.as_str()) {
                m.push_path(p);
            }
        }
        if let Some(c) = cwd {
            let text = c.to_string_lossy().into_owned();
            if !m.paths.iter().any(|p| p.to_string_lossy() == text) {
                m.paths.push(c.to_path_buf());
            }
        }
        if let Some(cmd) = &m.command {
            for word in abs_words(cmd) {
                let p = PathBuf::from(&word);
                if !m.paths.contains(&p) {
                    m.paths.push(p);
                }
            }
        }
        m
    }

    fn push_path(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() || self.paths.iter().any(|p| p == Path::new(text)) {
            return;
        }
        self.paths.push(PathBuf::from(text));
    }
}

/// A word that looks absolute, rewritten for comparison: a Git Bash `/c/dir` becomes `c:/dir`;
/// anything else keeps its text. Not an absolute-looking word at all means None.
fn word_path(word: &str) -> Option<String> {
    let w = word.trim_matches(|c| c == '"' || c == '\'' || c == '`');
    let b = w.as_bytes();
    // `X:\...` or `X:/...`.
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/') {
        return Some(w.to_owned());
    }
    // Git Bash drive form `/<letter>/rest`. A plain POSIX `/usr/...` is not it: its first run of
    // letters is longer than one.
    if b.len() >= 4 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b'/' {
        return Some(format!("{}/{}", &w[1..2].to_ascii_lowercase(), &w[2..]));
    }
    // A UNC share as written, and a home-relative word (kept plain; it only ever matches when
    // the header spells it the same way).
    if w.starts_with("\\\\") || w == "~" || w.starts_with("~/") {
        return Some(w.to_owned());
    }
    None
}

/// Absolute-looking words in a command, with quoted runs kept whole.
fn abs_words(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut run = String::new();
    let mut quote: Option<char> = None;
    let mut push = |run: &mut String, out: &mut Vec<String>| {
        if let Some(p) = word_path(run) {
            out.push(p);
        }
        run.clear();
    };
    for c in command.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    push(&mut run, &mut out);
                    quote = None;
                } else {
                    run.push(c);
                }
            }
            None => match c {
                '"' | '\'' | '`' => quote = Some(c),
                c if c.is_whitespace() => push(&mut run, &mut out),
                _ => run.push(c),
            },
        }
    }
    push(&mut run, &mut out);
    out
}

/// What the prompt shows for one rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    /// The whole text (until the shared prompt cap rewrites it to a line or the file list).
    Full,
    /// One line with the topic that carries the rest.
    Line,
    /// Nothing at all.
    Hidden,
}

/// True when every chat-wide condition of the rule holds for this chat: kinds, people, PCs and
/// the dates. These gate everything; a rule that fails them is hidden even where it otherwise
/// would fire (`otherwise` does not rescue it).
fn chat_gate(when: &RuleWhen, facts: &Facts) -> bool {
    if !when.kinds.is_empty() && !facts.kind.in_list(&when.kinds) {
        return false;
    }
    if !when.people.is_empty()
        && !when.people.iter().any(|p| {
            facts
                .person
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(p))
        })
    {
        return false;
    }
    if !when.pcs.is_empty() && !when.pcs.iter().any(|p| p.eq_ignore_ascii_case(&facts.pc)) {
        return false;
    }
    if let Some(d) = when.from.as_deref().and_then(parse_date) {
        if facts.today < d {
            return false;
        }
    }
    if let Some(d) = when.until.as_deref().and_then(parse_date) {
        if facts.today > d {
            return false;
        }
    }
    true
}

/// The decision for one rule at launch, before the shared prompt cap rewrites it. `always` is
/// the header's `always: true` flag (it means the same as `show: full`). A rule with moment
/// conditions shows what it promised (`otherwise`) until something fires it; a rule whose
/// folders already cover the chat's folder counts as fired at launch.
pub fn at_launch(when: &RuleWhen, always: bool, len_chars: usize, facts: &Facts) -> Decision {
    if !chat_gate(when, facts) {
        return Decision::Hidden;
    }
    if when.has_moment() {
        let fired = facts
            .cwd
            .as_ref()
            .is_some_and(|cwd| when.folders.iter().any(|f| inside(cwd, Path::new(f))));
        if !fired {
            return match when.otherwise.as_deref() {
                Some("line") => Decision::Line,
                _ => Decision::Hidden,
            };
        }
    }
    match when.show.as_deref() {
        Some("full") => Decision::Full,
        Some("line") => Decision::Line,
        // auto: whole text up to the inline limit, else one line.
        _ if always || len_chars <= INLINE_RULE_CHARS => Decision::Full,
        _ => Decision::Line,
    }
}

/// True when this call is a moment the rule watches. The chat conditions gate it just as at
/// launch; any matching moment dimension (a named tool, command, folder or machine) fires the
/// rule — that is what lets one rule name both several commands and several tools.
pub fn fires(when: &RuleWhen, facts: &Facts, m: &Moment) -> bool {
    if !chat_gate(when, facts) || !when.has_moment() {
        return false;
    }
    let tools = when
        .tools
        .iter()
        .any(|p| glob_match(p, &m.tool) || m.inner.as_ref().is_some_and(|i| glob_match(p, i)));
    if tools {
        return true;
    }
    let commands = m
        .command
        .as_deref()
        .is_some_and(|cmd| when.commands.iter().any(|c| command_match(c, cmd)));
    if commands {
        return true;
    }
    when.folders.iter().any(|f| {
        let folder = Path::new(f);
        m.paths.iter().any(|p| inside(p, folder))
    }) || when.hosts.iter().any(|h| {
        let want = h.to_lowercase();
        let hit = |t: &str| words(t).any(|w| w == want);
        m.command.as_deref().is_some_and(hit) || m.paths.iter().any(|p| hit(&p.to_string_lossy()))
    })
}

/// True when `inner` is `outer` or sits inside it, compared as text (lower case, forward
/// slashes). Header folders are taken exactly as written.
fn inside(inner: &Path, outer: &Path) -> bool {
    crate::is_inside(inner, outer)
}

/// Split text into whole words for host matching: runs of letters, digits, dot, dash and
/// underscore. `nas1` is a word in `ssh nas1 -v` but not in `ssh nas10 -v`.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
}

/// Case-insensitive name match with `*` as "any run of characters". A pattern without `__` is
/// read against the last `__`-part of the name, so `app_save_rule` matches
/// `mcp__<server>__app_save_rule`; a pattern with `__` must match the whole name.
fn glob_match(pattern: &str, name: &str) -> bool {
    let pat = pattern.trim().to_lowercase();
    let name = name.trim().to_lowercase();
    if pat.is_empty() || name.is_empty() {
        return false;
    }
    let target = if pat.contains("__") {
        name.as_str()
    } else {
        name.rsplit_once("__").map(|(_, t)| t).unwrap_or(&name)
    };
    glob_here(&pat, target)
}

/// Plain star matching without regex: the literal runs between `*` must appear in order, the
/// first (unless the pattern starts with `*`) at the start and the last (unless it ends with
/// `*`) at the end. A `*` may cover nothing.
fn glob_here(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return true; // the pattern was only stars
    }
    let anchored_start = !pattern.starts_with('*');
    let anchored_end = !pattern.ends_with('*');
    let mut at = 0usize;
    for (i, part) in parts.iter().enumerate() {
        let first_here = i == 0 && anchored_start;
        let last_ends = i + 1 == parts.len() && anchored_end;
        if first_here && last_ends {
            return text == *part;
        }
        if first_here {
            if !text[at..].starts_with(part) {
                return false;
            }
            at += part.len();
            continue;
        }
        if last_ends {
            // The final run must sit exactly at the end, after everything matched so far.
            return text.ends_with(part) && text.len() >= at + part.len();
        }
        match text[at..].find(part) {
            Some(off) => at += off + part.len(),
            None => return false,
        }
    }
    true
}

/// True when a listed command appears at the start of any segment of `cmd`. Segments split at
/// line ends and at `&&`, `||`, `;` and `|`; leading env assignments, `sudo`, `&` and `call` are
/// skipped in each, and an executable is matched by its bare name without a folder prefix or a
/// `.exe`.
fn command_match(pattern: &str, cmd: &str) -> bool {
    let want: Vec<String> = pattern.split_whitespace().map(str::to_lowercase).collect();
    if want.is_empty() {
        return false;
    }
    cmd.split(['\n', ';', '|', '&'])
        .any(|segment| starts_with_command(&want, segment))
}

fn starts_with_command(want: &[String], segment: &str) -> bool {
    let mut tokens = segment.split_whitespace().peekable();
    // Skip leading env assignments and lead-ins; they sit before the command proper.
    while let Some(t) = tokens.peek() {
        let t = t.trim_matches(|c| c == '"' || c == '\'');
        let lowered = t.to_lowercase();
        if lowered == "sudo"
            || lowered == "call"
            || lowered == "&"
            || (t.contains('=') && !t.contains('/') && !t.contains('\\'))
        {
            tokens.next();
        } else {
            break;
        }
    }
    let got: Vec<String> = tokens.take(want.len()).map(|t| exec_name(t)).collect();
    got.len() == want.len() && got.iter().zip(want).all(|(g, w)| g == w)
}

/// The bare executable name of a token: folder prefix and `.exe` gone.
fn exec_name(token: &str) -> String {
    let token = token.trim_matches(|c| c == '"' || c == '\'');
    let base = token.rsplit(['/', '\\']).next().unwrap_or(token);
    match base.len() >= 5 && base[base.len() - 4..].eq_ignore_ascii_case(".exe") {
        true => base[..base.len() - 4].to_lowercase(),
        false => base.to_lowercase(),
    }
}

// ---------------------------------------------------------------------------
// What each context has already seen.

/// The most chat records to keep; the oldest goes when a new one would pass the cap.
const MAX_SENT_CHATS: usize = 500;

/// One (chat, agent) pair's notes and used-up refusals, under its chat. Keying by the agent as
/// well means a sub-agent's hit does not use up the root chat's note or its one-time refusal.
#[derive(Clone, Debug, Default)]
struct AgentSeen {
    /// (rule key, revision): already given to this context.
    sent: HashSet<(String, String)>,
    /// Rule keys whose one-time refusal has been spent.
    held: HashSet<String>,
}

/// One chat's launch record and what its contexts have seen. Keyed by the chat id.
#[derive(Clone, Debug, Default)]
pub struct ChatRecord {
    /// Rule key -> what the launch showed and the revision it showed: the diff base for later
    /// messages. Empty means no launch record; message notes stay quiet until the next launch.
    pub launch: HashMap<String, (Decision, String)>,
    /// The chat's context generation; a change means compaction reset every context.
    pub gen: u64,
    /// The worker's epoch for this chat; a change clears notes and refusals alike.
    pub epoch: String,
    /// Set once `record_launch` has run: what the prompt actually showed. Touches from the
    /// note/refusal bookkeeping do not make a launch record on their own.
    launched: bool,
    agents: HashMap<String, AgentSeen>,
}

/// All chats' records with the touch order for eviction. The launch fills `launch` from the
/// prompt build; the message path diffs against it and the worker line answers through it.
#[derive(Debug)]
pub struct SentStore {
    chats: HashMap<String, (u64, ChatRecord)>,
    clock: u64,
}

impl Default for SentStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SentStore {
    pub fn new() -> Self {
        SentStore {
            chats: HashMap::new(),
            clock: 0,
        }
    }

    /// The record for a chat, creating it (and evicting the oldest when over cap).
    fn touch(&mut self, chat_id: &str) -> &mut ChatRecord {
        if !self.chats.contains_key(chat_id) && self.chats.len() >= MAX_SENT_CHATS {
            let oldest = self
                .chats
                .iter()
                .min_by_key(|(_, (tick, _))| *tick)
                .map(|(id, _)| id.clone());
            if let Some(id) = oldest {
                self.chats.remove(&id);
            }
        }
        self.clock += 1;
        let tick = self.clock;
        self.chats
            .entry(chat_id.to_owned())
            .or_insert_with(|| (tick, ChatRecord::default()))
            .1
    }

    fn seen(&mut self, chat_id: &str, agent: &str) -> &mut AgentSeen {
        let record = self.touch(chat_id);
        record.agents.entry(agent.to_owned()).or_default()
    }

    /// The launch showed `decisions` (rule key -> decision and revision): the record is
    /// replaced, which also resets what has been sent — a context is new.
    pub fn record_launch(&mut self, chat_id: &str, decisions: Vec<(String, Decision, String)>) {
        let record = self.touch(chat_id);
        *record = ChatRecord {
            launch: decisions.into_iter().map(|(k, d, r)| (k, (d, r))).collect(),
            launched: true,
            ..ChatRecord::default()
        };
    }

    /// A copy of one chat's launch map for a message note to diff against; None until the chat
    /// has been launched through this store.
    pub fn launch_map(&self, chat_id: &str) -> Option<HashMap<String, (Decision, String)>> {
        let record = &self.chats.get(chat_id)?.1;
        record.launched.then(|| record.launch.clone())
    }

    /// Whether the chat has a launch record at all (the message path stays quiet without one).
    pub fn has_launch(&self, chat_id: &str) -> bool {
        self.chats.get(chat_id).is_some_and(|(_, r)| r.launched)
    }

    /// A worker ended: its launch record must not outlive it, or the next send would diff
    /// against a dead launch and the relaunch would carry fresh rules twice.
    pub fn end_worker(&mut self, chat_id: &str) {
        self.chats.remove(chat_id);
    }

    /// The compaction generation changed: notes start again in every context of the chat
    /// (spent refusals wait for the epoch, matching where each reset is seen from).
    pub fn set_gen(&mut self, chat_id: &str, gen: u64) {
        let record = self.touch(chat_id);
        if record.gen != gen {
            record.gen = gen;
            for seen in record.agents.values_mut() {
                seen.sent.clear();
            }
        }
    }

    /// The worker's epoch changed: notes and spent refusals all start again.
    pub fn set_epoch(&mut self, chat_id: &str, epoch: &str) {
        let record = self.touch(chat_id);
        if record.epoch != epoch {
            record.epoch = epoch.to_owned();
            record.agents.clear();
        }
    }

    /// True the first time this rule's revision is asked about for a (chat, agent): send it.
    /// A repeat says "already sent".
    pub fn first_note(&mut self, chat_id: &str, agent: &str, key: &str, revision: &str) -> bool {
        self.seen(chat_id, agent)
            .sent
            .insert((key.to_owned(), revision.to_owned()))
    }

    /// True the first time this rule's one-time refusal is asked for in a (chat, agent):
    /// refuse that call; afterwards the identical call goes through.
    pub fn take_stop_turn(&mut self, chat_id: &str, agent: &str, key: &str) -> bool {
        self.seen(chat_id, agent).held.insert(key.to_owned())
    }

    /// Drop one chat's record entirely (a layer deleted outright, say).
    pub fn forget(&mut self, chat_id: &str) {
        self.chats.remove(chat_id);
    }

    /// How many chats are held (a test seam for the cap).
    pub fn chat_count(&self) -> usize {
        self.chats.len()
    }
}

static SENT: Mutex<Option<SentStore>> = Mutex::new(None);

/// Run one operation against the process-wide store: the launch records through it, the message
/// path reads it, and a worker line answers through it.
pub fn with_sent<R>(f: impl FnOnce(&mut SentStore) -> R) -> R {
    let mut guard: MutexGuard<'_, Option<SentStore>> =
        SENT.lock().unwrap_or_else(|p| p.into_inner());
    let store = guard.get_or_insert_with(SentStore::new);
    f(store)
}

/// The launch's own call, as the rules section uses it: record what one chat's prompt showed.
pub fn record_launch(chat_id: &str, decisions: Vec<(String, Decision, String)>) {
    with_sent(|s| s.record_launch(chat_id, decisions));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn facts(kind: ChatKind, cwd: Option<&str>, person: Option<&str>, pc: &str) -> Facts {
        Facts {
            cwd: cwd.map(PathBuf::from),
            kind,
            person: person.map(str::to_owned),
            pc: pc.to_owned(),
            today: NaiveDate::from_ymd_opt(2026, 11, 26).unwrap(),
        }
    }

    fn coding() -> Facts {
        facts(
            ChatKind::Coding,
            Some("C:\\work\\repo"),
            Some("alice"),
            "pc-a",
        )
    }

    /// Build a trigger from key/value pairs by reading it back as a header would.
    fn when(pairs: &[(&str, &str)]) -> RuleWhen {
        let head: String = pairs.iter().map(|(k, v)| format!("{k}: {v}\n")).collect();
        crate::rules::split_front(&format!("---\n{head}---\nx"))
            .0
            .when
            .clone()
    }

    #[test]
    fn a_rule_shows_whole_as_a_line_or_not_at_all_by_its_trigger() {
        let long = INLINE_RULE_CHARS + 1;
        let f = coding();
        // (trigger pairs, always, body length in chars, chat facts, expected decision)
        let cases: Vec<(Vec<(&str, &str)>, bool, usize, Facts, Decision)> = vec![
            // No trigger: today's behaviour, byte for byte.
            (vec![], false, 50, f.clone(), Decision::Full),
            (vec![], false, long, f.clone(), Decision::Line),
            (vec![], true, long, f.clone(), Decision::Full),
            // Show levels overrule the length either way.
            (vec![("show", "line")], false, 50, f.clone(), Decision::Line),
            (
                vec![("show", "full")],
                false,
                long,
                f.clone(),
                Decision::Full,
            ),
            (
                vec![("show", "auto")],
                false,
                long,
                f.clone(),
                Decision::Line,
            ),
            // Chat conditions: AND across dimensions, case does not matter.
            (
                vec![("kinds", "coding")],
                false,
                50,
                f.clone(),
                Decision::Full,
            ),
            (
                vec![("kinds", "household")],
                false,
                50,
                f.clone(),
                Decision::Hidden,
            ),
            (
                vec![("kinds", "coding, orchestrating")],
                false,
                50,
                facts(ChatKind::Orchestrating, None, None, "pc-a"),
                Decision::Full,
            ),
            (
                vec![("people", "alice")],
                false,
                50,
                f.clone(),
                Decision::Full,
            ),
            (
                vec![("people", "ALICE")],
                false,
                50,
                facts(ChatKind::Coding, None, Some("Bob"), "pc-a"),
                Decision::Hidden,
            ),
            (
                vec![("people", "alice")],
                false,
                50,
                facts(ChatKind::Coding, None, None, "pc-a"),
                Decision::Hidden,
            ),
            (vec![("pcs", "PC-A")], false, 50, f.clone(), Decision::Full),
            (
                vec![("pcs", "pc-b")],
                false,
                50,
                f.clone(),
                Decision::Hidden,
            ),
            (
                vec![("kinds", "coding"), ("people", "alice"), ("pcs", "pc-a")],
                false,
                50,
                f.clone(),
                Decision::Full,
            ),
            (
                // AND across dimensions: one that fails hides the rule.
                vec![("kinds", "coding"), ("pcs", "pc-b")],
                false,
                50,
                f.clone(),
                Decision::Hidden,
            ),
            // Dates are inclusive at both ends; a not-yet or expired rule is hidden.
            (
                vec![("from", "2026-11-01")],
                false,
                50,
                f.clone(),
                Decision::Full,
            ),
            (
                vec![("from", "2026-12-01")],
                false,
                50,
                f.clone(),
                Decision::Hidden,
            ),
            (
                vec![("until", "2026-11-26")],
                false,
                50,
                f.clone(),
                Decision::Full,
            ),
            (
                vec![("until", "2026-11-25")],
                false,
                50,
                f.clone(),
                Decision::Hidden,
            ),
            // Moment rules at launch: the promise of `otherwise` until they fire.
            (
                vec![("tools", "Bash")],
                false,
                50,
                f.clone(),
                Decision::Hidden,
            ),
            (
                vec![("tools", "Bash"), ("otherwise", "line")],
                false,
                50,
                f.clone(),
                Decision::Line,
            ),
            (
                // The chat gate beats otherwise: a failing kind hides even a line.
                vec![
                    ("tools", "Bash"),
                    ("otherwise", "line"),
                    ("kinds", "household"),
                ],
                false,
                50,
                f.clone(),
                Decision::Hidden,
            ),
            // Folders already matching at launch count as fired.
            (
                vec![("folders", "C:\\work\\repo")],
                false,
                50,
                f.clone(),
                Decision::Full,
            ),
            (
                vec![("folders", "C:\\work\\other")],
                false,
                50,
                f.clone(),
                Decision::Hidden,
            ),
            (
                vec![("folders", "C:\\WORK\\REPO"), ("show", "line")],
                false,
                long,
                f.clone(),
                Decision::Line,
            ),
            (
                // A chat without a folder cannot be inside any folder.
                vec![("folders", "C:\\work\\repo")],
                false,
                50,
                facts(ChatKind::Household, None, None, "pc-a"),
                Decision::Hidden,
            ),
        ];
        for (pairs, always, len, chat, want) in cases {
            let got = at_launch(&when(&pairs), always, len, &chat);
            assert_eq!(got, want, "trigger {pairs:?}");
        }
    }

    #[test]
    fn a_moment_fires_only_on_what_the_rule_names() {
        let f = coding();
        let bash = |cmd: &str, cwd: Option<&str>| {
            Moment::from_call(
                "Bash",
                &json!({"command": cmd}),
                cwd.map(PathBuf::from).as_deref(),
            )
        };

        // Commands match at the start of a segment, after leading forms and exe paths.
        let kill = when(&[("commands", "taskkill, Stop-Process")]);
        for cmd in [
            "taskkill /IM x",
            "C:\\Windows\\System32\\taskkill.exe /IM x",
            "FOO=bar sudo taskkill /IM x",
            "cd x && taskkill /IM x",
            "echo hi; taskkill /IM x",
        ] {
            assert!(fires(&kill, &f, &bash(cmd, None)), "{cmd}");
        }
        assert!(!fires(&kill, &f, &bash("echo do-taskkill-thing", None)));

        // Two-word commands match word by word.
        let git = when(&[("commands", "git push")]);
        assert!(fires(
            &git,
            &f,
            &bash("cd /c/work/repo && git push origin main", None)
        ));
        assert!(!fires(&git, &f, &bash("git revert x", None)));

        // Tools: star runs; a bare name reads as the last `__`-part; the inner tool of a proxy.
        let api = Moment::from_call(
            "mcp__helm__app_mcp_call",
            &json!({"name": "app_save_rule"}),
            None,
        );
        assert!(fires(&when(&[("tools", "app_save_rule")]), &f, &api));
        assert!(!fires(&when(&[("tools", "Bash")]), &f, &api));
        assert!(fires(
            &when(&[("tools", "mcp__github__*")]),
            &f,
            &Moment::from_call("mcp__github__fetch", &json!({}), None)
        ));

        // Folders: a Read field, absolute words in both Bash forms, and the hook's cwd.
        let folders = when(&[("folders", "C:\\work\\repo")]);
        assert!(fires(
            &folders,
            &f,
            &Moment::from_call("Read", &json!({"file_path": "C:\\work\\repo\\a.txt"}), None)
        ));
        assert!(!fires(
            &folders,
            &f,
            &Moment::from_call(
                "Read",
                &json!({"file_path": "C:\\work\\other\\a.txt"}),
                None
            )
        ));
        assert!(fires(&folders, &f, &bash("cat /c/work/repo/a.txt", None)));
        assert!(fires(
            &folders,
            &f,
            &bash("ls \"C:\\work\\repo\\odd name\"", None)
        ));
        assert!(!fires(
            &folders,
            &f,
            &Moment::from_call("Read", &json!({"paths": ["D:\\x", "E:\\y"]}), None)
        ));
        assert!(fires(&folders, &f, &bash("ls", Some("C:\\work\\repo"))));

        // Hosts are whole words in commands and paths; other PCs never match.
        let hosts = when(&[("hosts", "nas1, 192.0.2.10")]);
        assert!(fires(&hosts, &f, &bash("ssh nas1 -v docker ps", None)));
        assert!(!fires(&hosts, &f, &bash("ssh nas10 -v", None)));
        assert!(fires(&hosts, &f, &bash("ping 192.0.2.10", None)));
        let unc = Moment::from_call("Read", &json!({"file_path": "\\\\nas1\\homes\\x"}), None);
        assert!(fires(&hosts, &f, &unc));

        // Chat gates speak here too; a rule with no moment dimension never fires at a call.
        let home = when(&[("kinds", "household")]);
        assert!(!fires(&home, &f, &bash("taskkill /IM x", None)));
        let stopped = when(&[("commands", "taskkill"), ("until", "2026-11-25")]);
        assert!(!fires(&stopped, &f, &bash("taskkill /IM x", None)));
    }

    #[test]
    fn a_rule_goes_out_once_per_context() {
        let mut store = SentStore::new();
        store.record_launch(
            "chat-1",
            vec![
                ("a".into(), Decision::Full, "r1".into()),
                ("b".into(), Decision::Line, "r2".into()),
            ],
        );
        assert!(store.has_launch("chat-1"));
        assert!(!store.has_launch("chat-2"));

        // Notes go once per (chat, agent); a sub-agent keeps its own copy.
        assert!(store.first_note("chat-1", "root", "a", "r1"));
        assert!(!store.first_note("chat-1", "root", "a", "r1"));
        assert!(store.first_note("chat-1", "agent-1", "a", "r1"));
        // A new revision of the same rule goes again.
        assert!(store.first_note("chat-1", "root", "a", "r2"));

        // One refusal per (chat, agent); afterwards the identical call passes.
        assert!(store.take_stop_turn("chat-1", "root", "c"));
        assert!(!store.take_stop_turn("chat-1", "root", "c"));
        assert!(store.take_stop_turn("chat-1", "agent-1", "c"));

        // A new generation sends notes again but keeps spent refusals; a new epoch clears both.
        store.set_gen("chat-1", 7);
        assert!(store.first_note("chat-1", "root", "a", "r1"));
        assert!(!store.take_stop_turn("chat-1", "root", "c"));
        store.set_epoch("chat-1", "s2:0");
        assert!(store.first_note("chat-1", "root", "a", "r1"));
        assert!(store.take_stop_turn("chat-1", "root", "c"));

        // A relaunch replaces the record; a worker ending clears it, so a next send diffing for
        // a note gets nothing and the relaunch carries fresh rules exactly once.
        store.record_launch("chat-1", vec![("d".into(), Decision::Full, "r9".into())]);
        assert_eq!(store.launch_map("chat-1").unwrap().len(), 1);
        store.end_worker("chat-1");
        assert!(!store.has_launch("chat-1"));

        // The cap keeps the newest records.
        let mut big = SentStore::new();
        for n in 0..MAX_SENT_CHATS + 10 {
            big.record_launch(
                &format!("chat-{n}"),
                vec![("k".into(), Decision::Full, "r".into())],
            );
        }
        assert_eq!(big.chat_count(), MAX_SENT_CHATS);
        assert!(!big.has_launch("chat-0"));
        assert!(big.has_launch(&format!("chat-{}", MAX_SENT_CHATS + 9)));
    }

    #[test]
    fn path_words_globs_and_command_splits_read_as_documented() {
        assert_eq!(
            word_path("/c/work/repo/a.txt").as_deref(),
            Some("c:/work/repo/a.txt")
        );
        assert_eq!(word_path("/usr/bin"), None);
        assert_eq!(
            abs_words(r#"cat "C:\odd, name\x" /c/work/repo/a && echo hi"#),
            ["C:\\odd, name\\x".to_owned(), "c:/work/repo/a".to_owned()]
        );

        // Globs: suffix after the last __ for bare names; star runs anywhere.
        assert!(glob_match("app_save_rule", "mcp__helm__app_save_rule"));
        assert!(!glob_match("app_save_rule", "mcp__helm__app_save_rules"));
        assert!(glob_match(
            "mcp__*browser*",
            "mcp__broker__browser_navigate"
        ));
        assert!(!glob_match("mcp__github__*", "Bash"));
        assert!(glob_match("*help*", "app_help"));
        assert!(glob_match("Read", "Read"));
        assert!(!glob_match("", "Read"));

        // Commands: lead-ins are skipped only at the start of a segment.
        let want = vec!["git".to_owned(), "push".to_owned()];
        assert!(starts_with_command(&want, "FOO=1 sudo git push"));
        assert!(starts_with_command(&want, "  git   push origin"));
        assert!(!starts_with_command(&want, "git pull --rebase"));
        // command_match splits segments at `&&`, `;` and line ends.
        assert!(command_match("git push", "x && y; git push --force"));
        assert!(command_match(
            "taskkill",
            "first\r\nsecond\n& taskkill /IM x"
        ));
    }
}
