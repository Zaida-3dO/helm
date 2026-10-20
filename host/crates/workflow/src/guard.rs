//! The landing guard's evaluator. Pure: it
//! judges one shell command against the tracked set and the protected branches, with repository
//! facts injected ([`RepoFacts`]). Wiring it into Claude Code's `PreToolUse` Bash hook waits for
//! a later step; [`crate::overrides`] turns a refusal plus an override marker into a decision.

use crate::shell::{self, Parsed, Simple};
use std::path::{Path, PathBuf};

/// Commands larger than this are not read; the guard fails closed where work is at stake.
pub const MAX_COMMAND: usize = 1024 * 1024;

/// What git says about a working folder. `None` means it could not say (fail closed).
pub trait RepoFacts {
    /// `host/owner/name` of the folder's `origin` remote.
    fn repo(&self, cwd: &Path) -> Option<String>;
    fn current_branch(&self, cwd: &Path) -> Option<String>;
    fn default_branch(&self, cwd: &Path) -> Option<String>;
    /// `host/owner/name` of a named remote. The default knows only `origin`; any other remote is
    /// unknown, so a push to it fails closed where work is at stake.
    fn remote_repo(&self, cwd: &Path, remote: &str) -> Option<String> {
        (remote == "origin").then(|| self.repo(cwd)).flatten()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tracked {
    pub repo: String,
    pub branch: String,
    pub pr: Option<String>,
}

/// The state the guard judges against.
#[derive(Clone, Debug, Default)]
pub struct Ctx {
    pub cwd: PathBuf,
    pub task_active: bool,
    /// The active task's repositories.
    pub task_repos: Vec<String>,
    /// Every non-terminal change of every open task, in any chat.
    pub tracked: Vec<Tracked>,
    pub protected: Vec<String>,
    /// Repositories whose land rule is draft.
    pub draft_repos: Vec<String>,
    pub flip_label: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// `# helm-override: <reason>` is enough.
    Reason,
    /// The override must quote the user's own words.
    Quote,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny {
        tier: Tier,
        rule: &'static str,
        why: String,
    },
}

/// The landing words the pre-filter looks for (after quotes are read).
const LANDING_WORDS: [&str; 11] = [
    "send-pack",
    "push",
    "merge",
    "pull",
    "ready",
    "create",
    "api",
    "sync",
    "curl",
    "hub",
    "remove-label",
];

fn mentions_landing(line: &str) -> bool {
    // `$'...'` and `$"..."` can spell any word with escapes (`$'\x70ush'`): read those lines in
    // full, where the reader decodes them.
    if line.contains("$'") || line.contains("$\"") {
        return true;
    }
    // Look past quoting, escapes and brace syntax, so `git pu''sh` and `git pu{sh,}` are still
    // seen as a push; and, separately, past expansions that can be empty and glob brackets, so
    // `git pu$@sh`, `git pu$(true)sh` and `git p[u]sh` are too.
    let bare = |s: &str| -> String {
        s.chars()
            .filter(|c| !matches!(c, '\'' | '"' | '\\' | '{' | '}' | ','))
            .collect()
    };
    let plain = bare(line);
    let unexpanded = bare(&without_expansions(line, false));
    if LANDING_WORDS
        .iter()
        .any(|w| plain.contains(w) || unexpanded.contains(w))
    {
        return true;
    }
    // A pattern word (`pu?h`, `p*`) can become a landing word if a file of that name exists.
    bare(&without_expansions(line, true))
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '|' | '&' | '(' | ')' | '<' | '>'))
        .filter(|t| t.contains(GLOB))
        .any(|t| t.len() > 256 || LANDING_WORDS.iter().any(|w| glob_matches(t, w)))
}

/// Whether the filename pattern `pattern` (`*`, `?` and `[...]`, any class read as "one
/// character") matches all of `text`.
fn glob_matches(pattern: &str, text: &str) -> bool {
    enum El {
        Star,
        One,
        Lit(char),
    }
    let p: Vec<char> = pattern.chars().collect();
    let mut els = Vec::new();
    let mut i = 0;
    while i < p.len() {
        match p[i] {
            '*' => els.push(El::Star),
            '?' => els.push(El::One),
            '[' => match p[i + 1..].iter().position(|&c| c == ']') {
                Some(k) => {
                    els.push(El::One);
                    i += k + 1;
                }
                None => els.push(El::Lit('[')),
            },
            c => els.push(El::Lit(c)),
        }
        i += 1;
    }
    let t: Vec<char> = text.chars().collect();
    // reach[j]: the elements read so far can match exactly the first j characters of `text`.
    let mut reach = vec![false; t.len() + 1];
    reach[0] = true;
    for el in &els {
        let mut next = vec![false; t.len() + 1];
        for j in 0..=t.len() {
            match el {
                El::Star => next[j] = reach[j] || (j > 0 && next[j - 1]),
                El::One => next[j] = j > 0 && reach[j - 1],
                El::Lit(c) => next[j] = j > 0 && reach[j - 1] && t[j - 1] == *c,
            }
        }
        reach = next;
    }
    reach[t.len()]
}

/// `line` with every `$` expansion and backtick substitution removed, and glob brackets either
/// removed or kept. Only the pre-filter uses it, so it may remove too much but never too little
/// of a landing word.
fn without_expansions(line: &str, keep_brackets: bool) -> String {
    let mut out = String::new();
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '$' => match it.peek().copied() {
                Some(open @ ('(' | '{')) => {
                    let close = if open == '(' { ')' } else { '}' };
                    let mut depth = 0usize;
                    for x in it.by_ref() {
                        if x == open {
                            depth += 1;
                        } else if x == close {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                    }
                }
                Some(x) if x.is_ascii_alphabetic() || x == '_' => {
                    while it
                        .peek()
                        .is_some_and(|x| x.is_ascii_alphanumeric() || *x == '_')
                    {
                        it.next();
                    }
                }
                Some(_) => {
                    it.next();
                }
                None => {}
            },
            '`' => {
                for x in it.by_ref() {
                    if x == '`' {
                        break;
                    }
                }
            }
            '[' | ']' if !keep_brackets => {}
            c => out.push(c),
        }
    }
    out
}

/// The characters that make a word a filename pattern. The reader does not keep quoting, so a
/// quoted one counts too, which can only make the guard stricter.
const GLOB: [char; 3] = ['*', '?', '['];

enum Shape {
    None,
    /// A landing into `branch` of `repo` (a push, merge or pull into it).
    Into {
        repo: Option<String>,
        branch: Option<String>,
        at: PathBuf,
    },
    /// A GitHub-side landing: PR merge or ready, label removal, an API write, a repo sync.
    GitHub {
        repo: Option<String>,
        pr: Option<String>,
        what: &'static str,
    },
    /// A non-draft PR opened in `repo`.
    NonDraftPr {
        repo: Option<String>,
    },
    Unreadable(&'static str),
}

/// A refspec side as a branch name. git also finds `heads/main` as `refs/heads/main`.
fn strip_ref(r: &str) -> String {
    let r = r.trim_start_matches('+');
    let r = r.strip_prefix("refs/").unwrap_or(r);
    r.strip_prefix("heads/").unwrap_or(r).to_owned()
}

/// Whether a `git push` option takes a value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Takes {
    Nothing,
    /// `--opt value` or `--opt=value`.
    Value,
    /// Only `--opt=value`.
    Glued,
}

/// The long options of `git push` (and `git send-pack`'s that matter). git accepts any unique
/// prefix of one, and `--no-<name>` for most, so the guard resolves them the same way.
const PUSH_LONG: [(&str, Takes); 29] = [
    ("verbose", Takes::Nothing),
    ("quiet", Takes::Nothing),
    ("repo", Takes::Value),
    ("all", Takes::Nothing),
    ("branches", Takes::Nothing),
    ("mirror", Takes::Nothing),
    ("delete", Takes::Nothing),
    ("tags", Takes::Nothing),
    ("dry-run", Takes::Nothing),
    ("porcelain", Takes::Nothing),
    ("force", Takes::Nothing),
    ("force-with-lease", Takes::Glued),
    ("force-if-includes", Takes::Nothing),
    ("recurse-submodules", Takes::Value),
    ("thin", Takes::Nothing),
    ("receive-pack", Takes::Value),
    ("exec", Takes::Value),
    ("set-upstream", Takes::Nothing),
    ("progress", Takes::Nothing),
    ("prune", Takes::Nothing),
    ("verify", Takes::Nothing),
    ("no-verify", Takes::Nothing),
    ("follow-tags", Takes::Nothing),
    ("signed", Takes::Glued),
    ("atomic", Takes::Nothing),
    ("push-option", Takes::Value),
    ("ipv4", Takes::Nothing),
    ("ipv6", Takes::Nothing),
    ("stateless-rpc", Takes::Nothing),
];

