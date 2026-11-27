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

impl Facts {
    /// The facts of one chat from what its lookup knows plus the launch folder. `cwd_override`
    /// wins over the recorded folder when given (the worker reports the real one). The kind is
    /// derived: orchestrating when the Orchestrator is in effect for the chat, household when the
    /// lookup says the chat came from a channel or runs on the home folder, coding when its
    /// folder sits inside a git work tree, household otherwise. A house chat pointed at a git
    /// folder keeps `household`: the override and the recorded folder never turn it into a
    /// coding chat, only the lookup's flags decide.
    pub fn of(who: &crate::launch::ChatKnowledge, cwd_override: Option<&Path>) -> Facts {
        let cwd = cwd_override.map(PathBuf::from).or_else(|| {
            who.cwd
                .as_deref()
                .map(PathBuf::from)
                .filter(|p| !p.as_os_str().is_empty())
        });
        let kind = if who.orchestrating {
            ChatKind::Orchestrating
        } else if who.household {
            ChatKind::Household
        } else if cwd.as_deref().is_some_and(in_git_tree) {
            ChatKind::Coding
        } else {
            ChatKind::Household
        };
        Facts {
            cwd,
            kind,
            person: who.profile.as_ref().map(|p| p.name.clone()),
            pc: pc_name(),
            today: chrono::Local::now().date_naive(),
        }
    }
}

/// This machine's PC name (the `pcs:` key is compared to it, ignoring case).
fn pc_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default()
}

/// How far above a folder the git walk looks for `.git`, and how long a result is reused. The
/// launch of several chats in one folder should not stat the parents over and over.
const GIT_WALK_PARENTS: usize = 12;
const GIT_CACHE_SECS: u64 = 10;

static GIT_TREES: Mutex<Option<HashMap<String, (std::time::Instant, bool)>>> = Mutex::new(None);