/// A long option as git reads it: the full name, whether it was negated with `--no-`, and what
/// it takes. `None` when git does not know it or the prefix fits more than one option (git
/// refuses both, but the guard's list may be older than the git that runs).
fn push_long(name: &str) -> Option<(&'static str, bool, Takes)> {
    let mut found: Vec<(&'static str, bool, Takes)> = Vec::new();
    for (long, takes) in PUSH_LONG {
        if long == name {
            return Some((long, false, takes));
        }
        if long.starts_with(name) {
            found.push((long, false, takes));
        }
        if let Some(rest) = name.strip_prefix("no-") {
            if long == rest {
                return Some((long, true, Takes::Nothing));
            }
            if long.starts_with(rest) {
                found.push((long, true, Takes::Nothing));
            }
        }
    }
    match found[..] {
        [one] => Some(one),
        _ => None,
    }
}

/// What a `git push` or `git send-pack` command line names.
#[derive(Debug, Default)]
struct PushArgs {
    /// The remote and refspecs, in order.
    positional: Vec<String>,
    /// `--repo=<remote>`, used when no remote is given as an operand.
    repo: Option<String>,
}

fn push_args(args: &[&String]) -> Result<PushArgs, &'static str> {
    const ALL: &str = "git push --all, --branches or --mirror names no single branch";
    const UNKNOWN: &str =
        "a git push option the guard does not know, or an abbreviation of more than one";
    const RUNS: &str = "git push --receive-pack or --exec runs a program the guard does not read";
    let mut out = PushArgs::default();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;
        if a == "--" {
            out.positional
                .extend(args[i..].iter().map(|s| s.to_string()));
            break;
        }
        if let Some(l) = a.strip_prefix("--") {
            let (name, glued) = match l.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (l, None),
            };
            let (long, negated, takes) = push_long(name).ok_or(UNKNOWN)?;
            if negated {
                if glued.is_some() {
                    return Err(UNKNOWN);
                }
                continue;
            }
            let value = match (takes, glued) {
                (Takes::Nothing, Some(_)) => return Err(UNKNOWN),
                (Takes::Value, None) => {
                    i += 1;
                    Some(args.get(i - 1).ok_or(UNKNOWN)?.to_string())
                }
                (_, v) => v.map(str::to_owned),
            };
            match long {
                "all" | "branches" | "mirror" => return Err(ALL),
                "receive-pack" | "exec" => return Err(RUNS),
                "repo" => out.repo = value,
                _ => {}
            }
        } else if a.len() > 1 && a.starts_with('-') {
            // A cluster of one-letter flags; `-o` takes the rest of the cluster or the next word.
            for (k, f) in a[1..].char_indices() {
                match f {
                    'v' | 'q' | 'd' | 'n' | 'f' | 'u' | '4' | '6' => {}
                    'o' => {
                        if a[k + 2..].is_empty() {
                            args.get(i).ok_or(UNKNOWN)?;
                            i += 1;
                        }
                        break;
                    }
                    _ => return Err(UNKNOWN),
                }
            }
        } else {
            out.positional.push(a.to_owned());
        }
    }
    Ok(out)
}

/// `git pull --ff-only` makes no new commit, unless a later option turns merging or rebasing
/// back on. Any option that could (`--ff`, `--no-ff`, `--rebase`, `-r`, or an abbreviation of
/// one) means the pull is judged.
fn pull_is_ff_only(args: &[&String]) -> bool {
    args.iter().any(|a| a.as_str() == "--ff-only")
        && !args.iter().any(|a| {
            let a = a.as_str();
            a != "--ff-only"
                && (a.starts_with("--f")
                    || a.starts_with("--no-f")
                    || a.starts_with("--r")
                    || a.starts_with("--no-r")
                    || (a.starts_with('-') && !a.starts_with("--") && a.contains('r')))
        })
}

/// Branch switches seen so far on the line (`git checkout <b>`, `git switch -c <b>`, ...). A
/// later command acts on the folder's own branch **or** any branch switched to before it: the
/// reader does not keep the separators, and a switch after `;` or `||` can fail and leave the old
/// branch checked out, so every one stays a candidate. Switches are line-wide, not per folder,
/// which can only add candidates.
#[derive(Clone, Debug, Default)]
struct Switched {
    branches: Vec<String>,
    /// A switch whose target can't be read (`-`, `@{-1}`, a computed name): the current branch is
    /// unknown from then on, and the guard fails closed on anything that needs it.
    unknown: bool,
}

impl Switched {
    /// Every branch `cwd` could be on, newline-joined; `None` when that can't be known.
    fn current(&self, facts: &dyn RepoFacts, cwd: &Path) -> Option<String> {
        if self.unknown {
            return None;
        }
        let mut all = vec![facts.current_branch(cwd)?];
        all.extend(self.branches.iter().cloned());
        Some(all.join("\n"))
    }

    fn to(&mut self, name: &str) {
        if name.is_empty() || name == "-" || name.contains('$') || name.contains("@{") {
            self.unknown = true;
            return;
        }
        // `main~2` and `main^0` detach at main; keep main as the candidate.
        let base = name.split(['~', '^']).next().unwrap_or(name);
        let base = base.trim_start_matches("refs/heads/");
        let base = base.strip_prefix("refs/remotes/").unwrap_or(base);
        self.branches.push(base.to_owned());
        // `origin/main` checks out a local `main` with `--track` or by guessing.
        if let Some((_, tail)) = base.split_once('/') {
            self.branches.push(tail.to_owned());
        }
    }

    /// Record what a `git` verb does to the current branch, if anything.
    fn record(&mut self, verb: &str, args: &[&String]) {
        let positional = |skip_value: &[&str]| {
            let mut out = Vec::new();
            let mut i = 0;
            while i < args.len() {
                let a = args[i].as_str();
                if skip_value.contains(&a) {
                    i += 1;
                } else if a == "-" || !a.starts_with('-') {
                    out.push(a);
                }
                i += 1;
            }
            out
        };
        match verb {
            "checkout" | "switch" => {
                // The flags that create the branch they name: separate (`-b x`), glued (`-bx`,
                // `-fbx`) or long with `=`.
                let (short, long): (&[char], &[&str]) = if verb == "checkout" {
                    (&['b', 'B'], &["--orphan"])
                } else {
                    (&['c', 'C'], &["--create", "--force-create", "--orphan"])
                };
                let mut i = 0;
                let mut before = Vec::new();
                let mut after_dashdash = false;
                let mut pathspec_file = false;
                while i < args.len() {
                    let a = args[i].as_str();
                    i += 1;
                    if after_dashdash {
                        return; // `git checkout <tree> -- <paths>` restores files
                    }
                    if a == "--" {
                        after_dashdash = true;
                    } else if let Some(l) = a.strip_prefix("--") {
                        let (name, glued) = match l.split_once('=') {
                            Some((n, v)) => (n, Some(v)),
                            None => (l, None),
                        };
                        let name = format!("--{name}");
                        if long.contains(&name.as_str()) {
                            match glued
                                .map(str::to_owned)
                                .or_else(|| args.get(i).map(|s| s.to_string()))
                            {
                                Some(b) => self.to(&b),
                                None => self.unknown = true,
                            }
                            return;
                        }
                        if name == "--conflict" && glued.is_none() {
                            i += 1;
                        }
                        pathspec_file |= name == "--pathspec-from-file";
                    } else if a.len() > 1 && a.starts_with('-') {
                        if let Some(k) = a[1..].find(|c| short.contains(&c)) {
                            let rest = &a[k + 2..];
                            match (!rest.is_empty())
                                .then(|| rest.to_owned())
                                .or_else(|| args.get(i).map(|s| s.to_string()))
                            {
                                Some(b) => self.to(&b),
                                None => self.unknown = true,
                            }
                            return;
                        }
                    } else {
                        before.push(a);
                    }
                }
                if pathspec_file {
                    return;
                }
                // One operand is the branch. More than one is a file restore, unless an option
                // the reader does not know took a value; every operand stays a candidate.
                for b in before {
                    self.to(b);
                }
            }
            "branch" => {
                let renames = args.iter().any(|a| {
                    a.as_str() == "--move"
                        || (a.starts_with('-') && !a.starts_with("--") && a.contains(['m', 'M']))
                });
                if renames {
                    // `git branch -m [<old>] <new>`: the current branch may now be <new>.
                    match positional(&[]).last() {
                        Some(b) => self.to(b),
                        None => self.unknown = true,
                    }
                }
            }
            "symbolic-ref" => {
                let ops = positional(&["-m"]);
                if let Some(k) = ops.iter().position(|a| a.eq_ignore_ascii_case("HEAD")) {
                    if let Some(b) = ops.get(k + 1) {
                        self.to(b);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Lines where a command can run after a switch written later: a loop body runs again, a
/// function runs when it is called, and `&` puts a command in the background. On these the guard
/// collects every switch on the line first.
fn reorders(cmd: &str) -> bool {
    let background = cmd
        .replace("&&", "")
        .replace(">&", "")
        .replace("<&", "")
        .replace("&>", "")
        .contains('&');
    let keyword = cmd
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|w| matches!(w, "for" | "while" | "until" | "select" | "function"));
    let function = cmd
        .match_indices('(')
        .any(|(i, _)| cmd[i + 1..].trim_start().starts_with(')'));
    background || keyword || function
}

fn git_shape(words: &[String], here: &Path, facts: &dyn RepoFacts, sw: &mut Switched) -> Shape {
    // `-C` applies to this command only; it never carries to the rest of the line.
    let mut owned = here.to_path_buf();
    let cwd = &mut owned;
    let mut i = 1;
    while i < words.len() {
        let w = &words[i];
        // A computed global option (`-c "$K"`, `-C "$D"`) could point the push anywhere.
        if w.contains('$')
            || words.get(i + 1).is_some_and(|v| {
                v.contains('$') && matches!(w.as_str(), "-c" | "-C" | "--config-env")
            })
        {
            return Shape::Unreadable("git with a computed option before its verb");
        }
        match w.as_str() {
            "-C" => {
                let Some(d) = words.get(i + 1) else {
                    return Shape::Unreadable("git -C without a folder");
                };
                *cwd = cwd.join(d);
                i += 2;
            }
            s if s == "-c" || s == "--config-env" || s.starts_with("--config-env=") => {
                // `-c <key>=<value>` and `--config-env[=]<key>=<variable>` both set config.
                let (setting, step) = match s.strip_prefix("--config-env=") {
                    Some(v) => (Some(v), 1),
                    None => (words.get(i + 1).map(String::as_str), 2),
                };
                let key = setting
                    .map(|k| k.split('=').next().unwrap_or("").to_ascii_lowercase())
                    .unwrap_or_default();
                if ["alias.", "remote.", "push.", "branch.", "url.", "include"]
                    .iter()
                    .any(|p| key.starts_with(p))
                {
                    return Shape::Unreadable("git -c changes how pushes resolve");
                }
                i += step;
            }
            s if s.starts_with("--git-dir")
                || s.starts_with("--work-tree")
                || s == "--exec-path" =>
            {
                return Shape::Unreadable("git with its own --git-dir or --work-tree");
            }
            s if s.starts_with('-') => i += 1,
            _ => break,
        }
    }
    let Some(verb) = words.get(i) else {
        return Shape::None;
    };
    if verb.contains('$') {
        return Shape::Unreadable("git with a computed verb");
    }
    // A glob (`p[u]sh`, `HEAD:ma?n`) becomes whatever file names it matches.
    if verb.contains(GLOB) {
        return Shape::Unreadable("git with a pattern for its verb");
    }
    if matches!(verb.as_str(), "push" | "send-pack" | "merge" | "pull")
        && words[i + 1..]
            .iter()
            .any(|w| w.contains('$') || w.contains(['?', '[']))
    {
        return Shape::Unreadable("a push, merge or pull with a computed argument");
    }
    let args: Vec<&String> = words[i + 1..].iter().collect();
    let repo = facts.repo(cwd);
    match verb.as_str() {
        "push" | "send-pack" => {
            // The branch this folder is on, after any switch earlier on the line.
            let current = sw.current(facts, cwd);
            let parsed = match push_args(&args) {
                Ok(p) => p,
                Err(why) => return Shape::Unreadable(why),
            };
            let positional = &parsed.positional;
            let refspecs = positional.get(1..).unwrap_or(&[]);
            let repo = match positional.first().or(parsed.repo.as_ref()) {
                Some(remote) if remote.as_str() != "origin" => facts.remote_repo(cwd, remote),
                _ => repo,
            };
            if refspecs.is_empty() {
                return Shape::Into {
                    repo,
                    branch: current,
                    at: cwd.clone(),
                };
            }
            // The first refspec that lands on a protected branch decides; the caller checks each.
            let mut dsts = Vec::new();
            for r in refspecs {
                // A pattern (`refs/heads/*:refs/heads/*`) or `:` (every matching branch) names
                // no single branch, and an empty destination is not one git spells out.
                if r.contains('*') || r.trim_start_matches('+') == ":" {
                    return Shape::Unreadable("a refspec that names no single branch");
                }
                match r.split_once(':') {
                    Some((_, "")) => {
                        return Shape::Unreadable("a refspec with an empty destination");
                    }
                    Some((_, dst)) => dsts.push(strip_ref(dst)),
                    None => {
                        let src = strip_ref(r);
                        // `HEAD`, `+HEAD` and `@` push the current branch to its own name.
                        if src.eq_ignore_ascii_case("HEAD") || src == "@" {
                            let Some(c) = &current else {
                                return Shape::Into {
                                    repo,
                                    branch: None,
                                    at: cwd.clone(),
                                };
                            };
                            dsts.push(c.clone());
                        } else {
                            dsts.push(src);
                        }
                    }
                }
            }
            Shape::Into {
                repo,
                branch: Some(dsts.join("\n")),
                at: cwd.clone(),
            }
        }
        "merge" | "pull" => {
            if verb == "pull" && pull_is_ff_only(&args) {
                return Shape::None;
            }
            if verb == "merge"
                && args
                    .iter()
                    .any(|a| matches!(a.as_str(), "--abort" | "--continue" | "--quit"))
            {
                return Shape::None;
            }
            Shape::Into {
                repo,
                branch: sw.current(facts, cwd),
                at: cwd.clone(),
            }
        }
        v @ ("checkout" | "switch" | "branch" | "symbolic-ref") => {
            sw.record(v, &args);
            Shape::None
        }
        _ => Shape::None,
    }
}

fn gh_repo_flag(args: &[&String]) -> Option<String> {
    let joined = args.iter().find_map(|a| {
        a.strip_prefix("--repo=")
            .or_else(|| a.strip_prefix("-R="))
            .map(str::to_owned)
    });
    let spaced = args
        .windows(2)
        .find(|w| w[0].as_str() == "-R" || w[0].as_str() == "--repo")
        .map(|w| w[1].to_string());
    joined.or(spaced).map(|r| {
        let r = r.trim_start_matches("https://").to_ascii_lowercase();
        if r.matches('/').count() == 1 {
            format!("github.com/{r}")
        } else {
            r
        }
    })
}

fn gh_shape(
    words: &[String],
    cwd: &Path,
    facts: &dyn RepoFacts,
    flip_label: Option<&str>,
) -> Shape {
    let args: Vec<&String> = words[1..].iter().collect();
    let repo = gh_repo_flag(&args).or_else(|| facts.repo(cwd));
    let first = args.first().map(|s| s.as_str());
    let second = args.get(1).map(|s| s.as_str());
    let computed = |w: &str| w.contains('$') || w.contains(GLOB);
    if first.is_some_and(computed) || second.is_some_and(computed) {
        return Shape::Unreadable("gh with a computed subcommand");
    }
    let landing_sub = matches!(
        (first, second),
        (Some("pr"), Some("merge" | "ready" | "edit" | "create"))
            | (Some("api"), _)
            | (Some("repo"), Some("sync"))
    );
    if landing_sub && args.iter().any(|w| w.contains('$')) {
        return Shape::Unreadable("a gh landing command with a computed argument");
    }
    let pr_arg = || {
        args.get(2)
            .filter(|a| !a.starts_with('-'))
            .map(|s| s.to_string())
    };
    match (first, second) {
        (Some("pr"), Some("merge")) => Shape::GitHub {
            repo,
            pr: pr_arg(),
            what: "a PR merge",
        },
        (Some("pr"), Some("ready")) => Shape::GitHub {
            repo,
            pr: pr_arg(),
            what: "a ready flip",
        },
        (Some("pr"), Some("create"))
            if !args
                .iter()
                .any(|a| a.as_str() == "--draft" || a.as_str() == "-d") =>
        {
            Shape::NonDraftPr { repo }
        }
        (Some("pr"), Some("edit")) => {
            let removes = args.windows(2).any(|w| {
                w[0].as_str() == "--remove-label"
                    && flip_label.is_some_and(|l| w[1].split(',').any(|x| x == l))
            });
            if removes {
                Shape::GitHub {
                    repo,
                    pr: pr_arg(),
                    what: "removing the review label",
                }
            } else {
                Shape::None
            }
        }
        (Some("repo"), Some("sync")) => Shape::GitHub {
            repo,
            pr: None,
            what: "a repo sync",
        },
        (Some("api"), _) => api_shape(&args[1..], cwd, facts),
        _ => Shape::None,
    }
}

/// `gh api`, read the way gh reads its flags: `-X PUT`, `-XPUT`, `-X=PUT`, `-iXPUT`,
/// `--method PUT` and `--method=PUT` all set the method, and fields or `--input` without a method
/// make it a POST. The repository is the one the endpoint path names, not the folder's.
fn api_shape(args: &[&String], cwd: &Path, facts: &dyn RepoFacts) -> Shape {
    let mut method: Option<String> = None;
    let mut fields: Vec<String> = Vec::new();
    let mut input = false;
    let mut hostname: Option<String> = None;
    let mut endpoints: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;
        let (flag, glued): (&str, Option<&str>) = if a == "--" {
            endpoints.extend(args[i..].iter().map(|s| s.as_str()));
            break;
        } else if let Some(long) = a.strip_prefix("--") {
            match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            }
        } else if a.len() > 1 && a.starts_with('-') {
            // A cluster of one-letter flags: the first that takes a value takes the rest.
            let short = &a[1..];
            let Some(k) = short.find(|c: char| "XHfFqtp".contains(c)) else {
                continue;
            };
            let flag = match &short[k..=k] {
                "X" => "method",
                "H" => "header",
                "f" => "raw-field",
                "F" => "field",
                "q" => "jq",
                "t" => "template",
                _ => "preview",
            };
            let rest = &short[k + 1..];
            let rest = rest.strip_prefix('=').unwrap_or(rest);
            (flag, (!rest.is_empty()).then_some(rest))
        } else {
            endpoints.push(a);
            continue;
        };
        if !matches!(
            flag,
            "method"
                | "header"
                | "field"
                | "raw-field"
                | "input"
                | "jq"
                | "template"
                | "preview"
                | "cache"
                | "hostname"
        ) {
            continue;
        }
        let value = match glued {
            Some(v) => v.to_owned(),
            None => {
                i += 1;
                args.get(i - 1).map(|s| s.to_string()).unwrap_or_default()
            }
        };
        match flag {
            "method" => method = Some(value.to_ascii_uppercase()),
            "field" | "raw-field" => fields.push(value),
            "input" => input = true,
            "hostname" => hostname = Some(value.to_ascii_lowercase()),
            _ => {}
        }
    }
    let writes = match method.as_deref() {
        Some(m) => !matches!(m, "GET" | "HEAD"),
        None => input || !fields.is_empty(),
    };
    let [endpoint] = endpoints[..] else {
        return if writes && endpoints.len() > 1 {
            Shape::Unreadable("gh api with more than one endpoint")
        } else {
            Shape::None
        };
    };
    let endpoint = endpoint.to_ascii_lowercase();
    let (host, path) = match endpoint
        .strip_prefix("https://")
        .or_else(|| endpoint.strip_prefix("http://"))
    {
        Some(url) => {
            let (h, p) = url.split_once('/').unwrap_or((url, ""));
            if h == "api.github.com" {
                ("github.com".to_owned(), p)
            } else {
                (h.to_owned(), p.strip_prefix("api/v3/").unwrap_or(p))
            }
        }
        None => (
            hostname.unwrap_or_else(|| "github.com".into()),
            endpoint.trim_start_matches('/'),
        ),
    };
    let path = path.split(['?', '#']).next().unwrap_or(path);
    if path == "graphql" || path.ends_with("/graphql") {
        // A mutation can name any repository by node id, so a write here fails closed. A
        // query cannot write.
        let mutation = input
            || fields
                .iter()
                .any(|f| f.to_ascii_lowercase().contains("mutation") || f.contains("=@"));
        return if mutation {
            Shape::GitHub {
                repo: None,
                pr: None,
                what: "a GitHub GraphQL mutation",
            }
        } else {
            Shape::None
        };
    }
    let landing = path.contains(['%', '\\'])
        || ["/merge", "/merges", "/git/refs", "/contents/", "/pulls"]
            .iter()
            .any(|p| path.contains(p));
    if !(writes && landing) {
        return Shape::None;
    }
    Shape::GitHub {
        repo: path_repo(&host, path, cwd, facts),
        pr: None,
        what: "a GitHub API write",
    }
}

/// `host/owner/name` from a `repos/<owner>/<name>/...` API path. `{owner}` and `{repo}` are the
/// folder's repository, as gh fills them in. Any other path names no repository, and neither does
/// one the server could read differently: a `.` or `..` segment, a `%` or `\` anywhere (a server
/// may decode `%2e%2e` or `..%2f` into a segment that climbs out of the named repository), an
/// owner or name holding any other character a GitHub name can't, or a host with a port, user or
/// trailing dot.
fn path_repo(host: &str, path: &str, cwd: &Path, facts: &dyn RepoFacts) -> Option<String> {
    let plain_host = !host.is_empty()
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'));
    if !plain_host || path.contains(['%', '\\']) || path.split('/').any(|s| s == "." || s == "..") {
        return None;
    }
    let mut parts = path.split('/');
    if parts.next() != Some("repos") {
        return None;
    }
    let (owner, name) = (parts.next()?, parts.next()?);
    let plain = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    };
    let (owner_here, name_here) = (owner == "{owner}", name == "{repo}");
    if !(owner_here || plain(owner)) || !(name_here || plain(name)) {
        return None;
    }
    if owner_here || name_here {
        let here = facts.repo(cwd)?;
        let mut it = here.rsplitn(3, '/');
        let (n, o, h) = (it.next()?, it.next()?, it.next()?);
        let owner = if owner_here { o } else { owner };
        let name = if name_here { n } else { name };
        return Some(format!("{h}/{owner}/{name}"));
    }
    Some(format!("{host}/{owner}/{name}"))
}

fn simple_shape(
    c: &Simple,
    cwd: &mut PathBuf,
    ctx: &Ctx,
    facts: &dyn RepoFacts,
    sw: &mut Switched,
) -> Shape {
    for (k, _) in &c.assigns {
        if k.starts_with("GIT_CONFIG_KEY_") || k.starts_with("GIT_CONFIG_VALUE_") {
            return Shape::Unreadable("it sets a variable that changes where git or gh acts");
        }
        if matches!(
            k.as_str(),
            "GIT_DIR"
                | "GIT_WORK_TREE"
                | "HOME"
                | "CDPATH"
                | "GH_REPO"
                | "GH_HOST"
                | "GIT_CONFIG_GLOBAL"
                | "GIT_CONFIG_PARAMETERS"
                | "GIT_CONFIG_COUNT"
        ) {
            return Shape::Unreadable("it sets a variable that changes where git or gh acts");
        }
    }
    // Look through shell keywords and command wrappers to the command they run.
    let mut start = 0;
    while let Some(w) = c.words.get(start) {
        match w.as_str() {
            "if" | "then" | "else" | "elif" | "while" | "until" | "do" | "!" | "time"
            | "command" | "exec" | "nohup" | "builtin" | "caffeinate" => start += 1,
            "env" => {
                start += 1;
                while let Some(a) = c.words.get(start) {
                    if a.starts_with('-') || a.contains('=') {
                        if a.split('=').next().is_some_and(|k| {
                            matches!(
                                k,
                                "GIT_DIR"
                                    | "GIT_WORK_TREE"
                                    | "HOME"
                                    | "GH_REPO"
                                    | "GH_HOST"
                                    | "CDPATH"
                            )
                        }) {
                            return Shape::Unreadable(
                                "it sets a variable that changes where git or gh acts",
                            );
                        }
                        start += 1;
                    } else {
                        break;
                    }
                }
            }
            "timeout" | "nice" | "ionice" | "stdbuf" => {
                start += 1;
                while c.words.get(start).is_some_and(|a| {
                    a.starts_with('-')
                        || a.bytes()
                            .all(|b| b.is_ascii_digit() || b == b'.' || b == b's' || b == b'm')
                }) {
                    start += 1;
                }
            }
            _ => break,
        }
    }
    let inner;
    let c = if start > 0 {
        inner = Simple {
            assigns: c.assigns.clone(),
            words: c.words[start..].to_vec(),
            computed: c.computed,
            subs: Vec::new(),
        };
        &inner
    } else {
        c
    };
    let Some(cmd) = c.words.first() else {
        return Shape::None;
    };
    // A computed command name could be git or gh.
    if cmd.contains('$') || (cmd.contains(GLOB) && cmd != "[" && cmd != "[[") {
        return Shape::Unreadable("the command's name is computed");
    }
    // git, gh, curl or hub as an operand of some other command (a wrapper option the skip above
    // did not know, `xargs`, `watch`, ...) is read as that command: here, unreadable.
    let base = |w: &str| {
        w.rsplit(['/', '\\'])
            .next()
            .unwrap_or(w)
            .trim_end_matches(".exe")
            .to_ascii_lowercase()
    };
    if !matches!(base(cmd).as_str(), "git" | "gh" | "curl" | "hub")
        && c.words[1..]
            .iter()
            .any(|w| matches!(base(w).as_str(), "git" | "gh" | "curl" | "hub"))
    {
        return Shape::Unreadable("it runs git, gh or curl through another command");
    }
    let name = cmd
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(cmd)
        .trim_end_matches(".exe")
        .to_ascii_lowercase();
    // git and gh judge their own computed words where they matter (the verb, a push or merge
    // target, a PR); a computed folder or curl request is unreadable outright.
    if c.computed && matches!(name.as_str(), "cd" | "pushd" | "curl") {
        return Shape::Unreadable("it uses a computed word");
    }
    match name.as_str() {
        "cd" | "pushd" => {
            match c.words.get(1) {
                Some(d) => *cwd = cwd.join(d),
                None => return Shape::Unreadable("cd with no folder"),
            }
            Shape::None
        }
        "git" => git_shape(&c.words, cwd, facts, sw),
        "gh" => gh_shape(&c.words, cwd, facts, ctx.flip_label.as_deref()),
        "hub" => Shape::Unreadable("hub is not read"),
        "curl" => {
            let text = c.words.join(" ");
            let writes =
                c.words.iter().any(|w| {
                    matches!(w.as_str(), "-d" | "--data" | "--data-raw" | "-F" | "--form")
                }) || c.words.windows(2).any(|w| {
                    (w[0] == "-X" || w[0] == "--request") && !w[1].eq_ignore_ascii_case("GET")
                });
            if writes && text.contains("api.github.com") {
                Shape::GitHub {
                    repo: None,
                    pr: None,
                    what: "a GitHub API write",
                }
            } else {
                Shape::None
            }
        }
        "eval" | "trap" | "source" | "." | "fish" | "csh" | "tcsh" | "su" | "runuser" | "sudo"
        | "xargs" => Shape::Unreadable("it runs text the guard cannot read"),
        "sh" | "bash" | "zsh" | "dash" | "pwsh" | "powershell" | "cmd" | "python" | "python3"
        | "node" | "perl" | "ruby" => {
            if c.words.iter().any(|w| {
                matches!(
                    w.as_str(),
                    "-c" | "-e" | "-Command" | "/c" | "-EncodedCommand"
                )
            }) {
                Shape::Unreadable("inline interpreter code is not read")
            } else {
                Shape::None
            }
        }
        _ => Shape::None,
    }
}

fn is_protected(ctx: &Ctx, facts: &dyn RepoFacts, cwd: &Path, branch: &str) -> bool {
    ctx.protected.iter().any(|p| p == branch)
        || facts.default_branch(cwd).as_deref() == Some(branch)
}

/// Judge one Bash command.
pub fn judge(cmd: &str, ctx: &Ctx, facts: &dyn RepoFacts) -> Verdict {
    let at_stake = ctx.task_active || !ctx.tracked.is_empty();
    let deny = |tier, rule, why: String| Verdict::Deny { tier, rule, why };
    if cmd.len() > MAX_COMMAND {
        return if at_stake {
            deny(
                Tier::Quote,
                "fail_closed",
                "The command is too large to check.".into(),
            )
        } else {
            Verdict::Allow
        };
    }
    if !mentions_landing(cmd) {
        return Verdict::Allow;
    }
    // Brace expansion, comments and quoting are the reader's: it reports a line that uses brace
    // expansion anywhere, substitutions included, as unreadable.
    let mut sw = Switched::default();
    if reorders(cmd) {
        // A first pass only collects the switches; judging with all of them can only deny more.
        let _ = judge_in(cmd, ctx, facts, &mut sw);
    }
    judge_in(cmd, ctx, facts, &mut sw)
}

/// [`judge`] after its size check and pre-filter. Substitutions recurse here with the same
/// switches, since a branch switch inside `$(…)` still moves the folder's branch.
fn judge_in(cmd: &str, ctx: &Ctx, facts: &dyn RepoFacts, sw: &mut Switched) -> Verdict {
    let at_stake = ctx.task_active || !ctx.tracked.is_empty();
    let deny = |tier, rule, why: String| Verdict::Deny { tier, rule, why };
    let cmds = match shell::parse(cmd) {
        Parsed::Commands(c) => c,
        Parsed::Unreadable(why) => {
            return if at_stake {
                deny(
                    Tier::Quote,
                    "fail_closed",
                    format!("The command can't be read ({why})."),
                )
            } else {
                Verdict::Allow
            };
        }
    };
    let mut cwd = ctx.cwd.clone();
    // GitHub names are case-insensitive, and an API path or `--repo` may be written either way.
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    let has_tracked = |repo: &str| ctx.tracked.iter().any(|t| same(&t.repo, repo));
    let mut reason_deny: Option<Verdict> = None;
    for c in &cmds {
        // What a substitution runs is judged as a command of its own.
        for sub in &c.subs {
            let here = Ctx {
                cwd: cwd.clone(),
                ..ctx.clone()
            };
            match judge_in(sub, &here, facts, sw) {
                Verdict::Allow => {}
                v @ Verdict::Deny {
                    tier: Tier::Quote, ..
                } => return v,
                v => {
                    reason_deny.get_or_insert(v);
                }
            }
        }
        let shape = simple_shape(c, &mut cwd, ctx, facts, sw);
        let (repo, landing, what): (Option<String>, bool, String) = match shape {
            Shape::None => continue,
            Shape::Unreadable(why) => {
                if at_stake {
                    return deny(
                        Tier::Quote,
                        "fail_closed",
                        format!("Part of the command can't be checked: {why}."),
                    );
                }
                continue;
            }
            Shape::Into { repo, branch, at } => {
                let Some(b) = branch else {
                    if at_stake {
                        return deny(
                            Tier::Quote,
                            "fail_closed",
                            "The branch this would land on could not be resolved.".into(),
                        );
                    }
                    continue;
                };
                let target = b
                    .lines()
                    .find(|x| is_protected(ctx, facts, &at, x))
                    .map(str::to_owned);
                match target {
                    Some(t) => (repo, true, format!("landing on the protected branch {t}")),
                    None => continue,
                }
            }
            Shape::GitHub { repo, pr, what } => {
                // A literal PR that no tracked change holds, in a repo whose tracked changes all
                // have PRs, is provably untracked: the reason tier is enough.
                if let (Some(r), Some(p)) = (&repo, &pr) {
                    let n = p.trim_start_matches('#');
                    let literal =
                        n.len() <= 9 && n.bytes().all(|b| b.is_ascii_digit()) && !n.is_empty();
                    let in_repo: Vec<&Tracked> =
                        ctx.tracked.iter().filter(|t| same(&t.repo, r)).collect();
                    let held = in_repo.iter().any(|t| {
                        t.pr.as_deref()
                            .is_some_and(|x| x.ends_with(&format!("/pull/{n}")))
                    });
                    if literal
                        && !held
                        && in_repo.iter().all(|t| t.pr.is_some())
                        && (ctx.task_active || !in_repo.is_empty())
                    {
                        // Keep judging: a quote-tier landing later in the line wins.
                        reason_deny.get_or_insert(deny(
                            Tier::Reason,
                            "untracked",
                            format!("{what} of untracked PR #{n}"),
                        ));
                        continue;
                    }
                }
                (repo, true, what.to_owned())
            }
            Shape::NonDraftPr { repo } => match &repo {
                Some(r) if ctx.draft_repos.iter().any(|d| same(d, r)) && at_stake => {
                    return deny(
                        Tier::Quote,
                        "draft_rule",
                        format!("{r} lands by the draft rule; open PRs there with --draft."),
                    );
                }
                _ => continue,
            },
        };
        if !landing {
            continue;
        }
        let Some(repo) = repo else {
            if at_stake {
                return deny(
                    Tier::Quote,
                    "fail_closed",
                    format!("{what}, in a repository the guard could not name"),
                );
            }
            continue;
        };
        if ctx.task_active && ctx.task_repos.iter().any(|t| same(t, &repo)) {
            return deny(Tier::Quote, "task_repo", format!("{what} in {repo}, a repository of the active task: Helm lands this task's changes itself"));
        }
        if has_tracked(&repo) {
            return deny(
                Tier::Quote,
                "containment",
                format!(
                    "{what} in {repo}, which has tracked work: Helm lands tracked changes itself"
                ),
            );
        }
    }
    reason_deny.unwrap_or(Verdict::Allow)
}

/// The guard's context from the host's own records: the chat's active task and its repos, the
/// tracked set across every chat, and the landing settings.
pub fn context(
    tx: &rusqlite::Transaction<'_>,
    chat_id: &str,
    cwd: &Path,
    s: &crate::types::WorkflowSettings,
) -> rusqlite::Result<Ctx> {
    let active = crate::store::active_run(tx, chat_id)?;
    let task_repos = match &active {
        Some(r) => crate::store::changes(tx, &r.id)?
            .into_iter()
            .map(|c| c.repo)
            .collect(),
        None => Vec::new(),
    };
    let mut st = tx.prepare(
        "SELECT c.repo, c.branch, c.pr FROM workflow_changes c JOIN workflow_runs r ON r.id = c.run_id
         WHERE r.status IN ('active', 'parked', 'handed_off')
           AND c.state NOT IN ('landed', 'flipped', 'dropped')",
    )?;
    let tracked = st
        .query_map([], |r| {
            Ok(Tracked {
                repo: r.get(0)?,
                branch: r.get(1)?,
                pr: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Ctx {
        cwd: cwd.to_path_buf(),
        task_active: active.is_some(),
        task_repos,
        tracked,
        protected: s.landing.protected_branches.clone(),
        draft_repos: s
            .landing
            .repos
            .iter()
            .filter(|r| r.mode == crate::types::LandMode::Draft)
            .map(|r| r.repo.clone())
            .collect(),
        flip_label: s.landing.flip_label.clone(),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub struct Facts {
        pub repo: Option<&'static str>,
        pub branch: Option<&'static str>,
    }

    impl RepoFacts for Facts {
        fn repo(&self, _cwd: &Path) -> Option<String> {
            self.repo.map(str::to_owned)
        }
        fn current_branch(&self, _cwd: &Path) -> Option<String> {
            self.branch.map(str::to_owned)
        }
        fn default_branch(&self, _cwd: &Path) -> Option<String> {
            Some("trunk".into())
        }
    }

    const F: Facts = Facts {
        repo: Some("github.com/o/r"),
        branch: Some("feat-x"),
    };

    fn ctx(task: bool) -> Ctx {
        Ctx {
            cwd: PathBuf::from("C:/w"),
            task_active: task,
            task_repos: if task {
                vec!["github.com/o/r".into()]
            } else {
                vec![]
            },
            tracked: vec![Tracked {
                repo: "github.com/o/r".into(),
                branch: "feat-x".into(),
                pr: Some("https://github.com/o/r/pull/7".into()),
            }],
            protected: vec!["main".into(), "master".into()],
            draft_repos: vec!["github.com/o/r".into()],
            flip_label: Some("needs-review".into()),
        }
    }

    fn rule(v: Verdict) -> &'static str {
        match v {
            Verdict::Allow => "allow",
            Verdict::Deny { rule, .. } => rule,
        }
    }

    #[test]
    fn pushes_to_feature_branches_pass_and_landings_are_refused() {
        let c = ctx(true);
        assert_eq!(judge("git push origin feat-x", &c, &F), Verdict::Allow);
        assert_eq!(judge("git status && git log", &c, &F), Verdict::Allow);
        assert_eq!(
            rule(judge("git push origin HEAD:main", &c, &F)),
            "task_repo"
        );
        assert_eq!(
            rule(judge(
                "git push origin +refs/heads/x:refs/heads/master",
                &c,
                &F
            )),
            "task_repo"
        );
        assert_eq!(
            rule(judge("git push origin trunk", &c, &F)),
            "task_repo",
            "the default branch is protected too"
        );
        assert_eq!(
            rule(judge("cd sub && gh pr merge 7 --squash", &c, &F)),
            "task_repo"
        );
        assert_eq!(rule(judge("gh pr ready", &c, &F)), "task_repo");
        // No task, but tracked work in the repo: containment.
        assert_eq!(rule(judge("gh pr merge 7", &ctx(false), &F)), "containment");
    }

    #[test]
    fn merges_and_pulls_count_only_into_a_protected_branch() {
        let c = ctx(true);
        assert_eq!(
            judge("git merge main", &c, &F),
            Verdict::Allow,
            "merging main into the feature branch"
        );
        let on_main = Facts {
            repo: F.repo,
            branch: Some("main"),
        };
        assert_eq!(rule(judge("git merge feat-x", &c, &on_main)), "task_repo");
        assert_eq!(judge("git pull --ff-only", &c, &on_main), Verdict::Allow);
    }

    #[test]
    fn unreadable_commands_fail_closed_only_where_work_is_at_stake() {
        let c = ctx(true);
        for cmd in [
            "git $(echo push) origin main",
            "GIT_DIR=/x git push",
            "bash -c 'git push origin main'",
            "eval \"git push\"",
            "git push 'origin",
            "hub merge",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "fail_closed", "{cmd}");
        }
        let free = Ctx {
            task_active: false,
            tracked: vec![],
            ..ctx(false)
        };
        assert_eq!(
            judge("bash -c 'git push origin main'", &free, &F),
            Verdict::Allow
        );
        let unknown = Facts {
            repo: Some("github.com/o/r"),
            branch: None,
        };
        assert_eq!(rule(judge("git push", &c, &unknown)), "fail_closed");
    }

    #[test]
    fn github_side_writes_draft_rule_and_untracked_prs() {
        let c = ctx(true);
        assert_eq!(
            rule(judge("gh api -X PUT repos/o/r/pulls/7/merge", &c, &F)),
            "task_repo"
        );
        assert_eq!(
            judge("gh api repos/o/r/pulls/7", &c, &F),
            Verdict::Allow,
            "a read"
        );
        assert_eq!(rule(judge("gh pr create --title x", &c, &F)), "draft_rule");
        assert_eq!(
            judge("gh pr create --draft --title x", &c, &F),
            Verdict::Allow
        );
        assert_eq!(
            rule(judge("gh pr edit 7 --remove-label needs-review", &c, &F)),
            "task_repo"
        );
        assert_eq!(judge("gh pr edit 7 --add-label x", &c, &F), Verdict::Allow);
        // PR 9 is held by nothing tracked, and every tracked change has a PR: reason tier.
        match judge("gh pr merge 9", &c, &F) {
            Verdict::Deny { tier, rule, .. } => {
                assert_eq!((tier, rule), (Tier::Reason, "untracked"))
            }
            v => panic!("{v:?}"),
        }
        assert_eq!(
            rule(judge(
                "curl -X POST https://api.github.com/repos/o/r/merges",
                &c,
                &F
            )),
            "fail_closed",
            "no repo named"
        );
    }

    #[test]
    fn rev_orch_4_bypasses_are_closed() {
        let c = ctx(true);
        // Quote wins when a reason-tier PR merge comes first on the line.
        assert_eq!(
            rule(judge("gh pr merge 9 && git push origin HEAD:main", &c, &F)),
            "task_repo"
        );
        for cmd in [
            "if git push origin HEAD:main; then echo ok; fi",
            "! git push origin HEAD:main",
            "env FOO=1 git push origin HEAD:main",
            "command git push origin main",
            "timeout 60 git push origin main",
            "nohup git push origin main",
            "time git push origin main",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "task_repo", "{cmd}");
        }
        for cmd in [
            "\"$GIT\" push origin main",
            "$(which git) push origin main",
            "git pu''sh origin main",
        ] {
            assert_ne!(judge(cmd, &c, &F), Verdict::Allow, "{cmd}");
        }
        // -C stays with its own command.
        assert_eq!(
            rule(judge(
                "git -C ../other log; git push origin HEAD:main",
                &c,
                &F
            )),
            "task_repo"
        );
        // A redirection glued to the refspec does not hide it.
        assert_eq!(
            rule(judge("git push origin HEAD:main>/dev/null", &c, &F)),
            "task_repo"
        );
        // A remote other than origin is not known, so it fails closed.
        assert_eq!(
            rule(judge("git push upstream HEAD:main", &c, &F)),
            "fail_closed"
        );
        assert_eq!(
            rule(judge("gh pr merge 7 --repo=o/r", &ctx(false), &F)),
            "containment"
        );
    }

    #[test]
    fn rev_orch_5_substitutions_and_wrapper_options_are_judged() {
        let c = ctx(true);
        for cmd in [
            "echo $(git push origin HEAD:main)",
            "echo `git push origin HEAD:main`",
            "echo \"$(gh pr merge 7)\"",
            "command -p git push origin main",
            "exec -a x git push origin main",
            "env -u FOO git push origin main",
            "timeout -s KILL 60 git push origin main",
            "stdbuf -o L git push origin main",
            "echo >$(git push origin main)",
            "cat <(git push origin main)",
        ] {
            assert_ne!(judge(cmd, &c, &F), Verdict::Allow, "{cmd}");
        }
        // A harmless substitution is still fine.
        assert_eq!(
            judge(
                "git commit -m \"$(cat msg)\" && git push origin feat-x",
                &c,
                &F
            ),
            Verdict::Allow
        );
        // Nothing at stake: no work tracked and no task.
        let free = Ctx {
            task_active: false,
            tracked: vec![],
            ..ctx(false)
        };
        assert_eq!(
            judge("echo $(git push origin HEAD:main)", &free, &F),
            Verdict::Allow
        );
    }

    /// Facts that depend on the folder: only `C:/w/tracked` is the tracked repository.
    struct ByFolder;

    impl RepoFacts for ByFolder {
        fn repo(&self, cwd: &Path) -> Option<String> {
            let p = cwd.to_string_lossy().replace('\\', "/");
            Some(if p.ends_with("tracked") {
                "github.com/o/r".into()
            } else {
                "github.com/o/free".into()
            })
        }
        fn current_branch(&self, _cwd: &Path) -> Option<String> {
            Some("feat-x".into())
        }
        fn default_branch(&self, _cwd: &Path) -> Option<String> {
            Some("trunk".into())
        }
    }

    #[test]
    fn rev_orch_6_computed_options_and_folders_are_closed() {
        let c = ctx(false); // no task: only the tracked repository is at stake
        for cmd in [
            "git -c \"$K\" push origin",
            "git -C \"$D\" push origin trunk",
            "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=remote.origin.push GIT_CONFIG_VALUE_0=HEAD:main git push",
        ] {
            assert_ne!(judge(cmd, &c, &ByFolder), Verdict::Allow, "{cmd}");
        }
        // The substitution runs where the line has cd'd to.
        assert_eq!(
            judge("echo $(git push origin main)", &c, &ByFolder),
            Verdict::Allow,
            "an untracked repo"
        );
        assert_eq!(
            rule(judge(
                "cd tracked && echo $(git push origin main)",
                &c,
                &ByFolder
            )),
            "containment"
        );
    }

    #[test]
    fn a_branch_switch_earlier_on_the_line_is_the_current_branch() {
        let c = ctx(true);
        for cmd in [
            "git checkout main && git merge x && git push",
            "git checkout main && git push",
            "git checkout main; git push",
            "git checkout main || git push",
            "git checkout main | git push",
            "git checkout main\ngit push",
            "git checkout main && git push origin HEAD",
            "git checkout main && git push origin +HEAD",
            "git checkout main && git push origin @",
            "git checkout main && git merge x",
            "git checkout main && git pull",
            "git checkout -b main && git push",
            "git checkout -B main && git push -u origin HEAD",
            "git checkout -bmain && git push",
            "git checkout -fB main && git push",
            "git checkout --orphan=main && git push",
            "git checkout refs/heads/main && git push",
            "git checkout -t origin/main && git push",
            "git checkout main -- && git push",
            "git switch main && git merge x && git push",
            "git switch main; git push origin HEAD",
            "git switch main\ngit merge x",
            "git switch -c main && git push",
            "git switch -C main && git push",
            "git switch --create=main && git push",
            "git switch --force-create main && git push",
            "git switch -d main && git push origin HEAD",
            "git -C . checkout main && git push",
            "echo $(git checkout main) && git push",
            "git branch -M main && git push",
            "git symbolic-ref HEAD refs/heads/main && git push",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "task_repo", "{cmd}");
        }
        // A target the guard can't read fails closed.
        for cmd in [
            "git checkout - && git push",
            "git switch - && git push origin HEAD",
            "git checkout @{-1} && git merge x",
            "git checkout \"$B\" && git push",
            "git checkout $(cat branch) && git push",
            "git switch -c \"$B\" && git push",
            "git checkout -b && git push",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "fail_closed", "{cmd}");
        }
        // A command that switches nothing, or a switch to a feature branch, changes nothing.
        for cmd in [
            "git checkout feat-y && git push",
            "git push && git checkout main",
            "git checkout -- main && git push",
            "git checkout main -- src && git push",
            "git checkout -b feat-y main && git push",
            "git switch -c feat-y main && git push origin HEAD",
            "git branch -d old && git push",
            "git checkout main && git push origin feat-x",
        ] {
            assert_eq!(judge(cmd, &c, &F), Verdict::Allow, "{cmd}");
        }
        // A switch can fail and leave the old branch out: on main, both stay candidates.
        let on_main = Facts {
            repo: F.repo,
            branch: Some("main"),
        };
        assert_eq!(
            rule(judge("git checkout feat-y; git push", &c, &on_main)),
            "task_repo"
        );
        // Nothing at stake.
        let free = Ctx {
            task_active: false,
            tracked: vec![],
            ..ctx(false)
        };
        assert_eq!(
            judge("git checkout main && git push", &free, &F),
            Verdict::Allow
        );
    }

    #[test]
    fn a_switch_written_after_a_command_that_runs_again_or_later_still_counts() {
        let c = ctx(true);
        for cmd in [
            "while true; do git push; git checkout main; done",
            "for i in 1 2; do git push; git checkout main; done",
            "sleep 1 && git push & git checkout main",
            "f() { git push; }; git checkout main; f",
            "function f { git push; }; git checkout main; f",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "task_repo", "{cmd}");
        }
        assert_eq!(
            rule(judge("trap 'git push origin HEAD:main' EXIT", &c, &F)),
            "fail_closed"
        );
    }

    #[test]
    fn every_spelling_of_a_gh_api_write_is_a_write() {
        let c = ctx(true);
        for cmd in [
            "gh api -X PUT repos/o/r/pulls/7/merge",
            "gh api -XPUT repos/o/r/pulls/7/merge",
            "gh api -X=PUT repos/o/r/pulls/7/merge",
            "gh api -X put repos/o/r/pulls/7/merge",
            "gh api -iXPUT repos/o/r/pulls/7/merge",
            "gh api -iX PUT repos/o/r/pulls/7/merge",
            "gh api --method PUT repos/o/r/pulls/7/merge",
            "gh api --method=PUT repos/o/r/pulls/7/merge",
            "gh api --method=put repos/o/r/pulls/7/merge",
            "gh api repos/o/r/pulls/7/merge --method Put",
            "gh api -X DELETE repos/o/r/git/refs/heads/main",
            "gh api repos/o/r/merges -f base=main -f head=x",
            "gh api repos/o/r/merges -fbase=main",
            "gh api repos/o/r/merges -F base=main",
            "gh api repos/o/r/merges -Fbase=main",
            "gh api repos/o/r/merges --field base=main",
            "gh api repos/o/r/merges --field=base=main",
            "gh api repos/o/r/merges --raw-field base=main",
            "gh api repos/o/r/merges --raw-field=base=main",
            "gh api repos/o/r/merges --input body.json",
            "gh api repos/o/r/merges --input=body.json",
            "gh api -H 'Accept: application/vnd.github+json' -X PUT repos/o/r/pulls/7/merge",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "task_repo", "{cmd}");
        }
        for cmd in [
            "gh api repos/o/r/pulls/7",
            "gh api -X GET repos/o/r/pulls -f state=open",
            "gh api -XGET repos/o/r/pulls/7/merge",
            "gh api --method=get repos/o/r/pulls/7/merge",
            "gh api -H 'Accept: application/vnd.github+json' repos/o/r/pulls/7/merge",
            "gh api -X POST repos/o/r/issues/7/comments -f body=hi",
        ] {
            assert_eq!(judge(cmd, &c, &F), Verdict::Allow, "{cmd}");
        }
    }