/// True when the folder or any of up to `GIT_WALK_PARENTS` parents holds a `.git`. Cached per
/// starting folder for a few seconds.
fn in_git_tree(dir: &Path) -> bool {
    let key = dir.to_string_lossy().to_lowercase();
    let mut guard = GIT_TREES.lock().unwrap_or_else(|p| p.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    if let Some((at, hit)) = map.get(&key) {
        if at.elapsed().as_secs() < GIT_CACHE_SECS {
            return *hit;
        }
    }
    let mut hit = false;
    let mut cur = Some(dir);
    for _ in 0..=GIT_WALK_PARENTS {
        let Some(d) = cur else { break };
        if d.join(".git").exists() {
            hit = true;
            break;
        }
        cur = d.parent();
    }
    map.insert(key, (std::time::Instant::now(), hit));
    hit
}

/// A `tools:` glob as a Claude Code hook matcher alternative: `*` becomes `.*`, the rest is
/// quoted, and a name without `__` is anchored so it matches the whole of a bare tool or the
/// last part of an `mcp__<server>__<name>` one. Patterns holding `__` match the whole name.
pub fn glob_to_regex(pattern: &str) -> String {
    let pat = pattern.trim();
    let mut re = String::new();
    // A plain name (letters, digits, `_`, `-`, and `*` wildcards only) anchors to the whole tool
    // name or its last `__`-part; anything else is a deliberate pattern — its regex characters
    // are quoted, but it matches wherever it fits.
    let mut plain = true;
    for c in pat.chars() {
        match c {
            '*' => re.push_str(".*"),
            c if !(c.is_ascii_alphanumeric() || c == '_' || c == '-') => {
                re.push('\\');
                re.push(c);
                plain = false;
            }
            c => re.push(c),
        }
    }
    if pat.contains("__") || !plain {
        re
    } else {
        format!("(^|__){re}$")
    }
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
        return Some(format!("{}:{}", w[1..2].to_ascii_lowercase(), &w[2..]));
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
    let push = |run: &mut String, out: &mut Vec<String>| {
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
    let got: Vec<String> = tokens.take(want.len()).map(exec_name).collect();
    got.len() == want.len() && got.iter().zip(want).all(|(g, w)| g == w)
}

/// The bare executable name of a token: folder prefix and `.exe` gone. Lower-casing happens
/// before any byte slicing, so a word with multi-byte characters cannot cut inside one.
fn exec_name(token: &str) -> String {
    let token = token.trim_matches(|c| c == '"' || c == '\'');
    let base = token.rsplit(['/', '\\']).next().unwrap_or(token);
    let b = base.to_lowercase();
    b.strip_suffix(".exe").unwrap_or(&b).to_owned()
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
    /// What a message note has carried since the launch (rule key -> decision and revision): the
    /// second half of the "no longer apply" base. A rule added after the launch is known to the
    /// chat through its note, so editing it away or deleting it must be reported like a launched
    /// rule going away would be.
    delivered: HashMap<String, (Decision, String)>,
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
        &mut self
            .chats
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

    /// The rules message notes have carried for this chat since its launch.
    pub fn delivered_map(&self, chat_id: &str) -> HashMap<String, (Decision, String)> {
        self.chats
            .get(chat_id)
            .map(|(_, r)| r.delivered.clone())
            .unwrap_or_default()
    }

    /// A message note's send committed: the rule it carried is now known to the chat.
    pub fn note_delivered(&mut self, chat_id: &str, key: &str, decision: Decision, revision: &str) {
        self.touch(chat_id)
            .delivered
            .insert(key.to_owned(), (decision, revision.to_owned()));
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

    /// The worker's epoch changed: notes and spent refusals all start again. The first value
    /// seen after a launch only sets the baseline — it is not a change, or notes carried between
    /// the launch and the first tool call would be forgotten and ride again.
    pub fn set_epoch(&mut self, chat_id: &str, epoch: &str) {
        let record = self.touch(chat_id);
        if record.epoch.is_empty() {
            record.epoch = epoch.to_owned();
            return;
        }
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

    /// Whether this rule's revision was already sent to a (chat, agent): read-only, so peeks
    /// answer without consuming anything.
    pub fn is_sent(&self, chat_id: &str, agent: &str, key: &str, revision: &str) -> bool {
        self.chats
            .get(chat_id)
            .and_then(|(_, r)| r.agents.get(agent))
            .is_some_and(|a| a.sent.contains(&(key.to_owned(), revision.to_owned())))
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

// ---------------------------------------------------------------------------
// The message note: what changed for a running chat since its launch.

/// A hidden-context note one message carries, plus the visible row it names.
#[derive(Clone, Debug)]
pub struct RuleNote {
    /// Hidden context riding the message: whole rules that newly apply, as `[Helm rule]` blocks.
    pub context: String,
    /// The transcript row: which rules were added or dropped.
    pub row: String,
    /// The (rule key, revision, decision) entries this note actually carried;
    /// `mark_message_sent` records them once the send commits, so a refused or rolled-back send
    /// does not consume the note.
    keys: Vec<(String, String, Decision)>,
}

/// The most whole-rule bytes one message note carries; the rest degrade to one line each.
const MSG_NOTE_CAP: usize = 6 * 1024;

fn rank(d: Decision) -> u8 {
    match d {
        Decision::Hidden => 0,
        Decision::Line => 1,
        Decision::Full => 2,
    }
}

/// The rule name for a row: the file stem from a rule key (`<layer>/<file>.md`).
fn key_stem(key: &str) -> String {
    let file = key.rsplit('/').next().unwrap_or(key);
    file.strip_suffix(".md").unwrap_or(file).to_owned()
}

/// What this chat should hear since its launch: rules now shown more fully (or whole rules
/// edited since), and rules that stopped applying or were deleted. Nothing before the chat has a
/// launch record, so a new chat never gets its rules twice before its first prompt. This is a
/// *peek*: it marks nothing. Call [`mark_message_sent`] only once the message write commits, so
/// a refused or rolled-back send does not consume the note.
pub fn message_note(profile_dir: &Path, chat_id: &str, gen: u64, _now_ms: i64) -> Option<RuleNote> {
    let (launched, delivered) = with_sent(|s| {
        if !s.has_launch(chat_id) {
            return None;
        }
        s.set_gen(chat_id, gen);
        Some((s.launch_map(chat_id)?, s.delivered_map(chat_id)))
    })?;
    let who = crate::launch::who(chat_id);
    let facts = Facts::of(&who, None);

    /// One rule as the diff sees it now.
    struct Now {
        key: String,
        decision: Decision,
        revision: String,
        title: String,
        body: String,
        topic: String,
        summary: String,
    }
    let current: Vec<Now> = crate::launch::rule_groups(profile_dir, &who)
        .into_iter()
        .flat_map(|(_, rules)| rules)
        .map(|r| {
            let summary = r.summary();
            Now {
                decision: at_launch(
                    &r.front.when,
                    r.front.always,
                    r.body.chars().count(),
                    &facts,
                ),
                key: r.key,
                revision: r.entry.revision,
                title: r.entry.title,
                body: r.body,
                topic: r.topic,
                summary,
            }
        })
        .collect();

    let mut adds: Vec<&Now> = Vec::new();
    for one in &current {
        let up = match launched.get(&one.key) {
            Some((prev, rev)) => {
                rank(one.decision) > rank(*prev)
                    || (one.decision == Decision::Full && *rev != one.revision)
            }
            None => one.decision != Decision::Hidden, // a rule added since the launch
        };
        if up && !with_sent(|s| s.is_sent(chat_id, "root", &one.key, &one.revision)) {
            adds.push(one);
        }
    }
    // Known means either the launch showed it or a message note carried it: both made it real
    // to this chat, so either way its going away has to be said.
    let known = |key: &str| {
        launched
            .get(key)
            .or_else(|| delivered.get(key))
            .map(|(d, _)| *d)
    };
    let mut gone: Vec<String> = current
        .iter()
        .filter_map(|one| {
            let was = known(&one.key)?;
            (rank(was) > 0 && one.decision == Decision::Hidden).then(|| key_stem(&one.key))
        })
        .collect();
    // Rules the chat was shown that are not in the layers any more: deleted.
    gone.extend(
        launched
            .iter()
            .chain(delivered.iter())
            .filter(|(key, (d, _))| rank(*d) > 0 && !current.iter().any(|c| c.key == **key))
            .map(|(key, _)| key_stem(key)),
    );
    gone.sort();
    gone.dedup();
    if adds.is_empty() && gone.is_empty() {
        return None;
    }
    adds.sort_by(|a, b| a.key.cmp(&b.key));
    gone.sort();
    let mut context = String::new();
    let mut keys: Vec<(String, String, Decision)> = Vec::new();
    let mut used_full = 0usize;
    let mut whole_count = 0usize;
    for one in &adds {
        if one.decision == Decision::Full
            && whole_count < 3
            && used_full + one.body.len() <= MSG_NOTE_CAP
        {
            context.push_str(&format!("\n\n[Helm rule]\n### {}\n{}", one.title, one.body));
            used_full += one.body.len();
            whole_count += 1;
        } else {
            context.push_str(&format!(
                "\n\n[Helm rule] {}: {} (read in full with app_help rule/{})",
                one.title, one.summary, one.topic
            ));
        }
        keys.push((one.key.clone(), one.revision.clone(), one.decision));
    }
    let mut row_parts: Vec<String> = Vec::new();
    if !adds.is_empty() {
        let names: Vec<&str> = adds.iter().map(|n| n.title.as_str()).collect();
        row_parts.push(format!("Rules added: {}", names.join(", ")));
    }
    if !gone.is_empty() {
        row_parts.push(format!(
            "These rules no longer apply here: {}",
            gone.join(", ")
        ));
    }
    Some(RuleNote {
        context: context.trim_start().to_owned(),
        row: row_parts.join(". "),
        keys,
    })
}

/// Record what a message note carried, once its send has committed: those rules will not ride
/// again in this context.
pub fn mark_message_sent(chat_id: &str, note: &RuleNote) {
    for (key, revision, decision) in &note.keys {
        with_sent(|s| {
            s.first_note(chat_id, "root", key, revision);
            s.note_delivered(chat_id, key, *decision, revision);
        });
    }
}

// ---------------------------------------------------------------------------
// The tool-call check: a worker asks what a call touches.

/// A `rule_check` line from the worker's PreToolUse hook. Unknown fields are ignored on purpose:
/// the line arrives whole from the worker, its `type` naming this handler (the guard's own check
/// is as lenient), and a refused line would leave every matching call waiting out the worker's
/// timeout with no answer at all.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleCheck {
    pub id: String,
    pub tool: String,
    #[serde(default)]
    pub input: serde_json::Value,
    #[serde(default)]
    pub cwd: Option<String>,
    /// The chat's root folder; a fallback for path checks when `cwd` is absent.
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub epoch: String,
    /// The agent making the call ("root" for the chat itself): its notes and refusals are
    /// tracked apart from the root's.
    #[serde(default)]
    pub agent_id: Option<String>,
}

/// What a message note holds whole per call at most, and their byte cap (the rest go on one line).
const TOOL_NOTE_WHOLE_MAX: usize = 3;
const TOOL_NOTE_CAP: usize = 4 * 1024;

/// One `rule_check` answered as the `rule_verdict` line for that worker. The store decides what
/// this chat and agent may still hear: a whole rule launched hidden, not yet noted this epoch,
/// that this call fires. Nothing is marked sent here — the worker's `rules_shown` acknowledgement
/// does that, so an answer that never arrived can come again.
pub fn check_rule_call(profile_dir: &Path, chat_id: &str, req: &RuleCheck) -> serde_json::Value {
    let agent = req.agent_id.clone().unwrap_or_else(|| "root".to_owned());
    let who = crate::launch::who(chat_id);
    let cwd = req
        .cwd
        .clone()
        .or_else(|| who.cwd.clone())
        .or_else(|| req.root.clone());
    let facts = Facts::of(
        &who,
        cwd.as_deref()
            .filter(|c| !c.is_empty())
            .map(std::path::Path::new),
    );
    let moment = Moment::from_call(&req.tool, &req.input, facts.cwd.as_deref());
    if !req.epoch.is_empty() {
        with_sent(|s| s.set_epoch(chat_id, &req.epoch));
    }
    let (stop, context, shown) = {
        let mut stop = None;
        let mut picked: Vec<(String, String, String)> = Vec::new(); // key, revision, whole text
        let launched = with_sent(|s| s.launch_map(chat_id).unwrap_or_default());
        for (_, rules) in crate::launch::rule_groups(profile_dir, &who) {
            for r in rules {
                if !r.front.when.has_moment() || !fires(&r.front.when, &facts, &moment) {
                    continue;
                }
                // Only a rule the prompt carried whole needs no note: a line shown at launch is
                // just the topic, and when the call fires the chat must still get the full text,
                // once.
                if launched
                    .get(&r.key)
                    .is_some_and(|(d, _)| *d == Decision::Full)
                {
                    continue;
                }
                if r.front.when.before.as_deref() == Some("stop") {
                    // Only one refusal per call: spend a turn only while none is spent yet.
                    if stop.is_none() && with_sent(|s| s.take_stop_turn(chat_id, &agent, &r.key)) {
                        stop = Some(format!(
                            "Refused by the rule \"{}\": {}",
                            r.entry.title, r.body
                        ));
                    }
                    continue;
                }
                let seen = with_sent(|s| s.is_sent(chat_id, &agent, &r.key, &r.entry.revision));
                if !seen {
                    picked.push((
                        r.key.clone(),
                        r.entry.revision.clone(),
                        format!("### {}\n{}", r.entry.title, r.body),
                    ));
                }
            }
        }
        picked.sort_by(|a, b| a.0.cmp(&b.0));
        let (context, shown) = notes_from(picked);
        (stop, context, shown)
    };
    let mut line = serde_json::json!({"type": "rule_verdict", "id": req.id});
    if let Some(reason) = stop {
        line["stop"] = serde_json::Value::String(reason);
        return line;
    }
    if !context.is_empty() {
        line["context"] = serde_json::Value::String(context);
        line["shown"] = serde_json::json!(shown);
    }
    line
}

/// Turn picked rules into the note text: at most TOOL_NOTE_WHOLE_MAX whole and 4 KB; the rest on
/// one line each. Each picked rule is a key, its revision and its whole `### title` + body text.
/// Returns (context, shown keys).
fn notes_from(picked: Vec<(String, String, String)>) -> (String, Vec<serde_json::Value>) {
    let mut context = String::new();
    let mut shown: Vec<serde_json::Value> = Vec::new();
    let mut used = 0usize;
    let mut whole = 0usize;
    for (key, revision, text) in picked {
        if whole < TOOL_NOTE_WHOLE_MAX && used + text.len() <= TOOL_NOTE_CAP {
            context.push_str(&format!("\n\n[Helm rule]\n{text}"));
            used += text.len();
            whole += 1;
        } else {
            // No room: name the file so the chat can read it itself. Still counted as shown to
            // stop every later call repeating it.
            context.push_str(&format!("\n\n[Helm rule] also applies: {}", key_stem(&key)));
        }
        shown.push(serde_json::json!({"key": key, "revision": revision}));
    }
    (context.trim_start().to_owned(), shown)
}

/// Record the notes a worker acknowledged showing.
pub fn mark_tool_notes(chat_id: &str, agent: &str, keys: &[(String, String)]) {
    for (key, revision) in keys {
        with_sent(|s| {
            s.first_note(chat_id, agent, key, revision);
        });
    }
}

/// Hear the worker's rule lines: answer each `rule_check` off the actor thread and remember the
/// notes it acknowledges via `rules_shown`.
pub fn register_worker_lines(l: &mut helm_hosttools::lines::WorkerLines, profile_dir: PathBuf) {
    l.register_worker_line("rule_check", move |ctx, v| {
        let chat = ctx.chat_id.to_owned();
        let services = ctx.services.clone();
        let dir = profile_dir.clone();
        let check: RuleCheck = match serde_json::from_value(v.clone()) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("chat {chat}: bad rule_check: {e}");
                return;
            }
        };
        let work = move || {
            let verdict = check_rule_call(&dir, &chat, &check);
            if let Some(b) = services.get::<helm_hosttools::lines::Broadcaster>() {
                if !b.0.send_to_worker(&chat, &verdict) {
                    log::debug!("chat {chat}: a rule verdict found no worker (it ended first)");
                }
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(h) => {
                h.spawn_blocking(work);
            }
            Err(_) => {
                std::thread::spawn(work);
            }
        }
    });
    l.register_worker_line("rules_shown", |ctx, v| {
        // The worker shows the note in its transcript; record it only now, per amendment to the
        // design: an answer that was never acknowledged may come again. If the app's actor takes
        // this line for its own transcript row first, call `mark_tool_notes` from there instead.
        let Some(keys) = v.get("keys").and_then(|k| k.as_array()) else {
            return;
        };
        let pairs: Vec<(String, String)> = keys
            .iter()
            .filter_map(|e| {
                Some((
                    e.get("key")?.as_str()?.to_owned(),
                    e.get("revision")?.as_str()?.to_owned(),
                ))
            })
            .collect();
        let agent = v
            .get("agentId")
            .and_then(|a| a.as_str())
            .unwrap_or("root")
            .to_owned();
        mark_tool_notes(ctx.chat_id, &agent, &pairs);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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
        type Case<'a> = (Vec<(&'a str, &'a str)>, bool, usize, Facts, Decision);
        let cases: Vec<Case> = vec![
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

        // A new generation sends notes again but keeps spent refusals; a *changed* epoch clears
        // both. The first epoch seen after a launch only sets the baseline, so set one here —
        // otherwise this call would be that baseline and change nothing.
        store.set_gen("chat-1", 7);
        assert!(store.first_note("chat-1", "root", "a", "r1"));
        assert!(!store.take_stop_turn("chat-1", "root", "c"));
        store.set_epoch("chat-1", "s1:0");
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
        // A word in another alphabet must not cut the `.exe` check inside a character, and an
        // upper-case name with its folder still matches.
        assert!(!command_match("git push", "echo Grüße"));
        assert!(command_match(
            "taskkill",
            "C:\\Tools\\TASKKILL.EXE /IM x && echo Grüße"
        ));

        // Globs become hook matcher alternatives: bare names anchor to the whole name or its
        // last `__`-part, patterns holding `__` match the whole name, and regex characters are
        // quoted.
        assert_eq!(glob_to_regex("app_save_rule"), "(^|__)app_save_rule$");
        assert_eq!(glob_to_regex("mcp__*browser*"), "mcp__.*browser.*");
        assert_eq!(glob_to_regex("git.push"), "git\\.push");
    }

    /// A helper writing one Everyone-layer rule and reading the crate's own decisions back.
    fn seed_rules(dir: &Path, files: &[(&str, &str)]) {
        let rules = crate::layers::Layer::Everyone.root(dir).join("rules");
        std::fs::create_dir_all(&rules).unwrap();
        for (name, text) in files {
            std::fs::write(rules.join(format!("{name}.md")), text).unwrap();
        }
        crate::launch::clear_rule_index();
    }

    fn note_chat_rules() -> [String; 3] {
        [
            // A plain short rule: whole at launch, and the baseline for later diffs.
            "Be kind.\n".to_owned(),
            // A commands rule with a one-time stop, kept back at launch.
            [
                "---",
                "commands: taskkill",
                "before: stop",
                "---",
                "# Kill by pid only",
                "",
                "Name kills take every worker down.",
            ]
            .join("\n"),
            // A tools rule kept back at launch, noted when the tool is used.
            [
                "---",
                "tools: Read",
                "---",
                "# Read carefully",
                "",
                "Look at what you overwrite.",
            ]
            .join("\n"),
        ]
    }

    #[test]
    fn a_tool_call_note_goes_once_and_a_stop_refuses_once_per_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let files = note_chat_rules();
        seed_rules(
            dir,
            &[
                ("kind", &files[0]),
                ("kills", &files[1]),
                ("reads", &files[2]),
            ],
        );
        let chat = "check-chat-toolonce";
        crate::launch::clear_rule_index();
        record_launch(
            chat,
            crate::launch::launch_decisions(dir, &Default::default()),
        );

        let req = |tool: &str, input: serde_json::Value, agent: Option<&str>| RuleCheck {
            id: "c1".into(),
            tool: tool.into(),
            input,
            cwd: None,
            root: None,
            epoch: "s1:0".into(),
            agent_id: agent.map(str::to_owned),
        };

        // The commands rule refuses this call once; the identical call afterwards passes.
        let kill = || {
            req(
                "Bash",
                serde_json::json!({"command": "taskkill /IM x"}),
                None,
            )
        };
        let first = check_rule_call(dir, chat, &kill());
        assert!(
            first["stop"]
                .as_str()
                .unwrap_or_default()
                .contains("Kill by pid only"),
            "{first}"
        );
        let second = check_rule_call(dir, chat, &kill());
        assert_eq!(second["stop"], serde_json::Value::Null);
        // A sub-agent keeps its own refusal turn.
        let sub = check_rule_call(
            dir,
            chat,
            &req(
                "Bash",
                serde_json::json!({"command": "taskkill /IM x"}),
                Some("sub-1"),
            ),
        );
        assert!(sub["stop"].is_string(), "{sub}");

        // The tools rule notes once per ack cycle: the answer itself marks nothing.
        let read = || req("Read", serde_json::json!({"file_path": "C:\\x\\a"}), None);
        let noted = check_rule_call(dir, chat, &read());
        assert!(
            noted["context"]
                .as_str()
                .unwrap_or_default()
                .contains("Read carefully"),
            "{noted}"
        );
        let again = check_rule_call(dir, chat, &read());
        assert!(again["context"].is_string(), "unacknowledged: comes again");
        let shown = noted["shown"].as_array().unwrap().clone();
        mark_tool_notes(
            chat,
            "root",
            &shown
                .iter()
                .map(|e| {
                    (
                        e["key"].as_str().unwrap().to_owned(),
                        e["revision"].as_str().unwrap().to_owned(),
                    )
                })
                .collect::<Vec<_>>(),
        );
        let after = check_rule_call(dir, chat, &read());
        assert_eq!(after["context"], serde_json::Value::Null);

        // A new epoch resets both notes and refusals.
        with_sent(|s| s.set_epoch(chat, "s2:0"));
        assert!(check_rule_call(dir, chat, &kill())["stop"].is_string());
    }

    /// The exact lines the worker's rule-notes hook sends (its fixture is
    /// `fakes/claude/fixtures/rule-notes.json`), driven through the registered handlers. A line
    /// that failed to parse answered nothing at all: no note, no refusal, and every matching call
    /// waited out the worker's timeout.
    #[test]
    fn the_exact_rule_check_lines_the_worker_sends_are_parsed_and_answered() {
        use helm_hosttools::lines::{Broadcaster, WorkerBroadcast, WorkerLineCtx, WorkerLines};
        use helm_ipc::{EventSink, NullSink, Services};
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let files = note_chat_rules();
        seed_rules(
            dir,
            &[
                ("kind", &files[0]),
                ("kills", &files[1]),
                ("reads", &files[2]),
            ],
        );
        struct Capture(std::sync::mpsc::Sender<serde_json::Value>);
        impl WorkerBroadcast for Capture {
            fn broadcast_to_workers(&self, _line: &serde_json::Value) {}
            fn send_to_worker(&self, _chat_id: &str, line: &serde_json::Value) -> bool {
                self.0.send(line.clone()).is_ok()
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let mut services = Services::new();
        services.insert(Arc::new(Broadcaster(Arc::new(Capture(tx)))));
        let services = Arc::new(services);
        let events: Arc<dyn EventSink> = Arc::new(NullSink);
        let mut l = WorkerLines::new();
        register_worker_lines(&mut l, dir.to_path_buf());
        let verdict = |id: &str| {
            rx.recv_timeout(std::time::Duration::from_secs(3))
                .unwrap_or_else(|_| panic!("no verdict came back for {id}"))
        };

        // The Read call from the fixtures (hook n1): it comes back with a note, which is what
        // answers the chat at all.
        let read = serde_json::json!({
            "type": "rule_check", "id": "n1", "tool": "Read",
            "input": { "file_path": "C:/work/repo/src/a.rs" },
            "cwd": "C:/work/repo", "epoch": "fake-session:0"
        });
        let chat = "exact-line-chat";
        assert!(
            l.handle(
                &WorkerLineCtx {
                    chat_id: chat,
                    services: &services,
                    events: &events,
                },
                &read
            ),
            "the line reached its handler"
        );
        let v1 = verdict("n1");
        assert_eq!(v1["id"], "n1");
        assert!(v1["context"].is_string(), "the note rides back: {v1}");

        // The name-kill Bash call (hook n2): the one-time refusal travels through the same shape.
        let bash = serde_json::json!({
            "type": "rule_check", "id": "n2", "tool": "Bash",
            "input": { "command": "taskkill /IM node.exe" },
            "cwd": "C:/work/repo", "epoch": "fake-session:0"
        });
        assert!(l.handle(
            &WorkerLineCtx {
                chat_id: chat,
                services: &services,
                events: &events,
            },
            &bash
        ));
        let v2 = verdict("n2");
        assert_eq!(v2["id"], "n2");
        assert!(v2["stop"].is_string(), "the refusal travels: {v2}");

        // The acknowledgement ride of the first note (shown keys, as the worker answers): a repeat
        // call then comes back empty, so nothing rides twice.
        let shown = v1["shown"].as_array().unwrap().clone();
        assert!(!shown.is_empty());
        assert!(l.handle(
            &WorkerLineCtx {
                chat_id: chat,
                services: &services,
                events: &events,
            },
            &serde_json::json!({"type": "rules_shown", "agentId": "root", "keys": shown})
        ));
        assert!(l.handle(
            &WorkerLineCtx {
                chat_id: chat,
                services: &services,
                events: &events,
            },
            &read
        ));
        let v3 = verdict("n1 again");
        assert!(v3["context"].is_null(), "spent: {v3}");
    }

    #[test]
    fn a_message_note_carries_added_edited_and_dropped_rules_once() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        seed_rules(dir, &[("kind", "# Be kind\n\nBe kind.\n")]);
        let chat = "note-chat-edits";
        record_launch(
            chat,
            crate::launch::launch_decisions(dir, &Default::default()),
        );

        // Nothing has changed: no note.
        assert!(message_note(dir, chat, 0, 0).is_none());

        // A rule appears and reaches this chat: it rides the next message whole, once marked.
        seed_rules(
            dir,
            &[
                ("kind", "# Be kind\n\nBe kind.\n"),
                ("sleep", "# Sleep tight\n\nLights out at ten.\n"),
            ],
        );
        let note = message_note(dir, chat, 0, 0).expect("added rule");
        assert!(note.context.contains("[Helm rule]\n### Sleep tight"));
        assert_eq!(note.row, "Rules added: Sleep tight");
        // The peek does not consume: it answers twice until marked.
        assert!(message_note(dir, chat, 0, 0).is_some());
        mark_message_sent(chat, &note);
        assert!(message_note(dir, chat, 0, 0).is_none());

        // Editing a whole rule sends the new text again (the revision changed).
        seed_rules(
            dir,
            &[
                ("kind", "# Be kind\n\nBe kind and brief.\n"),
                ("sleep", "# Sleep tight\n\nLights out at ten.\n"),
            ],
        );
        let note = message_note(dir, chat, 0, 0).expect("edited rule");
        assert!(note.context.contains("Be kind and brief."), "{note:?}");
        mark_message_sent(chat, &note);

        // A rule stops applying (a kinds gate this household chat fails): the row says so.
        seed_rules(
            dir,
            &[
                ("kind", "# Be kind\n\nBe kind and brief.\n"),
                (
                    "sleep",
                    "---\nkinds: coding\n---\n# Sleep tight\n\nLights out at ten.\n",
                ),
            ],
        );
        let note = message_note(dir, chat, 0, 0).expect("dropped rule");
        assert!(
            note.row.contains("no longer apply here: sleep"),
            "{}",
            note.row
        );

        // Deleting rules lands in the same row.
        seed_rules(dir, &[("kind", "# Be kind\n\nStill brief.\n")]);
        let note = message_note(dir, chat, 0, 0).expect("deleted and edited");
        assert!(
            note.row.contains("no longer apply here: sleep"),
            "{}",
            note.row
        );

        // A new context generation sends marked rules again.
        seed_rules(dir, &[("kind", "# Be kind\n\nBrief always.\n")]);
        let note = message_note(dir, chat, 7, 0).expect("after compaction");
        assert!(note.context.contains("Brief always."), "{note:?}");
    }

    #[test]
    fn the_kind_of_a_chat_comes_from_the_orchestrator_then_its_folder() {
        use crate::launch::ChatKnowledge;
        use std::fs;
        let base = tempfile::tempdir().unwrap();
        // No folder, no orchestrator: household.
        assert_eq!(
            Facts::of(&ChatKnowledge::default(), None).kind,
            ChatKind::Household
        );
        // The orchestrator flag wins over everything.
        let who = ChatKnowledge {
            orchestrating: true,
            ..ChatKnowledge::default()
        };
        assert_eq!(Facts::of(&who, None).kind, ChatKind::Orchestrating);
        // A folder inside a git work tree (marked by `.git` in it or above it) is coding…
        let repo = base.path().join("wt/work");
        fs::create_dir_all(base.path().join(".git")).unwrap();
        fs::create_dir_all(&repo).unwrap();
        assert_eq!(
            Facts::of(&ChatKnowledge::default(), Some(&repo)).kind,
            ChatKind::Coding,
            "the walk looks at parents too"
        );
        // …unless the lookup says the chat is a house one: a channel or home chat pointed at a
        // git folder keeps its household rules (the folder stays for path triggers to read).
        let house = ChatKnowledge {
            household: true,
            cwd: Some(repo.to_string_lossy().into_owned()),
            ..ChatKnowledge::default()
        };
        assert_eq!(Facts::of(&house, None).kind, ChatKind::Household);
        // Orchestrating still outranks the household flag.
        let both = ChatKnowledge {
            orchestrating: true,
            ..house
        };
        assert_eq!(Facts::of(&both, Some(&repo)).kind, ChatKind::Orchestrating);
        // …and a plain folder away from the git mark is household.
        let plain_base = tempfile::tempdir().unwrap();
        let plain = plain_base.path().join("plain");
        fs::create_dir_all(&plain).unwrap();
        assert_eq!(
            Facts::of(&ChatKnowledge::default(), Some(&plain)).kind,
            ChatKind::Household
        );
    }
}