    #[test]
    fn gh_api_judges_the_repository_its_path_names() {
        // No task. The line starts in C:/w, which is o/free; only o/r has tracked work.
        let c = ctx(false);
        for cmd in [
            "gh api -X PUT repos/o/r/pulls/7/merge",
            "gh api -X PUT /repos/o/r/pulls/7/merge",
            "gh api -X PUT repos/O/R/pulls/7/merge",
            "gh api -X PUT https://api.github.com/repos/o/r/pulls/7/merge",
            "gh api repos/o/r/merges -f base=main",
            "cd tracked && gh api -X PUT repos/{owner}/{repo}/pulls/7/merge",
            "cd tracked && gh api -X PUT repos/{owner}/r/pulls/7/merge",
        ] {
            assert_eq!(rule(judge(cmd, &c, &ByFolder)), "containment", "{cmd}");
        }
        // The path, not the folder, names the repository.
        for cmd in [
            "gh api -X PUT repos/{owner}/{repo}/pulls/7/merge",
            "cd tracked && gh api -X PUT repos/o/free/pulls/7/merge",
            "gh api graphql -f query='query { viewer { login } }'",
        ] {
            assert_eq!(judge(cmd, &c, &ByFolder), Verdict::Allow, "{cmd}");
        }
        for cmd in [
            "gh api -X PUT \"repos/$R/pulls/7/merge\"",
            "gh api -X PUT repositories/1/pulls/7/merge",
            "gh api -X PUT repos/{x}/r/pulls/7/merge",
            "gh api -X PUT repos/o/r/pulls/7/merge repos/o/free/x",
            "gh api -X PUT repos/o/%72/pulls/7/merge",
            "gh api -X PUT repos/%6f/r/pulls/7/merge",
            "gh api -X PUT repos/o/free/../r/pulls/7/merge",
            "gh api graphql -f query='mutation { mergePullRequest(input: {}) { clientMutationId } }'",
            "gh api graphql -F query=@merge.graphql",
        ] {
            assert_eq!(rule(judge(cmd, &c, &ByFolder)), "fail_closed", "{cmd}");
        }
        // Tracked names keep their case; a lower-cased --repo still matches them.
        let mixed = Ctx {
            tracked: vec![Tracked {
                repo: "github.com/O/R".into(),
                branch: "feat-x".into(),
                pr: None,
            }],
            ..ctx(false)
        };
        assert_eq!(
            rule(judge("gh pr merge 7 --repo=O/R", &mixed, &ByFolder)),
            "containment"
        );
    }

    #[test]
    fn brace_expansion_fails_closed() {
        let c = ctx(true);
        for cmd in [
            "git checkout {main,} && git push",
            "git push origin {HEAD:main,}",
            "git push origin HEAD:{main,feat-x}",
            "git push origin HEAD:ma{i..i}n",
            "gh api -X PUT {repos/o/r/pulls/7/merge,}",
            "git pu{sh,} origin HEAD:main",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "fail_closed", "{cmd}");
        }
        // Quoted braces, a group with no list, and lines with no landing word are not judged.
        for cmd in [
            "git commit -m \"a {b,c}\" && git push origin feat-x",
            "git commit -m 'a {b,c}' && git push origin feat-x",
            "f() { git push origin feat-x; }; f",
            "mkdir -p src/{a,b}",
        ] {
            assert_eq!(judge(cmd, &c, &F), Verdict::Allow, "{cmd}");
        }
        let free = Ctx {
            task_active: false,
            tracked: vec![],
            ..ctx(false)
        };
        assert_eq!(
            judge("git checkout {main,} && git push", &free, &F),
            Verdict::Allow
        );
    }

    #[test]
    fn brace_expansion_inside_a_substitution_fails_closed() {
        let c = ctx(true);
        for cmd in [
            "echo \"$(git push origin {HEAD:main,})\"",
            "echo \"`git push origin {HEAD:main,}`\"",
            "echo \"$(git pu{sh,} origin HEAD:main)\"",
            "echo \"$(echo $(git push origin HEAD:{main,x}))\"",
            "cat <(git push origin HEAD:ma{i..i}n)",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "fail_closed", "{cmd}");
        }
        // A substitution in a command with no words still runs.
        for cmd in [
            "<(git push origin HEAD:main)",
            ">$(git push origin HEAD:main)",
            "{ git push origin HEAD:main; }",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "task_repo", "{cmd}");
        }
        // `${x,}` is a parameter (case change), not a brace list.
        assert_eq!(
            judge("echo ${x,} && git push origin feat-x", &c, &F),
            Verdict::Allow
        );
    }

    #[test]
    fn comments_hide_only_what_bash_treats_as_a_comment() {
        let c = ctx(true);
        // A quote inside a comment does not hide the next line from the brace check.
        assert_eq!(
            rule(judge(
                "git status # it's fine\ngit push origin {HEAD:main,}",
                &c,
                &F
            )),
            "fail_closed"
        );
        // `#` starts a comment only at the start of a word: after a brace, a carriage return or
        // inside a word it is plain text, and the command after it still runs.
        for cmd in [
            "echo {#; git push origin HEAD:main",
            "echo }#; git push origin HEAD:main",
            "echo x\r#; git push origin HEAD:main",
            "echo x\u{a0}#; git push origin HEAD:main",
            // A `)` inside a comment does not close the substitution.
            "echo \"$(echo hi # )\ngit push origin HEAD:main)\"",
            // A `)` always ends a word.
            "(git push origin HEAD:main)",
            "(cd sub && git push origin HEAD:main)",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "task_repo", "{cmd:?}");
        }
        // A case pattern's `)` inside a substitution can't be matched up: fail closed.
        assert_eq!(
            rule(judge(
                "echo \"$(case x in a) git push origin HEAD:main;; esac)\"",
                &c,
                &F
            )),
            "fail_closed"
        );
        // Real comments are still comments, braces and quotes in them included.
        for cmd in [
            "git push origin feat-x # not main",
            "git push origin feat-x #{a,b}",
            "git push origin feat-x # it's done",
            "# git push origin HEAD:main\ngit push origin feat-x",
            "echo hi;# git push origin HEAD:main",
        ] {
            assert_eq!(judge(cmd, &c, &F), Verdict::Allow, "{cmd:?}");
        }
    }

    #[test]
    fn ansi_c_quoting_is_read_the_way_bash_reads_it() {
        let c = ctx(true);
        for cmd in [
            // `\'` does not end the string, so the push is a command of its own.
            "echo $'\\''; git push origin HEAD:main # '",
            "git $'push' origin $'HEAD:main'",
            "git $'\\x70ush' origin HEAD:main",
            "git $'\\160ush' origin HEAD:main",
            "git $'\\u0070ush' origin HEAD:main",
            "git push origin $'HEAD:\\x6dain'",
            "git push origin $'HEAD:\\U0000006Dain'",
            "git pu$'s'h origin HEAD:main",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "task_repo", "{cmd}");
        }
        // Escapes the guard can't turn into plain text fail closed.
        for cmd in [
            "git push origin $'HEAD:ma\\x00in'",
            "git push origin $'HEAD:\\cMmain'",
            "git push origin $'HEAD:\\xffmain'",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "fail_closed", "{cmd}");
        }
        // Hidden behind expansions that are empty or that only match files.
        for cmd in [
            "git pu$@sh origin HEAD:main",
            "git pu${x}sh origin HEAD:main",
            "git pu$(true)sh origin HEAD:main",
            "git pu`true`sh origin HEAD:main",
            "git p[u]sh origin HEAD:main",
            "git pu?h origin HEAD:main",
            "/usr/bin/g[i]t push origin HEAD:main",
            "command g?t push origin HEAD:main",
            "gh p? merge 7",
            "git push origin HEAD:ma[i]n",
        ] {
            assert_ne!(judge(cmd, &c, &F), Verdict::Allow, "{cmd}");
        }
        assert_eq!(
            judge("[ -f x ] && git push origin feat-x", &c, &F),
            Verdict::Allow
        );
        assert_eq!(
            judge(
                "git commit -m $'line one\\nline two' && git push origin feat-x",
                &c,
                &F
            ),
            Verdict::Allow
        );
    }

    /// Facts where `upstream` is the tracked repository and `origin` is not.
    struct TwoRemotes;

    impl RepoFacts for TwoRemotes {
        fn repo(&self, _cwd: &Path) -> Option<String> {
            Some("github.com/o/free".into())
        }
        fn current_branch(&self, _cwd: &Path) -> Option<String> {
            Some("main".into())
        }
        fn default_branch(&self, _cwd: &Path) -> Option<String> {
            Some("main".into())
        }
        fn remote_repo(&self, _cwd: &Path, remote: &str) -> Option<String> {
            match remote {
                "origin" => Some("github.com/o/free".into()),
                "upstream" => Some("github.com/o/r".into()),
                _ => None,
            }
        }
    }

    #[test]
    fn git_push_options_are_read_the_way_git_reads_them() {
        let c = ctx(true);
        // Every spelling of "push all branches", including unique abbreviations.
        for cmd in [
            "git push --branches",
            "git push origin --branches",
            "git push --bra origin",
            "git push --al",
            "git push --mirr origin",
            "git push --mi",
            "git push origin 'refs/heads/*:refs/heads/*'",
            "git push origin :",
            "git push origin +:",
            "git push origin HEAD:",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "fail_closed", "{cmd}");
        }
        // Options git doesn't know, or can't tell apart, and options that run a program.
        for cmd in [
            "git push --frobnicate origin feat-x",
            "git push --f origin feat-x",
            // git itself finds this one ambiguous (--force-with-lease, --force-if-includes).
            "git push --forc origin feat-x",
            "git push --thin=x origin feat-x",
            "git push -z origin feat-x",
            "git push --exec=x origin feat-x",
            "git push --receive-pack x origin feat-x",
            "git --config-env=remote.origin.push=V push",
            "git --config-env remote.origin.push=V push",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "fail_closed", "{cmd}");
        }
        // Options that take a value don't shift the remote and refspecs.
        for cmd in [
            "git push --force origin HEAD:main",
            "git push -fu origin HEAD:main",
            "git push -o ci.skip origin HEAD:main",
            "git push --push-opt ci.skip origin HEAD:main",
            "git push --recurse-submodules check origin HEAD:main",
            "git push -- origin HEAD:main",
            "git push origin HEAD:heads/main",
            "git push origin heads/main",
            "git push origin HEAD:refs/heads/main",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "task_repo", "{cmd}");
        }
        for cmd in [
            "git push --force-with-lease=feat-x:abc origin feat-x",
            "git push --no-all origin feat-x",
            "git push --set-up origin feat-x",
            "git push -o main origin feat-x",
        ] {
            assert_eq!(judge(cmd, &c, &F), Verdict::Allow, "{cmd}");
        }
        // `--repo` names the remote when no other does.
        let free = ctx(false);
        for cmd in ["git push --repo=upstream", "git push --rep upstream"] {
            assert_eq!(rule(judge(cmd, &free, &TwoRemotes)), "containment", "{cmd}");
        }
        assert_eq!(judge("git push", &free, &TwoRemotes), Verdict::Allow);
        // A later fast-forward option undoes `--ff-only`.
        let on_main = Facts {
            repo: F.repo,
            branch: Some("main"),
        };
        for cmd in [
            "git pull --ff-only --no-ff",
            "git pull --ff-only --ff",
            "git pull --ff-only --rebase",
            "git pull --ff-only -r",
        ] {
            assert_eq!(rule(judge(cmd, &c, &on_main)), "task_repo", "{cmd}");
        }
        assert_eq!(
            judge("git pull --ff-only origin", &c, &on_main),
            Verdict::Allow
        );
    }

    #[test]
    fn encoded_or_unusual_api_paths_name_no_repository() {
        let c = ctx(false);
        for cmd in [
            "gh api -X PUT repos/o/free/%2e%2e/%2e%2e/repos/o/r/pulls/7/merge",
            "gh api -X PUT repos/o/free/%2E%2E/%2E%2E/repos/o/r/pulls/7/merge",
            "gh api -X PUT repos/o/free/..%2f..%2frepos/o/r/pulls/7/merge",
            "gh api -X PUT 'repos/o/free/..\\..\\repos/o/r/pulls/7/merge'",
            "gh api -X PUT https://api.github.com:443/repos/o/r/pulls/7/merge",
            "gh api -X PUT https://x@api.github.com/repos/o/r/pulls/7/merge",
            "gh api -X PUT https://api.github.com./repos/o/r/pulls/7/merge",
        ] {
            assert_eq!(rule(judge(cmd, &c, &ByFolder)), "fail_closed", "{cmd}");
        }
        assert_eq!(
            judge("gh api -X PUT repos/o/free/pulls/7/merge", &c, &ByFolder),
            Verdict::Allow
        );
    }
}
