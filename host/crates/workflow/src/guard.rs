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
    /// The folder exists and is in no git repository. The default can't tell, so code that
    /// runs there is judged as running in an unknown checkout.
    fn not_a_repo(&self, _cwd: &Path) -> bool {
        false
    }
    /// The current branch's upstream (`origin/main`), if any.
    fn upstream(&self, _cwd: &Path) -> Option<String> {
        None
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
    /// The home folder, for `~` in folder names.
    pub home: Option<PathBuf>,
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

/// How much of a line the guard reads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reading {
    /// No landing word, and nothing that could make one: allowed unread.
    Skip,
    /// No landing word in the text, but the line computes words at run time (`$a$b`,
    /// `${x}sh`, `$(…)`, backticks, `{a..z}`) or feeds words to git through `xargs`, so a landing
    /// word could still appear. It is read in full; only a computed `cd` is let through, since
    /// with no landing word in sight the folder can't change what lands.
    Computed,
    /// A landing word is in the text: read in full.
    Full,
}

fn reading(line: &str) -> Reading {
    // bash deletes every backslash-newline before it reads the line, so `git pu\<newline>sh` is
    // a push: look at the line both with and without them.
    let joined = line.replace("\\\n", "");
    if mentions_landing(line) || mentions_landing(&joined) {
        return Reading::Full;
    }
    let words = bare(&joined);
    let fed = joined.contains('|') || joined.contains("<<");
    if joined.contains(['$', '`'])
        || (joined.contains('{') && joined.contains(".."))
        || (names_any(&words, &["xargs", "parallel"])
            && names_any(&words, &["git", "gh", "hub", "curl"]))
        || (fed && names_any(&words, &INTERPRETERS))
        || (names_any(&words, &INTERPRETERS) && spawns(&joined))
        || unknown_git_verb(&words)
    {
        Reading::Computed
    } else {
        Reading::Skip
    }
}

/// A git word followed by a verb that is not one of git's own (an alias such as `git pm`).
fn unknown_git_verb(words: &str) -> bool {
    let toks: Vec<&str> = words
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '|' | '&' | '(' | ')' | '<' | '>'))
        .filter(|t| !t.is_empty())
        .collect();
    toks.iter().enumerate().any(|(i, t)| {
        base(t) == "git"
            && toks[i + 1..]
                .iter()
                .find(|w| !w.starts_with('-'))
                .is_some_and(|v| !GIT_VERBS.contains(v) && !v.contains(['/', '.', '=']))
    })
}

/// Words that, beside an interpreter, start a process from its code.
const SPAWN_WORDS: [&str; 19] = [
    "system",
    "exec",
    "execSync",
    "execFile",
    "execFileSync",
    "spawn",
    "spawnSync",
    "popen",
    "subprocess",
    "child_process",
    "shell_exec",
    "passthru",
    "proc_open",
    "execute",
    "Deno",
    "Open3",
    "IPC",
    "fork",
    "pcntl_exec",
];

/// Whether interpreter code on the line may start a process: a spawn word, ruby or perl
/// backquotes, or ruby's `%x(`.
fn spawns(line: &str) -> bool {
    line.contains("%x(")
        || line.contains('`')
        || line
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|t| SPAWN_WORDS.contains(&t))
}

/// Shells and interpreters that run text given on their input.
const INTERPRETERS: [&str; 20] = [
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "pwsh",
    "powershell",
    "cmd",
    "python",
    "python3",
    "py",
    "node",
    "perl",
    "ruby",
    "php",
    "lua",
    "rscript",
    "bun",
    "deno",
    "osascript",
];

/// `s` without quoting, escapes and brace syntax.
fn bare(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\' | '{' | '}' | ','))
        .collect()
}

/// Whether any word of `text` (by its last path part, without `.exe`) is one of `names`, or a
/// filename pattern that matches one.
fn names_any(text: &str, names: &[&str]) -> bool {
    text.split(|c: char| c.is_whitespace() || matches!(c, ';' | '|' | '&' | '(' | ')' | '<' | '>'))
        .map(|w| {
            let w = w.rsplit('/').next().unwrap_or(w).to_ascii_lowercase();
            w.strip_suffix(".exe").map(str::to_owned).unwrap_or(w)
        })
        .any(|w| {
            names
                .iter()
                .any(|n| w == *n || (w.contains(GLOB) && glob_matches(&w, n)))
        })
}

fn mentions_landing(line: &str) -> bool {
    // `$'...'` and `$"..."` can spell any word with escapes (`$'\x70ush'`): read those lines in
    // full, where the reader decodes them.
    if line.contains("$'") || line.contains("$\"") {
        return true;
    }
    // Look past quoting, escapes and brace syntax, so `git pu''sh` and `git pu{sh,}` are still
    // seen as a push; and, separately, past expansions that can be empty and glob brackets, so
    // `git pu$@sh`, `git pu$(true)sh` and `git p[u]sh` are too.
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
    /// Unreadable code that stands on its own command: only the folder it runs in is at stake.
    UnreadableIn(&'static str, PathBuf),
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

/// git's own commands. Any other verb is an alias or an add-on, which the guard can't read.
const GIT_VERBS: &[&str] = &[
    "add",
    "am",
    "annotate",
    "apply",
    "archive",
    "bisect",
    "blame",
    "branch",
    "bugreport",
    "bundle",
    "cat-file",
    "check-attr",
    "check-ignore",
    "check-mailmap",
    "check-ref-format",
    "checkout",
    "checkout-index",
    "cherry",
    "cherry-pick",
    "citool",
    "clean",
    "clone",
    "column",
    "commit",
    "commit-graph",
    "commit-tree",
    "config",
    "count-objects",
    "credential",
    "describe",
    "diagnose",
    "diff",
    "diff-files",
    "diff-index",
    "diff-tree",
    "difftool",
    "fast-export",
    "fast-import",
    "fetch",
    "fetch-pack",
    "filter-branch",
    "fmt-merge-msg",
    "for-each-ref",
    "for-each-repo",
    "format-patch",
    "fsck",
    "gc",
    "get-tar-commit-id",
    "grep",
    "gui",
    "hash-object",
    "help",
    "hook",
    "http-push",
    "index-pack",
    "init",
    "init-db",
    "instaweb",
    "interpret-trailers",
    "lfs",
    "log",
    "ls-files",
    "ls-remote",
    "ls-tree",
    "mailinfo",
    "mailsplit",
    "maintenance",
    "merge",
    "merge-base",
    "merge-file",
    "merge-index",
    "merge-one-file",
    "merge-tree",
    "mktag",
    "mktree",
    "multi-pack-index",
    "mv",
    "name-rev",
    "notes",
    "pack-objects",
    "pack-redundant",
    "pack-refs",
    "patch-id",
    "prune",
    "prune-packed",
    "pull",
    "push",
    "range-diff",
    "read-tree",
    "rebase",
    "receive-pack",
    "reflog",
    "remote",
    "repack",
    "replace",
    "request-pull",
    "rerere",
    "reset",
    "restore",
    "rev-list",
    "rev-parse",
    "revert",
    "rm",
    "send-email",
    "send-pack",
    "shortlog",
    "show",
    "show-branch",
    "show-index",
    "show-ref",
    "sparse-checkout",
    "stage",
    "stash",
    "status",
    "stripspace",
    "submodule",
    "switch",
    "symbolic-ref",
    "tag",
    "unpack-file",
    "unpack-objects",
    "update-index",
    "update-ref",
    "update-server-info",
    "upload-archive",
    "upload-pack",
    "var",
    "verify-commit",
    "verify-pack",
    "verify-tag",
    "version",
    "whatchanged",
    "worktree",
    "write-tree",
    "backfill",
    "refs",
    "replay",
    "fsmonitor--daemon",
    "web--browse",
    "credential-cache",
];

/// git commands that only read (allowed under `xargs`).
const GIT_READS: [&str; 34] = [
    "log",
    "show",
    "diff",
    "status",
    "rev-parse",
    "ls-files",
    "ls-tree",
    "cat-file",
    "blame",
    "merge-base",
    "rev-list",
    "describe",
    "show-ref",
    "ls-remote",
    "for-each-ref",
    "name-rev",
    "shortlog",
    "patch-id",
    "merge-tree",
    "diff-tree",
    "diff-index",
    "diff-files",
    "whatchanged",
    "count-objects",
    "var",
    "version",
    "help",
    "grep",
    "show-branch",
    "cherry",
    "range-diff",
    "check-ignore",
    "check-attr",
    "verify-commit",
];

/// `-c` settings that change how pushes resolve, or that make git run a program.
const GIT_RISKY_CONFIG: [&str; 14] = [
    "alias.",
    "remote.",
    "push.",
    "branch.",
    "url.",
    "include",
    "core.sshcommand",
    "core.hookspath",
    "core.fsmonitor",
    "core.pager",
    "core.gitproxy",
    "credential.",
    "uploadpack.",
    "receivepack.",
];

fn git_shape(
    words: &[String],
    here: &Path,
    facts: &dyn RepoFacts,
    sw: &mut Switched,
    home: Option<&Path>,
) -> Shape {
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
                *cwd = join_dir(cwd, d, home);
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
                if GIT_RISKY_CONFIG.iter().any(|p| key.starts_with(p)) {
                    return Shape::Unreadable(
                        "git -c changes how pushes resolve or makes git run a program",
                    );
                }
                i += step;
            }
            s if s.starts_with("--git-dir")
                || s.starts_with("--work-tree")
                || s.starts_with("--exec-path")
                || s.starts_with("--namespace") =>
            {
                return Shape::Unreadable("git with its own --git-dir, --work-tree or --exec-path");
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
    if !GIT_VERBS.contains(&verb.as_str()) {
        return Shape::Unreadable(
            "a git command that is not one of git's own (an alias or an add-on)",
        );
    }
    if matches!(
        verb.as_str(),
        "push" | "send-pack" | "merge" | "pull" | "http-push"
    ) && words[i + 1..]
        .iter()
        .any(|w| w.contains('$') || w.contains(['?', '[']))
    {
        return Shape::Unreadable("a push, merge or pull with a computed argument");
    }
    let args: Vec<&String> = words[i + 1..].iter().collect();
    let repo = facts.repo(cwd);
    match verb.as_str() {
        "http-push" => Shape::Unreadable("git http-push is not read"),
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
                && (args
                    .iter()
                    .any(|a| matches!(a.as_str(), "--abort" | "--quit"))
                    || local_fast_forward(&args, facts, cwd, sw))
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

/// `git merge --ff-only <upstream>`, spelled literally, where the upstream is the current branch's
/// own and no branch switch came earlier on the line: it moves the local branch to what the remote
/// already has, and lands nothing anywhere.
fn local_fast_forward(args: &[&String], facts: &dyn RepoFacts, cwd: &Path, sw: &Switched) -> bool {
    let mut ff_only = false;
    let mut target: Option<&str> = None;
    for a in args {
        match a.as_str() {
            "--ff-only" => ff_only = true,
            "-q" | "--quiet" | "-v" | "--verbose" | "--no-edit" | "--stat" | "-n" | "--no-stat"
            | "--progress" | "--no-progress" => {}
            s if s.starts_with('-') => return false,
            s if target.is_none() => target = Some(s),
            _ => return false,
        }
    }
    ff_only
        && !sw.unknown
        && sw.branches.is_empty()
        && target.is_some()
        && facts.upstream(cwd).as_deref() == target
}

/// gh's flags that take a value, for the commands the guard reads.
fn gh_takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "-R" | "--repo"
            | "-t"
            | "--subject"
            | "--title"
            | "-T"
            | "-b"
            | "--body"
            | "-F"
            | "--body-file"
            | "--match-head-commit"
            | "-A"
            | "--author-email"
            | "--add-label"
            | "--remove-label"
            | "--add-assignee"
            | "--remove-assignee"
            | "--add-reviewer"
            | "--remove-reviewer"
            | "--add-project"
            | "--remove-project"
            | "-B"
            | "--base"
            | "-H"
            | "--head"
            | "-m"
            | "--milestone"
            | "-l"
            | "--label"
            | "-a"
            | "--assignee"
            | "-r"
            | "--reviewer"
            | "-p"
            | "--project"
            | "-s"
            | "--source"
            | "--branch"
    )
}

/// `--repo`/`-R` as gh reads it (`-R x`, `-R=x`, `-Rx`, `--repo=x`; the last one wins), as
/// `host/owner/name`.
fn gh_repo_flag(args: &[&String]) -> Option<String> {
    let mut found = None;
    let mut i = 0;
    while let Some(a) = args.get(i) {
        let a = a.as_str();
        let v = if a == "-R" || a == "--repo" {
            i += 1;
            args.get(i).map(|s| s.to_string())
        } else if let Some(v) = a.strip_prefix("--repo=").or_else(|| a.strip_prefix("-R=")) {
            Some(v.to_owned())
        } else {
            a.strip_prefix("-R")
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        if v.is_some() {
            found = v;
        }
        i += 1;
    }
    found.map(|r| {
        let r = r
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .to_ascii_lowercase();
        if r.matches('/').count() == 1 {
            format!("github.com/{r}")
        } else {
            r
        }
    })
}

/// The selectors of a gh command after its group and action words: the words that are not
/// flags or flag values.
fn gh_selectors<'a>(args: &[&'a String]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(a) = args.get(i) {
        let a = a.as_str();
        if a == "--" {
            out.extend(args[i + 1..].iter().map(|s| s.as_str()));
            break;
        }
        if a.starts_with('-') && a.len() > 1 {
            if !a.contains('=') && gh_takes_value(a) {
                i += 1;
            }
        } else {
            out.push(a);
        }
        i += 1;
    }
    out
}

/// A PR selector: `7`, `#7`, a PR URL (which names its own repository), or a branch name.
enum PrSel {
    Number(String),
    Url {
        repo: Option<String>,
        number: String,
    },
    Other,
}

fn pr_selector(s: &str) -> PrSel {
    let n = s.trim_start_matches('#');
    if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) {
        return PrSel::Number(n.trim_start_matches('0').to_owned());
    }
    let lower = s.to_ascii_lowercase();
    if let Some(at) = lower.find("/pull/").or_else(|| lower.find("/issues/")) {
        let number: String = lower[at..]
            .trim_start_matches("/pull/")
            .trim_start_matches("/issues/")
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let head = &lower[..at];
        let head = head.split_once("://").map(|x| x.1).unwrap_or(head);
        let parts: Vec<&str> = head.split('/').filter(|p| !p.is_empty()).collect();
        let repo = (parts.len() == 3 && !lower.contains('%')).then(|| {
            format!(
                "{}/{}/{}",
                parts[0].rsplit('@').next().unwrap_or(""),
                parts[1],
                parts[2]
            )
        });
        return PrSel::Url {
            repo,
            number: number.trim_start_matches('0').to_owned(),
        };
    }
    PrSel::Other
}

fn gh_shape(
    words: &[String],
    cwd: &Path,
    facts: &dyn RepoFacts,
    flip_label: Option<&str>,
) -> Shape {
    let args: Vec<&String> = words[1..].iter().collect();
    let flag_repo = gh_repo_flag(&args);
    let first = args.first().map(|s| s.as_str());
    let second = args.get(1).map(|s| s.as_str());
    let computed = |w: &str| w.contains('$') || w.contains(GLOB);
    if first.is_some_and(computed) || second.is_some_and(computed) {
        return Shape::Unreadable("gh with a computed subcommand");
    }
    let landing_sub = matches!(
        (first, second),
        (Some("pr"), Some("merge" | "ready" | "edit" | "create"))
            | (Some("issue"), Some("edit"))
            | (Some("api"), _)
            | (Some("repo"), Some("sync"))
    );
    if landing_sub && args.iter().any(|w| w.contains('$')) {
        return Shape::Unreadable("a gh landing command with a computed argument");
    }
    let rest: &[&String] = args.get(2..).unwrap_or(&[]);
    let sel = gh_selectors(rest);
    // The PR the command acts on, and the repository: a URL names its own, and a `--repo` beside
    // a URL that names another one can't be told apart.
    let target = || -> Result<(Option<String>, Option<String>), &'static str> {
        let here = || flag_repo.clone().or_else(|| facts.repo(cwd));
        match sel.first().map(|s| pr_selector(s)) {
            None | Some(PrSel::Other) => Ok((here(), None)),
            Some(PrSel::Number(n)) => Ok((here(), Some(n))),
            Some(PrSel::Url { repo: None, .. }) => Err("a PR address the guard can't read"),
            Some(PrSel::Url {
                repo: Some(r),
                number,
            }) => {
                if flag_repo.as_deref().is_some_and(|f| !same_repo(f, &r)) {
                    Err("a PR address and --repo that name different repositories")
                } else {
                    Ok((Some(r), Some(number)))
                }
            }
        }
    };
    let github = |what: &'static str| match target() {
        Ok((repo, pr)) => Shape::GitHub { repo, pr, what },
        Err(why) => Shape::Unreadable(why),
    };
    let removes_flip = || {
        let removed = |v: &str| flip_label.is_some_and(|l| v.split(',').any(|x| x.trim() == l));
        rest.windows(2)
            .any(|w| w[0].as_str() == "--remove-label" && removed(w[1]))
            || rest
                .iter()
                .any(|a| a.strip_prefix("--remove-label=").is_some_and(removed))
    };
    match (first, second) {
        (Some("pr"), Some("merge")) => github("a PR merge"),
        (Some("pr"), Some("ready")) => github("a ready flip"),
        (Some("pr"), Some("create"))
            if !rest
                .iter()
                .any(|a| a.as_str() == "--draft" || a.as_str() == "-d") =>
        {
            Shape::NonDraftPr {
                repo: flag_repo.or_else(|| facts.repo(cwd)),
            }
        }
        (Some("pr" | "issue"), Some("edit")) if removes_flip() => {
            github("removing the review label")
        }
        (Some("repo"), Some("sync")) => Shape::GitHub {
            repo: flag_repo.or_else(|| facts.repo(cwd)),
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

/// What the guard knows about the whole line before reading its commands.
#[derive(Clone, Copy, Default)]
struct LineFacts {
    /// The line defines an alias or a function, names a GitHub API address, or feeds a group or
    /// loop from a redirection: interpreter code on it never stands alone.
    whole: bool,
}

impl LineFacts {
    fn of(cmd: &str) -> Self {
        let lower = cmd.to_ascii_lowercase();
        let api = lower.contains("api.github.com")
            || lower.contains("/api/v3")
            || lower.contains("graphql");
        let alias = tokens(cmd).any(|t| t == "alias");
        let fed_group = ["done", "}", ")", "fi", "esac"].iter().any(|k| {
            cmd.match_indices(k).any(|(i, _)| {
                cmd[i + k.len()..]
                    .trim_start_matches([' ', '\t'])
                    .starts_with('<')
            })
        });
        // `name() {` or `name() (`: a function definition, not a call written in some code.
        let function = cmd.match_indices('(').any(|(i, _)| {
            let after = cmd[i + 1..].trim_start();
            after.starts_with(')') && after[1..].trim_start().starts_with(['{', '('])
        }) || tokens(cmd).any(|t| t == "function");
        LineFacts {
            whole: api || alias || fed_group || function,
        }
    }
}

/// `d` as the shell resolves it from `cwd`: `~` is the home folder, and on Windows a Git Bash
/// folder (`/c/x`, `/tmp`) is the Windows folder it stands for.
fn join_dir(cwd: &Path, d: &str, home: Option<&Path>) -> PathBuf {
    match (d.strip_prefix('~'), home) {
        (Some(rest), Some(h)) if rest.is_empty() || rest.starts_with('/') => {
            h.join(rest.trim_start_matches('/'))
        }
        _ if d.starts_with('/') => native_path(d),
        _ => cwd.join(d),
    }
}

/// A Git Bash folder as Windows reads it: `/c/x` is `C:/x`, `/tmp` the temporary folder. Other
/// systems read folders as they are.
pub fn native_path(p: &str) -> PathBuf {
    if cfg!(windows) {
        let mut chars = p.chars();
        if let (Some('/'), Some(d)) = (chars.next(), chars.next()) {
            let rest = chars.as_str();
            if d.is_ascii_alphabetic() && (rest.is_empty() || rest.starts_with('/')) {
                return PathBuf::from(format!(
                    "{}:/{}",
                    d.to_ascii_uppercase(),
                    rest.trim_start_matches('/')
                ));
            }
        }
        if let Some(rest) = p
            .strip_prefix("/tmp")
            .filter(|r| r.is_empty() || r.starts_with('/'))
        {
            return std::env::temp_dir().join(rest.trim_start_matches('/'));
        }
    }
    PathBuf::from(p)
}

#[allow(clippy::too_many_arguments)]
fn simple_shape(
    c: &Simple,
    cwd: &mut PathBuf,
    ctx: &Ctx,
    facts: &dyn RepoFacts,
    sw: &mut Switched,
    read: Reading,
    depth: usize,
    line: &LineFacts,
) -> Shape {
    const VARS: &str = "it sets a variable that changes where git or gh acts, or what they run";
    if c.assigns.iter().any(|(k, _)| moves_git(k)) {
        return Shape::Unreadable(VARS);
    }
    // Look through shell keywords and command wrappers to the command they run.
    let mut start = 0;
    while let Some(w) = c.words.get(start) {
        match w.as_str() {
            "if" | "then" | "else" | "elif" | "while" | "until" | "do" | "!" | "time" | "exec"
            | "nohup" | "builtin" | "caffeinate" => start += 1,
            "command" => {
                start += 1;
                // `command -v git` and `command -V git` look a name up.
                if c.words.get(start).is_some_and(|a| a == "-v" || a == "-V") {
                    return Shape::None;
                }
            }
            "env" => {
                start += 1;
                while let Some(a) = c.words.get(start) {
                    if matches!(a.as_str(), "-S" | "--split-string" | "-C" | "--chdir")
                        || a.starts_with("--split-string=")
                        || a.starts_with("--chdir=")
                    {
                        return Shape::Unreadable("env that splits a string or changes folder");
                    }
                    if a.starts_with('-') || a.contains('=') {
                        if a.split('=').next().is_some_and(moves_git) {
                            return Shape::Unreadable(VARS);
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
            words: c.words[start..].to_vec(),
            computed_words: c.computed_words.get(start..).unwrap_or(&[]).to_vec(),
            subs: Vec::new(),
            ..c.clone()
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
    let name = base(cmd);
    let home = ctx.home.as_deref();
    // git, gh and curl judge their own computed words where they matter; a computed folder is
    // unreadable outright.
    let folder = matches!(name.as_str(), "cd" | "pushd");
    if c.computed && folder && read == Reading::Full {
        return Shape::Unreadable("it changes to a computed folder");
    }
    if let Some(verb) = name.strip_prefix("git-") {
        // `git-push` is `git push`.
        let mut words = vec!["git".to_owned(), verb.to_owned()];
        words.extend(c.words[1..].iter().cloned());
        return git_shape(&words, cwd, facts, sw, home);
    }
    if let Some(l) = lang(&name) {
        return code_shape(&name, l, c, start > 0, cwd, line);
    }
    match name.as_str() {
        "cd" | "pushd" => {
            match c.words.get(1).filter(|d| *d != "--") {
                Some(d) => *cwd = join_dir(cwd, d, home),
                None => match home {
                    Some(h) if c.words.len() == 1 => *cwd = h.to_path_buf(),
                    _ => return Shape::Unreadable("cd with no folder"),
                },
            }
            Shape::None
        }
        "git" => git_shape(&c.words, cwd, facts, sw, home),
        "gh" => gh_shape(&c.words, cwd, facts, ctx.flip_label.as_deref()),
        "hub" => Shape::Unreadable("hub is not read"),
        "curl" => curl_shape(&c.words, cwd, facts),
        "eval" | "trap" | "source" | "." | "fish" | "csh" | "tcsh" | "runuser" | "sudo"
        | "doas" => Shape::Unreadable("it runs text the guard cannot read"),
        "xargs" => xargs_shape(&c.words[1..]),
        "find" => find_shape(&c.words),
        "awk" | "gawk" | "mawk" | "nawk" => {
            if c.computed {
                Shape::Unreadable("awk code the guard can't read")
            } else {
                awk_shape(&c.words)
            }
        }
        "sed" | "gsed" => sed_shape(&c.words, c.computed),
        n if SHELL_RUNNERS.contains(&n) => {
            Shape::Unreadable("it runs a command string through a shell")
        }
        n if n.contains("ssh") => {
            if depth > 0 {
                Shape::Unreadable("a remote command inside a remote command")
            } else {
                remote_shape(&c.words, ctx, facts)
            }
        }
        // A `for` word list is data, and these never run their operands.
        "for" | "select" | "case" | "in" => Shape::None,
        n if INERT.contains(&n) || package_query(&c.words) => Shape::None,
        _ => {
            // git, gh, curl or hub as an operand of some other command (a wrapper the reader does
            // not know, `watch`, ...) is read as that command: here, unreadable.
            if (1..c.words.len()).any(|i| operand_runs_tool(&c.words, i)) {
                Shape::Unreadable("it runs git, gh or curl through another command")
            } else {
                Shape::None
            }
        }
    }
}

/// Variables that, set on the line, change where git or gh acts or what they run.
const MOVES_GIT: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "HOME",
    "CDPATH",
    "GH_REPO",
    "XDG_CONFIG_HOME",
];

/// Variables that make an interpreter run code of their choosing.
const RUNS_CODE: [&str; 8] = [
    "PERL5OPT",
    "PERL5DB",
    "LUA_INIT",
    "NODE_OPTIONS",
    "PYTHONSTARTUP",
    "PYTHONINSPECT",
    "BASH_ENV",
    "ENV",
];

fn moves_git(k: &str) -> bool {
    MOVES_GIT.contains(&k)
        || RUNS_CODE.contains(&k)
        || k.starts_with("GIT_CONFIG")
        || k == "GIT_EXEC_PATH"
        || k == "GIT_SSH"
        || k == "GIT_SSH_COMMAND"
}

/// A command word by its last path part, lower case and without `.exe`.
fn base(w: &str) -> String {
    let b = w
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(w)
        .to_ascii_lowercase();
    b.strip_suffix(".exe").map(str::to_owned).unwrap_or(b)
}

const TOOLS: [&str; 4] = ["git", "gh", "curl", "hub"];

fn is_tool(w: &str) -> bool {
    let b = base(w);
    TOOLS.contains(&b.as_str()) || b.starts_with("git-")
}

/// Commands that never run their operands, so `git` among them is only a word.
const INERT: [&str; 22] = [
    "echo",
    "printf",
    "which",
    "type",
    "whereis",
    "man",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ls",
    "cat",
    "head",
    "tail",
    "wc",
    "test",
    "[",
    "[[",
    "dpkg-query",
    "apt-cache",
    "true",
    "false",
];

/// Wrappers that run the command after their own options.
const RUNNERS: [&str; 13] = [
    "env", "nice", "sudo", "doas", "setsid", "strace", "busybox", "timeout", "nohup", "stdbuf",
    "time", "command", "ionice",
];

/// Whether an operand of some other command would be run as git, gh, curl or hub. A word with a
/// blank in it is not a program name; the last word, when it is a path into a folder of that
/// name (`ls ~/git`) and not a program folder, is a path.
fn operand_runs_tool(words: &[String], i: usize) -> bool {
    let w = &words[i];
    if w.contains([' ', '\t', '\n']) || !is_tool(w) {
        return false;
    }
    let last = i + 1 == words.len();
    let path = w.contains('/')
        && !w.starts_with("./")
        && !w.starts_with("../")
        && !["/bin/", "/libexec/", "git-core", "/sbin/"]
            .iter()
            .any(|p| w.contains(p));
    !(last && path)
}

/// A package manager asked about packages, not running them.
fn package_query(words: &[String]) -> bool {
    match base(&words[0]).as_str() {
        "dpkg" => words[1..].iter().any(|w| {
            matches!(
                w.as_str(),
                "-s" | "-l"
                    | "-L"
                    | "-S"
                    | "-p"
                    | "--status"
                    | "--list"
                    | "--listfiles"
                    | "--search"
                    | "--print-avail"
            )
        }),
        "dpkg-query" | "apt-cache" => true,
        "rpm" => words[1..].iter().any(|w| w.starts_with("-q")),
        "brew" | "apt" | "winget" | "choco" | "scoop" => words
            .get(1)
            .is_some_and(|w| matches!(w.as_str(), "info" | "list" | "search" | "show")),
        _ => false,
    }
}

/// `xargs` options that take a value (`-I {}`, `-n 1`, ...), and whether the option turns on
/// replace mode. A glued value (`-I{}`, `-n1`) is part of the word.
fn xargs_command(args: &[String]) -> Result<(usize, Option<String>), &'static str> {
    let mut i = 0;
    let mut replace: Option<String> = None;
    let mut replaces = 0;
    while let Some(a) = args.get(i) {
        if a == "--" {
            return Ok((i + 1, replace));
        }
        if !a.starts_with('-') || a == "-" {
            break;
        }
        if a.contains(GLOB) || (a.contains(['{', '$']) && !a.starts_with("-I")) {
            return Err("xargs with an option the guard can't read");
        }
        if let Some(l) = a.strip_prefix("--") {
            let (name, glued) = match l.split_once('=') {
                Some((n, v)) => (n, Some(v.to_owned())),
                None => (l, None),
            };
            match name {
                "replace" => {
                    replaces += 1;
                    replace = Some(glued.unwrap_or_else(|| "{}".into()));
                }
                "arg-file" | "delimiter" | "max-args" | "max-procs" | "max-chars" | "max-lines"
                | "eof" | "process-slot-var"
                    if glued.is_none() =>
                {
                    i += 1;
                }
                _ => {}
            }
            i += 1;
            continue;
        }
        // A cluster of one-letter options; the first that takes a value takes the rest.
        let short = &a[1..];
        let mut took = false;
        for (k, f) in short.char_indices() {
            let rest = &short[k + 1..];
            match f {
                'I' | 'J' => {
                    replaces += 1;
                    replace = Some(if rest.is_empty() {
                        i += 1;
                        args.get(i)
                            .cloned()
                            .ok_or("xargs -I without its replace string")?
                    } else {
                        rest.to_owned()
                    });
                    took = true;
                }
                'i' => {
                    replaces += 1;
                    replace = Some(if rest.is_empty() {
                        "{}".into()
                    } else {
                        rest.to_owned()
                    });
                    took = true;
                }
                'a' | 'd' | 'E' | 'L' | 'n' | 'P' | 's' => {
                    if rest.is_empty() {
                        i += 1;
                    }
                    took = true;
                }
                'e' | 'l' => took = true, // an optional glued value
                _ => {}
            }
            if took {
                break;
            }
        }
        i += 1;
    }
    if replaces > 1 {
        return Err("xargs with more than one replace string");
    }
    Ok((i, replace))
}

/// What `xargs` runs. Only a git command that reads is let through: under replace mode only
/// `xargs -I<s> git <reading verb> …` with the replace string after the verb.
fn xargs_shape(args: &[String]) -> Shape {
    const WHY: &str = "xargs runs a command the guard can't read";
    let (k, replace) = match xargs_command(args) {
        Ok(x) => x,
        Err(why) => return Shape::Unreadable(why),
    };
    let mut cmd = &args[k..];
    while let Some(w) = cmd.first() {
        if RUNNERS.contains(&base(w).as_str()) || w.contains('=') || w.starts_with('-') {
            // A runner's own options or settings: whatever follows still runs.
            cmd = &cmd[1..];
        } else {
            break;
        }
    }
    let Some(name) = cmd.first() else {
        return Shape::None; // xargs runs echo
    };
    if name.contains(['$', '{', '}']) || name.contains(GLOB) {
        return Shape::Unreadable(WHY);
    }
    let b = base(name);
    if INTERPRETERS.contains(&b.as_str()) || SHELL_RUNNERS.contains(&b.as_str()) {
        return Shape::Unreadable(WHY);
    }
    if !is_tool(name) {
        return Shape::None;
    }
    if b != "git" {
        return Shape::Unreadable(WHY);
    }
    let Some((verb_at, verb)) = git_verb(cmd) else {
        return Shape::Unreadable(WHY);
    };
    let writes = cmd[verb_at..].iter().any(|w| {
        w.starts_with("--output")
            || (verb == "grep" && (w == "-O" || w.starts_with("--open-files")))
    });
    if !GIT_READS.contains(&verb.as_str()) || writes {
        return Shape::Unreadable(WHY);
    }
    if let Some(r) = replace {
        if r.is_empty() || cmd[..=verb_at].iter().any(|w| w.contains(&r)) {
            return Shape::Unreadable(WHY);
        }
    }
    Shape::None
}

/// The verb of a `git` command line and where it is, past git's own options.
fn git_verb(words: &[String]) -> Option<(usize, String)> {
    let mut i = 1;
    while let Some(w) = words.get(i) {
        match w.as_str() {
            "-C" | "-c" | "--config-env" | "--namespace" | "--exec-path" => i += 2,
            s if s.starts_with('-') => i += 1,
            v => return (!v.contains(['$', '*', '?', '['])).then(|| (i, v.to_owned())),
        }
    }
    None
}

/// Commands that run a command string or the words after them through a shell.
const SHELL_RUNNERS: [&str; 7] = [
    "watch", "script", "expect", "flock", "parallel", "unbuffer", "su",
];

/// `find … -exec <cmd> … ;`: the command after each action is run.
fn find_shape(words: &[String]) -> Shape {
    let mut i = 1;
    while i < words.len() {
        if matches!(words[i].as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
            let Some(cmd) = words.get(i + 1) else {
                return Shape::Unreadable("find -exec without a command");
            };
            let b = base(cmd);
            if is_tool(cmd)
                || cmd.contains(['{', '$'])
                || cmd.contains(GLOB)
                || INTERPRETERS.contains(&b.as_str())
                || SHELL_RUNNERS.contains(&b.as_str())
                || RUNNERS.contains(&b.as_str())
            {
                return Shape::Unreadable("find -exec runs a command the guard can't read");
            }
        }
        i += 1;
    }
    Shape::None
}

/// A remote shell (`ssh host '<command>'`): the remote command runs on a machine the guard can't
/// see, so one that may land is unreadable.
fn remote_shape(words: &[String], ctx: &Ctx, facts: &dyn RepoFacts) -> Shape {
    const WHY: &str = "a command run on another machine, which the guard can't check";
    // Every operand after the host is part of the remote command; joining them is how ssh sends it.
    let ops: Vec<&String> = words[1..].iter().filter(|w| !w.starts_with('-')).collect();
    if ops.len() < 2 {
        return Shape::None; // a login, or a host with no command
    }
    let remote = ops[1..]
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let cmds = match shell::parse(&remote) {
        Parsed::Commands(c) => c,
        Parsed::Unreadable(_) => return Shape::Unreadable(WHY),
    };
    let nowhere = PathBuf::from("\u{0}remote");
    for c in &cmds {
        if !c.subs.is_empty() {
            return Shape::Unreadable(WHY);
        }
        let mut here = nowhere.clone();
        let mut sw = Switched::default();
        match simple_shape(
            c,
            &mut here,
            ctx,
            facts,
            &mut sw,
            Reading::Full,
            1,
            &LineFacts::default(),
        ) {
            Shape::None => {}
            _ => return Shape::Unreadable(WHY),
        }
    }
    Shape::None
}

/// awk: string and regex literals are text. A pipe out of a `print` or into `getline`,
/// `system()`, `ENVIRON` and a gawk `@` call run or read commands.
fn awk_runs_commands(program: &str) -> bool {
    let p: Vec<char> = program.chars().collect();
    let mut i = 0;
    // The last significant character, to tell a regex `/` from a division.
    let mut last: Option<char> = None;
    let mut word = String::new();
    let mut words: Vec<String> = Vec::new();
    while i < p.len() {
        let c = p[i];
        if c.is_ascii_alphanumeric() || c == '_' {
            word.push(c);
            i += 1;
            continue;
        }
        if !word.is_empty() {
            words.push(std::mem::take(&mut word));
            last = Some('a');
        }
        match c {
            '"' => {
                i += 1;
                while i < p.len() && p[i] != '"' {
                    i += if p[i] == '\\' { 2 } else { 1 };
                }
                last = Some('a');
            }
            '/' if last.is_none_or(|l| "(,{};!~&|\n=?:".contains(l)) => {
                i += 1;
                while i < p.len() && p[i] != '/' && p[i] != '\n' {
                    if p[i] == '\\' {
                        i += 1;
                    } else if p[i] == '[' {
                        // A bracket expression can hold a `/`.
                        i += 1;
                        while i < p.len() && p[i] != ']' && p[i] != '\n' {
                            i += 1;
                        }
                    }
                    i += 1;
                }
                last = Some('a');
            }
            '#' => {
                while i < p.len() && p[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '|' if p.get(i + 1) == Some(&'|') => {
                i += 1;
                last = Some('|');
            }
            '|' | '@' => return true,
            ' ' | '\t' => {}
            c => last = Some(c),
        }
        i += 1;
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
        .iter()
        .any(|w| matches!(w.as_str(), "system" | "ENVIRON" | "PROCINFO"))
}

fn awk_shape(words: &[String]) -> Shape {
    const WHY: &str = "awk code that runs commands";
    let mut i = 1;
    let mut program: Option<&String> = None;
    while let Some(w) = words.get(i) {
        match w.as_str() {
            "--" => {
                program = words.get(i + 1);
                break;
            }
            "-f" | "--file" => return Shape::None, // a program file: a script, not read
            "-e" | "--source" => {
                let Some(code) = words.get(i + 1) else {
                    return Shape::Unreadable(WHY);
                };
                if awk_runs_commands(code) {
                    return Shape::Unreadable(WHY);
                }
                program = Some(code);
                i += 2;
                continue;
            }
            "-F" | "-v" | "--field-separator" | "--assign" => i += 2,
            s if s.starts_with('-') && s.len() > 1 => i += 1,
            _ => {
                if program.is_none() {
                    program = Some(w);
                }
                break;
            }
        }
    }
    match program {
        Some(p) if awk_runs_commands(p) => Shape::Unreadable(WHY),
        _ => Shape::None,
    }
}

/// Whether a GNU sed script runs commands: the `e` command, or an `s` command with the `e` flag.
fn sed_runs_commands(script: &str) -> bool {
    let p: Vec<char> = script.chars().collect();
    let mut i = 0;
    let skip_ws = |i: &mut usize| {
        while *i < p.len() && matches!(p[*i], ' ' | '\t' | '\n' | ';' | '}' | '{' | '!') {
            *i += 1;
        }
    };
    // Read a delimited part (`/re/`, the halves of `s`), from just after its opening delimiter.
    let delimited = |i: &mut usize, d: char| -> bool {
        while *i < p.len() && p[*i] != d {
            if p[*i] == '\\' {
                *i += 1;
            } else if p[*i] == '\n' {
                return false;
            }
            *i += 1;
        }
        *i += 1;
        *i <= p.len()
    };
    loop {
        skip_ws(&mut i);
        if i >= p.len() {
            return false;
        }
        // An address: numbers, `$`, `/re/` or `\cREc`, ranges with `,`, steps with `~`, `+N`.
        loop {
            match p.get(i) {
                Some(c) if c.is_ascii_digit() || matches!(c, '$' | ',' | '~' | '+' | ' ' | '!') => {
                    i += 1
                }
                Some('/') => {
                    i += 1;
                    if !delimited(&mut i, '/') {
                        return true;
                    }
                    while p.get(i).is_some_and(|c| matches!(c, 'I' | 'M')) {
                        i += 1;
                    }
                }
                Some('\\') => {
                    let Some(&d) = p.get(i + 1) else { return true };
                    i += 2;
                    if !delimited(&mut i, d) {
                        return true;
                    }
                }
                _ => break,
            }
        }
        let Some(&cmd) = p.get(i) else { return false };
        i += 1;
        match cmd {
            'e' => return true,
            's' | 'y' => {
                let Some(&d) = p.get(i) else { return true };
                if d == '\n' || d == '\\' {
                    return true;
                }
                i += 1;
                if !delimited(&mut i, d) || !delimited(&mut i, d) {
                    return true;
                }
                if cmd == 's' {
                    // The flags: up to the end of the command. `w` takes the rest as a file name.
                    while let Some(&f) = p.get(i) {
                        match f {
                            'e' => return true,
                            'w' => {
                                while i < p.len() && p[i] != '\n' {
                                    i += 1;
                                }
                                break;
                            }
                            ';' | '\n' | '}' | ' ' | '\t' | '#' => break,
                            _ => i += 1,
                        }
                    }
                }
            }
            // Text or a file name to the end of the line.
            'a' | 'i' | 'c' | 'r' | 'R' | 'w' | 'W' | ':' | 'b' | 't' | 'T' | '#' => {
                while i < p.len()
                    && p[i] != '\n'
                    && !(matches!(cmd, 'b' | 't' | 'T') && p[i] == ';')
                {
                    if p[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
    }
}

fn sed_shape(words: &[String], computed: bool) -> Shape {
    const WHY: &str = "a sed script that runs commands";
    let mut scripts: Vec<&String> = Vec::new();
    let mut from_file = false;
    let mut i = 1;
    while let Some(w) = words.get(i) {
        if w == "--" {
            i += 1;
            break;
        }
        if let Some(code) = w.strip_prefix("--expression=") {
            if sed_runs_commands(code) {
                return Shape::Unreadable(WHY);
            }
            scripts.push(w);
        } else if w == "-e" || w == "--expression" {
            let Some(code) = words.get(i + 1) else {
                return Shape::Unreadable(WHY);
            };
            scripts.push(code);
            i += 1;
        } else if w == "-f" || w == "--file" || w.starts_with("--file=") {
            from_file = true;
            if !w.contains('=') {
                i += 1;
            }
        } else if w == "-l" || w == "--line-length" {
            i += 1;
        } else if w.starts_with('-') && w.len() > 1 && !w.starts_with("--") {
            // A cluster; `-e` or `-f` inside takes the rest or the next word.
            if let Some(k) = w[1..].find(['e', 'f']) {
                let rest = &w[k + 2..];
                let value = if rest.is_empty() {
                    i += 1;
                    words.get(i)
                } else {
                    None
                };
                if w.as_bytes()[k + 1] == b'f' {
                    from_file = true;
                } else if let Some(v) = value {
                    scripts.push(v);
                } else if sed_runs_commands(rest) {
                    return Shape::Unreadable(WHY);
                }
            }
        } else if !w.starts_with('-') {
            break;
        }
        i += 1;
    }
    if scripts.is_empty() && !from_file {
        if let Some(w) = words.get(i) {
            scripts.push(w);
        }
    }
    if computed && scripts.iter().any(|s| s.contains('$')) {
        return Shape::Unreadable("a sed script the guard can't read");
    }
    if scripts.iter().any(|s| sed_runs_commands(s)) {
        Shape::Unreadable(WHY)
    } else {
        Shape::None
    }
}

/// curl, read for a write to GitHub's API. A request with a body or a method other than GET or
/// HEAD is a write; the repository is the one the address names.
fn curl_shape(words: &[String], cwd: &Path, facts: &dyn RepoFacts) -> Shape {
    let mut writes = false;
    let mut get = false;
    let mut urls: Vec<&String> = Vec::new();
    let mut computed_option = false;
    let method = |m: &str, writes: &mut bool| {
        if !m.eq_ignore_ascii_case("GET") && !m.eq_ignore_ascii_case("HEAD") {
            *writes = true;
        }
    };
    let mut i = 1;
    while let Some(w) = words.get(i) {
        i += 1;
        if let Some(l) = w.strip_prefix("--") {
            let (name, glued) = match l.split_once('=') {
                Some((n, v)) => (n, Some(v.to_owned())),
                None => (l, None),
            };
            let value = || glued.clone().or_else(|| words.get(i).cloned());
            match name {
                "request" => {
                    let m = value().unwrap_or_default();
                    computed_option |= m.contains('$');
                    method(&m, &mut writes);
                }
                "data" | "data-raw" | "data-binary" | "data-urlencode" | "data-ascii" | "json"
                | "form" | "form-string" | "upload-file" => writes = true,
                "get" => get = true,
                "url" => {
                    if let Some(u) = words.get(i).filter(|_| glued.is_none()) {
                        urls.push(u);
                    }
                }
                _ => {}
            }
            if glued.is_none()
                && matches!(
                    name,
                    "request"
                        | "data"
                        | "data-raw"
                        | "data-binary"
                        | "data-urlencode"
                        | "data-ascii"
                        | "json"
                        | "form"
                        | "form-string"
                        | "upload-file"
                        | "header"
                        | "output"
                        | "user"
                        | "user-agent"
                        | "referer"
                        | "cookie"
                        | "cookie-jar"
                        | "config"
                        | "max-time"
                        | "connect-timeout"
                        | "write-out"
                        | "proxy"
                        | "retry"
                        | "cacert"
                        | "cert"
                        | "key"
                        | "url"
                )
            {
                i += 1;
            }
        } else if w.len() > 1 && w.starts_with('-') {
            let short: Vec<char> = w[1..].chars().collect();
            for (k, f) in short.iter().enumerate() {
                let rest: String = short[k + 1..].iter().collect();
                let takes = "XdFTHoOuAebcKwmxryYzEC".contains(*f) && *f != 'O';
                let value = if rest.is_empty() && takes {
                    let v = words.get(i).cloned();
                    i += 1;
                    v
                } else {
                    Some(rest.clone())
                };
                match f {
                    'X' => {
                        let m = value.unwrap_or_default();
                        computed_option |= m.contains('$');
                        method(&m, &mut writes);
                    }
                    'd' | 'F' | 'T' => writes = true,
                    'G' => get = true,
                    _ => {}
                }
                if takes {
                    break;
                }
            }
        } else {
            urls.push(w);
        }
    }
    if computed_option {
        return Shape::Unreadable("curl with a computed method");
    }
    if !writes || get {
        return Shape::None;
    }
    if urls.iter().any(|u| u.contains('$')) {
        return Shape::Unreadable("a curl write to an address the guard can't read");
    }
    for u in &urls {
        let lower = u.to_ascii_lowercase();
        let rest = lower
            .strip_prefix("https://")
            .or_else(|| lower.strip_prefix("http://"))
            .unwrap_or(&lower);
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        let (host, path) = if host == "api.github.com" {
            ("github.com".to_owned(), path.to_owned())
        } else if let Some(p) = path.strip_prefix("api/v3/") {
            (host.to_owned(), p.to_owned())
        } else {
            continue;
        };
        let path = path.split(['?', '#']).next().unwrap_or("").to_owned();
        return Shape::GitHub {
            repo: path_repo(&host, &path, cwd, facts),
            pr: None,
            what: "a GitHub API write",
        };
    }
    if words
        .iter()
        .any(|w| w.to_ascii_lowercase().contains("api.github.com"))
    {
        return Shape::GitHub {
            repo: None,
            pr: None,
            what: "a GitHub API write",
        };
    }
    Shape::None
}

/// The languages whose code the guard looks for on a command line.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    Shell,
    Windows,
    Python,
    Perl,
    Node,
    /// ruby, php, lua, Rscript, bun, deno and the like: inline code is found, never read.
    Other,
}

fn lang(name: &str) -> Option<Lang> {
    Some(match name {
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "mksh" | "ash" => Lang::Shell,
        "pwsh" | "powershell" | "cmd" => Lang::Windows,
        "perl" | "perl5" => Lang::Perl,
        "node" | "nodejs" => Lang::Node,
        "ruby" | "php" | "lua" | "luajit" | "rscript" | "bun" | "deno" | "tclsh" | "osascript"
        | "jshell" | "irb" => Lang::Other,
        n if n == "py" || n.starts_with("python") || n.starts_with("pypy") => Lang::Python,
        _ => return None,
    })
}

/// Where an interpreter's code comes from.
enum Code {
    /// It only prints a version or help, or checks syntax.
    Inert,
    /// A script file or module: not read (the scripts residual).
    Script(String),
    /// Code on the command line.
    Inline(String),
    /// Code on its input.
    Stdin,
    Unreadable(&'static str),
}

/// Python modules a module run (`-m`) may name that run code of their own.
const CODE_MODULES: [&str; 8] = [
    "timeit", "code", "pdb", "runpy", "cProfile", "profile", "trace", "doctest",
];

fn python_code(args: &[String]) -> Code {
    let mut i = 0;
    while let Some(w) = args.get(i) {
        i += 1;
        match w.as_str() {
            "-" => return Code::Stdin,
            "--" => return args.get(i).map_or(Code::Stdin, |s| Code::Script(s.clone())),
            "--version" | "--help" | "-V" | "-VV" | "-h" | "-?" => return Code::Inert,
            s if s.starts_with("--") => {
                if s == "--check-hash-based-pycs" {
                    i += 1;
                }
            }
            s if s.len() > 1 && s.starts_with('-') => {
                let short: Vec<char> = s[1..].chars().collect();
                for (k, f) in short.iter().enumerate() {
                    let rest: String = short[k + 1..].iter().collect();
                    let value = |i: &mut usize| {
                        if rest.is_empty() {
                            *i += 1;
                            args.get(*i - 1).cloned()
                        } else {
                            Some(rest.clone())
                        }
                    };
                    match f {
                        'c' => {
                            return value(&mut i)
                                .map_or(Code::Unreadable("python -c without code"), Code::Inline)
                        }
                        'm' => {
                            let m = value(&mut i).unwrap_or_default();
                            return if CODE_MODULES.contains(&m.as_str()) || m.contains('$') {
                                Code::Unreadable("a python module that runs code")
                            } else {
                                Code::Script(m)
                            };
                        }
                        'W' | 'X' | 'Q' => {
                            value(&mut i);
                            break;
                        }
                        'V' | 'h' => return Code::Inert,
                        _ => {}
                    }
                }
            }
            s => return Code::Script(s.to_owned()),
        }
    }
    Code::Stdin
}

/// perl's options, clusters included: `-0pi -e`, `-lane`, `-CSDe` reach `-e`; `-pie` is `-i`
/// with backup extension `e`. Modules (`-M`) and the debugger make the code unreadable.
fn perl_code(args: &[String], modules: &mut bool) -> Code {
    let mut codes: Vec<String> = Vec::new();
    let mut i = 0;
    while let Some(w) = args.get(i) {
        i += 1;
        if w == "--" {
            break;
        }
        if w == "-" {
            return Code::Stdin;
        }
        if !(w.len() > 1 && w.starts_with('-')) {
            i -= 1;
            break;
        }
        if w == "--version" || w == "--help" {
            return Code::Inert;
        }
        let short: Vec<char> = w[1..].chars().collect();
        let mut k = 0;
        while k < short.len() {
            let f = short[k];
            let rest: String = short[k + 1..].iter().collect();
            match f {
                'e' | 'E' => {
                    if rest.is_empty() {
                        match args.get(i) {
                            Some(c) => codes.push(c.clone()),
                            None => return Code::Unreadable("perl -e without code"),
                        }
                        i += 1;
                    } else {
                        codes.push(rest);
                    }
                    break;
                }
                'i' | 'F' | 'x' => break, // the rest of the word is the option's value
                'M' | 'm' | 'I' => {
                    *modules = true;
                    if rest.is_empty() {
                        i += 1;
                    }
                    break;
                }
                'd' | 'D' => return Code::Unreadable("perl's debugger"),
                'v' | 'V' => return Code::Inert,
                '0' => {
                    k += 1;
                    while k < short.len() && (short[k].is_ascii_hexdigit() || short[k] == 'x') {
                        k += 1;
                    }
                    continue;
                }
                'l' => {
                    k += 1;
                    while k < short.len() && short[k].is_ascii_digit() {
                        k += 1;
                    }
                    continue;
                }
                'C' => {
                    k += 1;
                    while k < short.len() && "IOESioDALa0123456789".contains(short[k]) {
                        k += 1;
                    }
                    continue;
                }
                _ => {}
            }
            k += 1;
        }
    }
    if !codes.is_empty() {
        return Code::Inline(codes.join("\n"));
    }
    args.get(i).map_or(Code::Stdin, |s| Code::Script(s.clone()))
}

fn node_code(args: &[String]) -> Code {
    let mut i = 0;
    while let Some(w) = args.get(i) {
        i += 1;
        let (name, glued) = match w.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_owned())),
            _ => (w.as_str(), None),
        };
        match name {
            "-e" | "--eval" | "-p" | "--print" | "-pe" => {
                return glued
                    .or_else(|| args.get(i).cloned())
                    .map_or(Code::Unreadable("node -e without code"), Code::Inline)
            }
            "-v" | "--version" | "-h" | "--help" | "-c" | "--check" => return Code::Inert,
            "-" => return Code::Stdin,
            "--" => return args.get(i).map_or(Code::Stdin, |s| Code::Script(s.clone())),
            "-r"
            | "--require"
            | "--import"
            | "--loader"
            | "--experimental-loader"
            | "--input-type"
            | "--env-file"
            | "-C"
            | "--conditions"
            | "--test-name-pattern"
            | "--test-reporter"
                if glued.is_none() =>
            {
                i += 1
            }
            s if s.starts_with('-') => {}
            s => return Code::Script(s.to_owned()),
        }
    }
    Code::Stdin
}

/// ruby `-e`, php `-r`/`-B`/`-R`/`-E`/`--run`, lua `-e`, Rscript `-e`, bun `-e`/`--eval`,
/// `deno eval`, osascript `-e`: inline code; otherwise a script or the input.
fn other_code(name: &str, args: &[String]) -> Code {
    let flags: &[&str] = match name {
        "php" => &["-r", "-B", "-R", "-E", "--run"],
        "bun" => &["-e", "--eval", "-p", "--print"],
        _ => &["-e"],
    };
    if name == "deno" {
        return match args.first().map(String::as_str) {
            Some("eval") => args
                .get(1)
                .map_or(Code::Unreadable("deno eval without code"), |c| {
                    Code::Inline(c.clone())
                }),
            Some(s) => Code::Script(s.to_owned()),
            None => Code::Stdin,
        };
    }
    let mut i = 0;
    while let Some(w) = args.get(i) {
        i += 1;
        let cluster_e =
            name == "ruby" && w.starts_with('-') && !w.starts_with("--") && w.ends_with('e');
        if flags.contains(&w.as_str()) || cluster_e {
            return args
                .get(i)
                .map_or(Code::Unreadable("inline code without its text"), |c| {
                    Code::Inline(c.clone())
                });
        }
        if w == "--version" || w == "-v" || w == "--help" {
            return Code::Inert;
        }
        if !w.starts_with('-') || w == "-" {
            return if w == "-" {
                Code::Stdin
            } else {
                Code::Script(w.clone())
            };
        }
    }
    Code::Stdin
}

/// The words of `text` as git, gh and the landing verbs would be spotted in it: runs of letters,
/// digits, `-` and `_`.
fn tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
        .filter(|t| !t.is_empty())
}

const NAMES_LANDING: [&str; 9] = [
    "merge",
    "pull",
    "push",
    "send-pack",
    "http-push",
    "git",
    "gh",
    "curl",
    "hub",
];

fn names_landing(text: &str) -> bool {
    tokens(text).any(|t| NAMES_LANDING.contains(&t.to_ascii_lowercase().as_str()))
}

/// Interpreter code stands on its own when nothing comes before the interpreter, nothing is piped
/// into it, nothing on its command is computed, nothing feeds it from a descriptor or device, and
/// neither it nor any heredoc it reads names git, gh, curl, hub or a landing verb. Then only the
/// folder it runs in is at stake.
fn stands_alone(c: &Simple, wrapped: bool, line: &LineFacts) -> bool {
    !wrapped
        && !line.whole
        && c.assigns.is_empty()
        && !c.piped
        && !c.computed
        && c.subs.is_empty()
        && !c.redirs.iter().any(|r| {
            r.starts_with("<&")
                || r.starts_with("<<<")
                || r.contains("/dev/")
                || r.contains("/proc/")
        })
        && !c
            .words
            .iter()
            .any(|w| w.contains(['$', '`']) || names_landing(w))
        && c.heredocs
            .iter()
            .all(|h| (h.quoted || !h.body.contains(['$', '`'])) && !names_landing(&h.body))
}

/// Python modules simple file-editing code may import.
const PY_MODULES: [&str; 58] = [
    "re",
    "json",
    "sys",
    "os",
    "pathlib",
    "textwrap",
    "difflib",
    "collections",
    "itertools",
    "functools",
    "string",
    "math",
    "glob",
    "fnmatch",
    "csv",
    "io",
    "datetime",
    "time",
    "typing",
    "dataclasses",
    "shutil",
    "hashlib",
    "base64",
    "unicodedata",
    "tomllib",
    "argparse",
    "pprint",
    "statistics",
    "random",
    "copy",
    "enum",
    "operator",
    "html",
    "struct",
    "bisect",
    "heapq",
    "decimal",
    "fractions",
    "uuid",
    "tempfile",
    "filecmp",
    "zlib",
    "gzip",
    "bz2",
    "lzma",
    "zipfile",
    "tarfile",
    "configparser",
    "xml",
    "secrets",
    "locale",
    "calendar",
    "contextlib",
    "abc",
    "numbers",
    "warnings",
    "traceback",
    "codecs",
];

/// What `os.` and `sys.` may be followed by in simple code.
const PY_OS: [&str; 22] = [
    "path", "getcwd", "listdir", "makedirs", "mkdir", "remove", "unlink", "rename", "replace",
    "walk", "sep", "linesep", "environ", "getenv", "scandir", "stat", "rmdir", "chdir", "utime",
    "chmod", "fspath", "curdir",
];
const PY_SYS: [&str; 10] = [
    "argv",
    "stdout",
    "stderr",
    "stdin",
    "exit",
    "path",
    "version",
    "version_info",
    "platform",
    "maxsize",
];

/// Names that run or reach code in Python, whatever module they come from.
const PY_BANNED: [&str; 24] = [
    "exec",
    "eval",
    "compile",
    "__import__",
    "importlib",
    "getattr",
    "setattr",
    "delattr",
    "globals",
    "locals",
    "vars",
    "builtins",
    "__builtins__",
    "breakpoint",
    "subprocess",
    "ctypes",
    "socket",
    "multiprocessing",
    "asyncio",
    "urllib",
    "http",
    "requests",
    "runpy",
    "pty",
];

/// Whether Python code is the simple file-editing kind: it imports only modules from a fixed
/// list, uses `os` and `sys` only for files, paths and arguments, and never names anything that
/// runs code, starts a process or opens a connection.
fn simple_python(code: &str) -> bool {
    let chars: Vec<char> = code.chars().collect();
    // Identifiers with what follows them (`os . system` is `os.system` to Python).
    let mut ids: Vec<(String, Option<String>)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_alphanumeric() || chars[i] == '_' {
            let s = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let id: String = chars[s..i].iter().collect();
            if !id.is_ascii() {
                return false; // Python folds look-alike letters into plain ones
            }
            let mut j = i;
            while j < chars.len() && matches!(chars[j], ' ' | '\t') {
                j += 1;
            }
            let attr = (chars.get(j) == Some(&'.')).then(|| {
                j += 1;
                while j < chars.len() && matches!(chars[j], ' ' | '\t') {
                    j += 1;
                }
                let a = j;
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                chars[a..j].iter().collect::<String>()
            });
            ids.push((id, attr));
        } else {
            i += 1;
        }
    }
    for (id, _) in &ids {
        let dunder = id.starts_with("__")
            && !matches!(
                id.as_str(),
                "__name__" | "__main__" | "__file__" | "__doc__"
            );
        if dunder || PY_BANNED.contains(&id.as_str()) {
            return false;
        }
    }
    // Imports: each statement that imports names only allowed modules.
    for stmt in code.split(['\n', ';']) {
        let t = stmt.trim();
        let words: Vec<&str> = t.split_whitespace().collect();
        match words.first().copied() {
            Some("import") => {
                for m in t["import".len()..].split(',') {
                    let m = m.split_whitespace().collect::<Vec<_>>();
                    let root = m
                        .first()
                        .map(|x| x.split('.').next().unwrap_or(""))
                        .unwrap_or("");
                    if !PY_MODULES.contains(&root) || (m.len() > 1 && matches!(root, "os" | "sys"))
                    {
                        return false;
                    }
                }
            }
            Some("from") => {
                let root = words
                    .get(1)
                    .map(|x| x.split('.').next().unwrap_or(""))
                    .unwrap_or("");
                if !PY_MODULES.contains(&root) || words.get(2) != Some(&"import") {
                    return false;
                }
                if matches!(root, "os" | "sys") && words.get(1).is_some_and(|m| !m.contains('.')) {
                    let allowed = if root == "os" {
                        &PY_OS[..]
                    } else {
                        &PY_SYS[..]
                    };
                    let names = t.split_once(" import ").map(|x| x.1).unwrap_or("");
                    if names
                        .split([',', '(', ')'])
                        .map(|n| n.split_whitespace().next().unwrap_or(""))
                        .any(|n| !n.is_empty() && !allowed.contains(&n))
                    {
                        return false;
                    }
                }
            }
            _ => {
                if tokens(t).any(|w| w == "import") {
                    return false; // an import the guard can't place
                }
            }
        }
    }
    // Outside its imports, `os` only as `os.<file call>` and `sys` only as `sys.<argument or
    // stream>`.
    let body: String = code
        .split('\n')
        .flat_map(|l| l.split(';'))
        .filter(|st| {
            let t = st.trim_start();
            !(t.starts_with("import ") || t.starts_with("from "))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if body != code {
        return simple_python_body(&body);
    }
    simple_python_body(code)
}

/// The `os` and `sys` rule of [`simple_python`], on code with its imports taken out.
fn simple_python_body(code: &str) -> bool {
    let chars: Vec<char> = code.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if !(chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
            i += 1;
            continue;
        }
        let s = i;
        while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
            i += 1;
        }
        let id: String = chars[s..i].iter().collect();
        let before = s.checked_sub(1).map(|k| chars[k]);
        let allowed: &[&str] = match id.as_str() {
            "os" if before != Some('.') => &PY_OS,
            "sys" if before != Some('.') => &PY_SYS,
            _ => continue,
        };
        let mut j = i;
        while j < chars.len() && matches!(chars[j], ' ' | '\t') {
            j += 1;
        }
        if chars.get(j) != Some(&'.') {
            return false;
        }
        j += 1;
        while j < chars.len() && matches!(chars[j], ' ' | '\t') {
            j += 1;
        }
        let a = j;
        while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
            j += 1;
        }
        let attr: String = chars[a..j].iter().collect();
        if !allowed.contains(&attr.as_str()) {
            return false;
        }
    }
    true
}

/// Whether perl code is only substitutions and transliterations (`s/a/b/g; tr/a-z/A-Z/`), with
/// no flag or construct that runs code (`/e`, `(?{ })`, `@{[ ]}`, `${\ }`).
fn simple_perl(code: &str) -> bool {
    let p: Vec<char> = code.chars().collect();
    let mut i = 0;
    let close_of = |d: char| match d {
        '{' => '}',
        '(' => ')',
        '[' => ']',
        '<' => '>',
        d => d,
    };
    // One delimited part starting at its opening delimiter; returns its text.
    let part = |i: &mut usize| -> Option<String> {
        let open = *p.get(*i)?;
        if open.is_alphanumeric() || open.is_whitespace() || open == '#' {
            return None;
        }
        let close = close_of(open);
        *i += 1;
        let mut depth = 1;
        let start = *i;
        while *i < p.len() {
            let c = p[*i];
            if c == '\\' {
                *i += 2;
                continue;
            }
            if c == close && open != close {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            } else if c == open && open != close {
                depth += 1;
            } else if c == close {
                break;
            }
            *i += 1;
        }
        if *i >= p.len() {
            return None;
        }
        let text: String = p[start..*i].iter().collect();
        *i += 1;
        Some(text)
    };
    loop {
        while i < p.len() && (p[i].is_whitespace() || p[i] == ';') {
            i += 1;
        }
        if i >= p.len() {
            return true;
        }
        let rest: String = p[i..].iter().take(2).collect();
        let (op, flags_ok): (usize, &str) = if rest.starts_with("tr")
            && !p.get(i + 2).is_some_and(|c| c.is_alphanumeric())
        {
            (2, "cdsr")
        } else if (p[i] == 's' || p[i] == 'y') && !p.get(i + 1).is_some_and(|c| c.is_alphanumeric())
        {
            (1, if p[i] == 's' { "gimsxopdualnr" } else { "cdsr" })
        } else {
            return false;
        };
        let subst = op == 1 && p[i] == 's';
        i += op;
        let Some(pattern) = part(&mut i) else {
            return false;
        };
        // With bracket delimiters the second half has its own pair, maybe after blanks.
        let bracketed = close_of(p[i - 1]) != p[i - 1] || matches!(p[i - 1], '}' | ')' | ']' | '>');
        let replacement = if bracketed {
            while i < p.len() && p[i].is_whitespace() {
                i += 1;
            }
            part(&mut i)
        } else {
            // The closing delimiter of the first half opens the second.
            i -= 1;
            part(&mut i)
        };
        let Some(replacement) = replacement else {
            return false;
        };
        let start = i;
        while i < p.len() && p[i].is_ascii_alphabetic() {
            i += 1;
        }
        let flags: String = p[start..i].iter().collect();
        if !flags.chars().all(|f| flags_ok.contains(f)) {
            return false;
        }
        if subst
            && (pattern.contains("(?{")
                || pattern.contains("(??{")
                || replacement.contains("@{")
                || replacement.contains("${"))
        {
            return false;
        }
    }
}

/// What an interpreter or shell on the line runs, read as far as the guard reads code.
fn code_shape(
    name: &str,
    l: Lang,
    c: &Simple,
    wrapped: bool,
    cwd: &Path,
    line: &LineFacts,
) -> Shape {
    let args = &c.words[1..];
    match l {
        Lang::Shell => {
            if args.iter().any(|w| {
                w == "-c" || (w.starts_with('-') && !w.starts_with("--") && w.contains('c'))
            }) {
                return Shape::Unreadable("inline shell code is not read");
            }
            return if c.stdin_fed
                || script_word(name, args).is_none_or(|s| s.contains('$'))
                || args.iter().any(|w| {
                    matches!(
                        w.as_str(),
                        "-" | "/dev/stdin" | "/dev/fd/0" | "/proc/self/fd/0"
                    ) || (w.starts_with('-') && !w.starts_with("--") && w.contains('s'))
                }) {
                // Fed by a heredoc, a here-string or a pipe, with no script named (or one whose
                // name is computed, such as `bash <(…)`), or told to read its input (`-`, `-s`,
                // `/dev/stdin`): it runs text the guard does not read.
                Shape::Unreadable("a shell running text from its input")
            } else {
                Shape::None
            };
        }
        Lang::Windows => {
            let inline = args.iter().any(|w| {
                let w = w.to_ascii_lowercase();
                matches!(
                    w.as_str(),
                    "-command" | "-c" | "/c" | "/k" | "-encodedcommand" | "-e" | "-ec" | "-enc"
                )
            });
            return if inline || (c.stdin_fed && args.is_empty()) {
                Shape::Unreadable("a PowerShell or cmd command string is not read")
            } else {
                Shape::None
            };
        }
        _ => {}
    }
    let mut modules = false;
    let code = match l {
        Lang::Python => python_code(args),
        Lang::Perl => perl_code(args, &mut modules),
        Lang::Node => node_code(args),
        _ => other_code(name, args),
    };
    // Code written in a computed word is not the code that runs.
    let mut code_computed = false;
    let text = match code {
        Code::Inert => return Shape::None,
        Code::Unreadable(why) => return Shape::Unreadable(why),
        Code::Script(s) => {
            return if s.contains('$')
                || s.starts_with("/dev/")
                || s.starts_with("/proc/")
                || s.starts_with("data:")
            {
                Shape::Unreadable("a program read from a computed name, a device or a process")
            } else if c.stdin_fed && s == "-" {
                Shape::Unreadable("an interpreter running text from its input")
            } else {
                Shape::None
            };
        }
        Code::Inline(t) => {
            code_computed = c
                .words
                .iter()
                .zip(&c.computed_words)
                .any(|(w, k)| *k && *w == t);
            t
        }
        Code::Stdin => {
            // Its own quoted heredoc is code the guard can read; any other input is not.
            let fed_otherwise = c.piped || c.redirs.iter().any(|r| r.starts_with('<'));
            match &c.heredocs[..] {
                [h] if !fed_otherwise => h.body.clone(),
                _ => return Shape::Unreadable("an interpreter running text from its input"),
            }
        }
    };
    let env_code = c
        .assigns
        .iter()
        .any(|(k, _)| RUNS_CODE.contains(&k.as_str()));
    let simple = !env_code
        && !code_computed
        && match l {
            Lang::Python => simple_python(&text),
            Lang::Perl => !modules && simple_perl(&text),
            _ => false,
        };
    if simple {
        Shape::None
    } else if stands_alone(c, wrapped, line) {
        Shape::UnreadableIn("inline interpreter code is not read", cwd.to_path_buf())
    } else {
        Shape::Unreadable("inline interpreter code is not read")
    }
}

/// The script an interpreter is told to run: its first word that is not an option. For a shell,
/// `+x` is an option too, and `-o`/`-O` (alone or ending a cluster such as `-eo`) take the next
/// word as their value. `None` when no script is named, so the interpreter reads its input,
/// which may come from a redirection on an enclosing group or loop that the guard does not track.
fn script_word<'a>(name: &str, args: &'a [String]) -> Option<&'a str> {
    let shell = matches!(name, "sh" | "bash" | "zsh" | "dash");
    let mut i = 0;
    while let Some(w) = args.get(i) {
        if w == "--" {
            return args.get(i + 1).map(String::as_str);
        }
        let option = w.len() > 1 && (w.starts_with('-') || (shell && w.starts_with('+')));
        if !option {
            return Some(w);
        }
        let takes_value = shell
            && (matches!(w.as_str(), "--rcfile" | "--init-file")
                || (!w.starts_with("--") && w.contains(['o', 'O'])));
        i += if takes_value { 2 } else { 1 };
    }
    None
}

/// Two repositories are the same when their owner and name match, on any host: an API path, a
/// `--repo` or a URL may name the host differently from the remote.
pub fn same_repo(a: &str, b: &str) -> bool {
    let tail = |s: &str| {
        let mut it = s.rsplit('/');
        let name = it
            .next()
            .unwrap_or("")
            .trim_end_matches(".git")
            .to_ascii_lowercase();
        let owner = it.next().unwrap_or("").to_ascii_lowercase();
        (owner, name)
    };
    let (x, y) = (tail(a), tail(b));
    !x.1.is_empty() && x == y
}

/// A quote-tier refusal ends the judging; a reason-tier one is kept while the rest of the line
/// is read, since a quote-tier refusal later on wins.
fn keep(v: Verdict, soft: &mut Option<Verdict>) -> Option<Verdict> {
    match v {
        Verdict::Deny {
            tier: Tier::Quote, ..
        } => Some(v),
        Verdict::Deny { .. } => {
            soft.get_or_insert(v);
            None
        }
        Verdict::Allow => None,
    }
}

fn is_protected(ctx: &Ctx, facts: &dyn RepoFacts, cwd: &Path, branch: &str) -> bool {
    ctx.protected.iter().any(|p| p == branch)
        || facts.default_branch(cwd).as_deref() == Some(branch)
}

/// Which shell reads the command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellKind {
    /// bash, zsh, dash: the reader's own language.
    Posix,
    /// PowerShell (Claude Code's PowerShell tool on Windows).
    PowerShell,
}

/// Judge one Bash command.
pub fn judge(cmd: &str, ctx: &Ctx, facts: &dyn RepoFacts) -> Verdict {
    judge_as(cmd, ShellKind::Posix, ctx, facts)
}

/// Judge one command for the shell that will run it. PowerShell lines are read as POSIX shell
/// only when they hold nothing PowerShell reads differently (variables, backtick escapes, script
/// blocks, the call operator); any other PowerShell line that mentions a landing fails closed.
pub fn judge_as(cmd: &str, shell: ShellKind, ctx: &Ctx, facts: &dyn RepoFacts) -> Verdict {
    let at_stake = ctx.task_active || !ctx.tracked.is_empty();
    let unknown = if ctx.task_active {
        Tier::Quote
    } else {
        Tier::Reason
    };
    if cmd.len() > MAX_COMMAND {
        return if at_stake {
            Verdict::Deny {
                tier: Tier::Quote,
                rule: "fail_closed",
                why: "The command is too large to check.".into(),
            }
        } else {
            Verdict::Allow
        };
    }
    if shell == ShellKind::PowerShell
        && cmd.contains(['$', '`', '@', '{', '}', '(', ')', '&', '\\'])
    {
        return if at_stake && mentions_landing(cmd) {
            Verdict::Deny {
                tier: unknown,
                rule: "fail_closed",
                why: "A PowerShell command that may land can't be read; run it with the Bash tool instead.".into(),
            }
        } else {
            Verdict::Allow
        };
    }
    let read = reading(cmd);
    if read == Reading::Skip {
        return Verdict::Allow;
    }
    // Brace expansion, comments and quoting are the reader's: it reports a line that uses brace
    // expansion anywhere, substitutions included, as unreadable.
    let line = LineFacts::of(cmd);
    let mut sw = Switched::default();
    if reorders(cmd) {
        // A first pass only collects the switches; judging with all of them can only deny more.
        let _ = judge_in(cmd, ctx, facts, &mut sw, read, &line);
    }
    judge_in(cmd, ctx, facts, &mut sw, read, &line)
}

/// [`judge`] after its size check and pre-filter. Substitutions recurse here with the same
/// switches, since a branch switch inside `$(…)` still moves the folder's branch.
fn judge_in(
    cmd: &str,
    ctx: &Ctx,
    facts: &dyn RepoFacts,
    sw: &mut Switched,
    read: Reading,
    line: &LineFacts,
) -> Verdict {
    let at_stake = ctx.task_active || !ctx.tracked.is_empty();
    // A landing the guard can't place: with a task, it may be the task's; without one it can
    // only touch other tasks' work.
    let unknown = if ctx.task_active {
        Tier::Quote
    } else {
        Tier::Reason
    };
    let deny = |tier, rule, why: String| Verdict::Deny { tier, rule, why };
    let cmds = match shell::parse(cmd) {
        Parsed::Commands(c) => c,
        Parsed::Unreadable(why) => {
            return if at_stake {
                deny(
                    unknown,
                    "fail_closed",
                    format!("The command can't be read ({why})."),
                )
            } else {
                Verdict::Allow
            };
        }
    };
    let mut cwd = ctx.cwd.clone();
    let task_repo = |r: &str| ctx.task_active && ctx.task_repos.iter().any(|t| same_repo(t, r));
    let has_tracked = |r: &str| ctx.tracked.iter().any(|t| same_repo(&t.repo, r));
    // Keep judging after a reason-tier refusal: a quote-tier one later in the line wins.
    let mut soft: Option<Verdict> = None;
    for c in &cmds {
        // What a substitution runs is judged as a command of its own.
        for sub in &c.subs {
            let here = Ctx {
                cwd: cwd.clone(),
                ..ctx.clone()
            };
            if let Some(v) = keep(judge_in(sub, &here, facts, sw, read, line), &mut soft) {
                return v;
            }
        }
        let shape = simple_shape(c, &mut cwd, ctx, facts, sw, read, 0, line);
        let (repo, what): (Option<String>, String) = match shape {
            Shape::None => continue,
            Shape::Unreadable(why) => {
                if at_stake {
                    let v = deny(
                        unknown,
                        "fail_closed",
                        format!("Part of the command can't be checked: {why}."),
                    );
                    if let Some(v) = keep(v, &mut soft) {
                        return v;
                    }
                }
                continue;
            }
            Shape::UnreadableIn(why, dir) => {
                // Only the folder this code runs in is at stake.
                let tier = match facts.repo(&dir) {
                    Some(r) if task_repo(&r) => Some(Tier::Quote),
                    Some(r) if has_tracked(&r) => Some(Tier::Reason),
                    Some(_) => None,
                    None if facts.not_a_repo(&dir) => None,
                    None => at_stake.then_some(unknown),
                };
                if let Some(tier) = tier {
                    let v = deny(
                        tier,
                        "fail_closed",
                        format!("Part of the command can't be checked: {why}, and it runs in a checkout with work at stake (or one the guard can't name)."),
                    );
                    if let Some(v) = keep(v, &mut soft) {
                        return v;
                    }
                }
                continue;
            }
            Shape::Into { repo, branch, at } => {
                let Some(b) = branch else {
                    if at_stake {
                        let v = deny(
                            unknown,
                            "fail_closed",
                            "The branch this would land on could not be resolved.".into(),
                        );
                        if let Some(v) = keep(v, &mut soft) {
                            return v;
                        }
                    }
                    continue;
                };
                let target = b
                    .lines()
                    .find(|x| is_protected(ctx, facts, &at, x))
                    .map(str::to_owned);
                match target {
                    Some(t) => (repo, format!("landing on the protected branch {t}")),
                    None => continue,
                }
            }
            Shape::GitHub { repo, pr, what } => {
                let literal = pr.as_deref().filter(|n| n.len() <= 9 && !n.is_empty());
                let held = |n: &str| {
                    ctx.tracked.iter().any(|t| {
                        t.pr.as_deref()
                            .and_then(|x| x.rsplit('/').next())
                            .map(|x| x.trim_start_matches('0'))
                            == Some(n)
                    })
                };
                if let (Some(r), Some(n)) = (&repo, literal) {
                    if !held(n) {
                        let in_repo: Vec<&Tracked> = ctx
                            .tracked
                            .iter()
                            .filter(|t| same_repo(&t.repo, r))
                            .collect();
                        if task_repo(r) {
                            // In a task repository, a PR no tracked change holds, where every
                            // tracked change has its PR recorded, is provably not the task's.
                            if in_repo.iter().all(|t| t.pr.is_some()) {
                                soft.get_or_insert(deny(
                                    Tier::Reason,
                                    "untracked",
                                    format!("{what} of untracked PR #{n}"),
                                ));
                                continue;
                            }
                        } else {
                            // Another task's repository: only a tracked PR is contained.
                            continue;
                        }
                    }
                }
                (repo, what.to_owned())
            }
            Shape::NonDraftPr { repo } => match &repo {
                Some(r) if ctx.draft_repos.iter().any(|d| same_repo(d, r)) && at_stake => {
                    return deny(
                        Tier::Quote,
                        "draft_rule",
                        format!("{r} lands by the draft rule; open PRs there with --draft."),
                    );
                }
                _ => continue,
            },
        };
        let Some(repo) = repo else {
            if at_stake {
                let v = deny(
                    unknown,
                    "fail_closed",
                    format!("{what}, in a repository the guard could not name"),
                );
                if let Some(v) = keep(v, &mut soft) {
                    return v;
                }
            }
            continue;
        };
        if task_repo(&repo) {
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
    soft.unwrap_or(Verdict::Allow)
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
        home: home_dir(),
    })
}

/// The user's home folder, as the shell's `~`.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
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
            home: None,
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
        // curl names the repository in its address, as gh api does.
        assert_eq!(
            rule(judge(
                "curl -X POST https://api.github.com/repos/o/r/merges",
                &c,
                &F
            )),
            "task_repo"
        );
        assert_eq!(
            rule(judge("curl -d x https://api.github.com/graphql", &c, &F)),
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
        // Tracked names keep their case; a lower-cased --repo still matches them. Outside the
        // task's repositories only a merge of a tracked PR is contained.
        let mixed = Ctx {
            tracked: vec![Tracked {
                repo: "github.com/O/R".into(),
                branch: "feat-x".into(),
                pr: Some("https://github.com/O/R/pull/7".into()),
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

    #[test]
    fn line_continuations_and_process_substitutions_read_as_bash_reads_them() {
        let c = ctx(true);
        for cmd in [
            // A process substitution is a word: a `#` right after it is not a comment.
            "cat <(true)#; git push origin HEAD:main",
            "cat >(true)#; git push origin HEAD:main",
            // A line continuation is deleted before bash reads the line: the `#` after it starts
            // a comment, so the quote in that comment hides nothing.
            "git status \\\n#'\ngit push origin HEAD:main\n# '",
            // A continuation inside `$'`, a word or an operator still joins it.
            "git $\\\n'\\x70ush' origin HEAD:main",
            "git pu\\\nsh origin HEAD:main",
            "git push origin HEAD:main |\\\n| true",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "task_repo", "{cmd:?}");
        }
        // `<\<newline><` is still a heredoc, whose text is not a command.
        assert_eq!(
            judge("cat <\\\n<'EOF'\ngit push origin HEAD:main\nEOF", &c, &F),
            Verdict::Allow
        );
    }

    #[test]
    fn hex_escapes_in_braces_decode_like_bash() {
        let c = ctx(true);
        for cmd in [
            "git $'\\x{70}ush' origin HEAD:main",
            "git $'\\x{0000070}ush' origin HEAD:main",
            "git $'\\x{170}ush' origin HEAD:main",
            "git $'\\x{70ush' origin HEAD:main",
            "git push origin $'HEAD:\\x{6d}ain'",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "task_repo", "{cmd}");
        }
        assert_eq!(
            rule(judge("git push origin $'HEAD:\\x{}main'", &c, &F)),
            "fail_closed"
        );
    }

    #[test]
    fn a_hash_inside_arithmetic_is_not_a_comment() {
        let c = ctx(true);
        for cmd in [
            "(( 1 # )) || git push origin HEAD:main",
            "if (( 1 # )); then :; else git push origin HEAD:main; fi",
            "for (( i=0; i<1 #; i++ )); do :; done; git push origin HEAD:main",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "fail_closed", "{cmd}");
        }
        for cmd in [
            "(( 16#ff > 1 )) && git push origin feat-x",
            "( # a subshell comment\ngit push origin feat-x )",
        ] {
            assert_eq!(judge(cmd, &c, &F), Verdict::Allow, "{cmd:?}");
        }
    }

    #[test]
    fn landing_words_built_at_run_time_are_read() {
        let c = ctx(true);
        for cmd in [
            "a=pu; b=sh; git $a$b origin HEAD:main",
            "x=pu; git ${x}sh origin HEAD:main",
            "git $(printf 'p%sh' us) origin HEAD:main",
            "echo pu | (read v; git ${v}sh origin HEAD:main)",
            "git p{u..u}sh origin HEAD:main",
            "echo hsup | rev | xargs -I{} git {} origin HEAD:main",
            "$G $V origin HEAD:main",
        ] {
            assert_ne!(judge(cmd, &c, &F), Verdict::Allow, "{cmd}");
        }
        // Common commands that compute words but land nothing still pass.
        for cmd in [
            "echo $HOME",
            "cd \"$(git rev-parse --show-toplevel)\" && npm test",
            "for f in $(ls); do echo \"$f\"; done",
            "git log --format=\"%H $x\" -n 1",
            "git commit -m \"$(cat <<'EOF'\nfix {a,b}: it's done\n\nsecond paragraph\nEOF\n)\" && git push origin feat-x",
        ] {
            assert_eq!(judge(cmd, &c, &F), Verdict::Allow, "{cmd:?}");
        }
    }

    #[test]
    fn arithmetic_and_text_fed_to_a_shell_are_read_as_bash_reads_them() {
        let c = ctx(true);
        // `<<` inside arithmetic is a shift, not a heredoc: the next lines are commands.
        for cmd in [
            "(( 1 << 2 ))\ngit push origin HEAD:main\ntrue",
            "for (( i=0; i<1<<2; i++ ))\ndo :; done\ngit push origin HEAD:main\ntrue",
            "echo $((1<<2))\ngit push origin HEAD:main\ntrue",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "task_repo", "{cmd:?}");
        }
        for cmd in [
            "echo $[1<<2]\ngit push origin HEAD:main\ntrue]",
            // Heredoc text starts after the newline that follows `(( ))`.
            "cat <<'EOF'; (( 1 +\n$(git push origin HEAD:main) ))\nx\nEOF",
            // A shell or interpreter runs the text on its input.
            "bash <<'EOF'\ngit push origin HEAD:main\nEOF",
            "python3 - <<'EOF'\nimport os\nos.system('git push origin HEAD:main')\nEOF",
            "cat <<'EOF' | bash\ngit push origin HEAD:main\nEOF",
            "sh <<< 'git push origin HEAD:main'",
            "echo 'git push origin HEAD:main' | bash",
            // No landing word in the text, but words fed to git or a shell.
            "echo otrg | tr a-y b-z | xargs -I{} g''it {} origin HEAD:main",
            "echo otrg | tr a-y b-z | x''args -I{} git {} origin HEAD:main",
            "echo otrg | tr a-y b-z | xargs -I{} /usr/bin/g?t {} origin HEAD:main",
            "echo tig otrg nigiro niam:DAEH | tr a-y b-z | sh",
        ] {
            assert_ne!(judge(cmd, &c, &F), Verdict::Allow, "{cmd:?}");
        }
        for cmd in [
            "bash scripts/check.sh | tee log && git push origin feat-x",
            "(( n = 1 << 3 )) && git push origin feat-x",
        ] {
            assert_eq!(judge(cmd, &c, &F), Verdict::Allow, "{cmd:?}");
        }
    }

    #[test]
    fn subshells_that_look_like_arithmetic_and_shells_fed_by_pipes_fail_closed() {
        let c = ctx(true);
        let on_main = Facts {
            repo: F.repo,
            branch: Some("main"),
        };
        // bash reads these `((` as two subshells, where `>` is a redirection.
        for cmd in [
            "((git push origin >feat-x) )",
            "echo $((git push origin >feat-x) )",
            "((true) ; git push origin >feat-x)",
            "((git push origin >feat-x)\\\n)",
            "(\\\n(git push origin >feat-x) )",
            "echo $\\\n((git push origin >feat-x) )",
            "( (git push origin >feat-x) )",
            "git push origin {x}>/dev/null",
        ] {
            assert_ne!(judge(cmd, &c, &on_main), Verdict::Allow, "{cmd}");
        }
        // `$(\<newline>(` is `$((` to bash.
        assert_ne!(
            judge(
                "echo $(\\\n(1<<'2'\n$(git push origin HEAD:main)\n2\n))",
                &c,
                &F
            ),
            Verdict::Allow
        );
        // A shell reading a heredoc or a pipe, whatever other words it is given.
        for cmd in [
            "cat <<'EOF' | bash 2>&1\ngit push origin HEAD:main\nEOF",
            "cat <<'EOF' | bash --\ngit push origin HEAD:main\nEOF",
            "cat <<'EOF' | bash -o pipefail\ngit push origin HEAD:main\nEOF",
            "cat <<'EOF' | bash +x\ngit push origin HEAD:main\nEOF",
            "cat <<'EOF' | bash -es a\ngit push origin HEAD:main\nEOF",
            "cat <<'EOF' | bash /dev/stdin\ngit push origin HEAD:main\nEOF",
            "echo 'git push origin HEAD:main' | bash 2>&1",
            "echo hju qvti psjhjo IFBE:nbjo | tr b-zB-Z a-yA-Y | bash -o pipefail",
            "{ bash +x; } <<'EOF'\ngit push origin HEAD:main\nEOF",
            "while read -r l; do bash +x; done <<'EOF'\nfirst\ngit push origin HEAD:main\nEOF",
            "if true; then bash +x; fi <<'EOF'\ngit push origin HEAD:main\nEOF",
            "( bash +x ) <<< 'git push origin HEAD:main'",
            "echo 'git push origin HEAD:main' | (true; bash +x)",
            "printf 'x\ngit push origin HEAD:main\n' | { read x; bash -o pipefail; }",
            "bash +x < <(echo 'git push origin HEAD:main')",
            "bash <(echo 'git push origin HEAD:main')",
            "echo 'git push origin HEAD:main' |& bash +x",
            "echo 'git push origin HEAD:main' |\nbash +x",
        ] {
            assert_ne!(judge(cmd, &c, &F), Verdict::Allow, "{cmd:?}");
        }
        // An fd number before a redirection is not an argument; a quoted number is.
        assert_eq!(
            judge("git push origin feat-x 2>/dev/null", &c, &F),
            Verdict::Allow
        );
        for cmd in [
            "git push \"1\">x HEAD:main",
            "git push origin \\1>x HEAD:main",
            "git push origin ''1>x HEAD:main",
            "git push origin $'1'>x HEAD:main",
            "git push origin 1\"\">x HEAD:main",
            "git push --repo 2147483648>/dev/null origin HEAD:main",
            "git push -o 2147483648>/dev/null origin HEAD:main",
        ] {
            assert_ne!(judge(cmd, &c, &F), Verdict::Allow, "{cmd:?}");
        }
    }

    #[test]
    fn heredoc_text_is_skipped_and_what_follows_is_read() {
        let c = ctx(true);
        for cmd in [
            "cat <<'EOF'\ngit push origin HEAD:main\nEOF\ngit push origin HEAD:main",
            "cat <<-EOF\n\tx\n\tEOF\ngit push origin HEAD:main",
            "cat <<EOF; git push origin HEAD:main\nx\nEOF",
            "cat <<< \"$(git push origin HEAD:main)\"",
            "cat <<A <<B\na\nA\nb\nB\ngit push origin HEAD:main",
        ] {
            assert_eq!(rule(judge(cmd, &c, &F)), "task_repo", "{cmd:?}");
        }
        for cmd in [
            // Unquoted text is expanded: a substitution in it runs.
            "cat <<EOF\n$(git push origin HEAD:main)\nEOF",
            "cat <<EOF\n`git push origin HEAD:main`\nEOF",
            // No end line.
            "cat <<EOF\ngit push origin HEAD:main",
            // A delimiter that only looks like the end line.
            "cat <<'EOF'\n EOF\ngit push origin HEAD:main",
        ] {
            assert_ne!(judge(cmd, &c, &F), Verdict::Allow, "{cmd:?}");
        }
        assert_eq!(
            judge("cat <<'EOF'\ngit push origin HEAD:main\nEOF", &c, &F),
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

    /// A machine like the one the guard runs on: `/h/git/app` is a checkout of `owner/app` on
    /// `main`, the worktrees under `/h/git/app-worktrees/` are on feature branches, and the notes,
    /// inbox and temporary folders are no repository.
    struct Machine;

    /// A folder in the corpus's Git Bash form, whichever way the guard spelled it.
    fn norm(p: &Path) -> String {
        let s = p.to_string_lossy().replace('\\', "/");
        let tmp = std::env::temp_dir().to_string_lossy().replace('\\', "/");
        let tmp = tmp.trim_end_matches('/');
        if let Some(rest) = s.strip_prefix(tmp) {
            return format!("/tmp{rest}");
        }
        match s.as_bytes() {
            [d, b':', ..] => format!("/{}{}", (*d as char).to_ascii_lowercase(), &s[2..]),
            _ => s,
        }
    }

    impl RepoFacts for Machine {
        fn repo(&self, cwd: &Path) -> Option<String> {
            norm(cwd)
                .starts_with("/h/git/app")
                .then(|| "git.example/owner/app".into())
        }
        fn current_branch(&self, cwd: &Path) -> Option<String> {
            let p = norm(cwd);
            if p.starts_with("/h/git/app-worktrees/") {
                Some("u/feature".into())
            } else if p.starts_with("/h/git/app") {
                Some("main".into())
            } else {
                None
            }
        }
        fn default_branch(&self, cwd: &Path) -> Option<String> {
            self.repo(cwd).map(|_| "main".into())
        }
        fn not_a_repo(&self, cwd: &Path) -> bool {
            let p = norm(cwd);
            ["/tmp", "/h/notes", "/h/inbox"]
                .iter()
                .any(|d| p.starts_with(d))
                || p == "/h"
        }
        fn upstream(&self, cwd: &Path) -> Option<String> {
            self.current_branch(cwd).map(|b| format!("origin/{b}"))
        }
    }

    /// The three places a command is judged: a chat whose task is on `owner/app` (another chat
    /// tracks a branch there with no PR yet), a chat with no task while that work is tracked, and
    /// nothing tracked at all.
    fn places(cwd: &str) -> [Ctx; 3] {
        let tracked = vec![Tracked {
            repo: "git.example/owner/app".into(),
            branch: "u/other-chat".into(),
            pr: None,
        }];
        let base = Ctx {
            cwd: PathBuf::from(cwd),
            protected: vec!["main".into(), "master".into()],
            home: Some(PathBuf::from("/h")),
            ..Default::default()
        };
        [
            Ctx {
                task_active: true,
                task_repos: vec!["git.example/owner/app".into()],
                tracked: tracked.clone(),
                ..base.clone()
            },
            Ctx {
                tracked,
                ..base.clone()
            },
            base,
        ]
    }

    /// Plain Python that edits a file.
    const PY: &str = "import pathlib\np = pathlib.Path('src/app.ts')\ns = p.read_text()\ns = s.replace('oldName', 'newName')\np.write_text(s)\nprint('edited')";
    /// The same, editing text that names git.
    const PY_GIT: &str = "import re, pathlib\np = pathlib.Path('docs/notes.md')\ns = p.read_text()\ns = re.sub(r'git log -1', 'git log --oneline -1', s)\np.write_text(s)";
    /// Reading a file through a folder named git.
    const PY_PATH: &str = "import json, sys\nfrom pathlib import Path\nd = json.loads(Path('/h/git/app/package.json').read_text())\nprint(d['name'], sys.argv[1:])";
    /// Python the guard does not read (a module outside the file-editing list).
    const PY_OTHER: &str = "import platform\nprint(platform.node())";

    fn heredoc(lead: &str, py: &str, tail: &str) -> String {
        format!("{lead} <<'EOF'\n{py}\nEOF\n{tail}")
    }

    /// The landing guard's regression corpus: real commands an orchestrator ran in two days of
    /// work, each judged in the three places above. `A`/`D` is allow or deny in the task chat and
    /// in the chat without a task; with nothing tracked every one is allowed. Almost all are
    /// harmless and must pass; the few that stay refused run code the guard can't read in a
    /// checkout with work at stake, or land for real. Rows marked `later` keep a refusal the guard
    /// is meant to drop once it reads more (a variable set after `&&`, a merge on a detached
    /// checkout).
    #[test]
    fn the_regression_corpus_of_real_commands() {
        let wt = |n: u32| format!("/h/git/app-worktrees/wt{n}");
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let pkgs = "build-essential pkg-config curl file libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libxdo-dev libssl-dev librsvg2-dev lsof";
        let dpkg_loop = format!("for p in {pkgs}; do dpkg -s $p >/dev/null 2>&1 && echo \"ok $p\" || echo \"MISSING $p\"; done");
        let ssh_prefix = "cd /tmp/drafts\ncat > a.txt <<'EOF'\nline one\nline two\nEOF\ncat > c.txt <<'EOF'\nthird\nEOF\nfor b in a c; do timeout 90 box-ssh box-$b -- 'cat > ~/inbox/note.txt' < ${b}.txt && echo sent $b; done";
        let perl_lines = "perl -0pi -e 's/fn old_name/fn new_name/g' mod.rs\nperl -0pi -e 's/old_name\\(/new_name(/g' tabs.rs\nperl -0pi -e 's{use crate::old}{use crate::new}g' lib.rs";
        // (folder, command, verdict in the task chat and in the chat without one, later)
        let rows: Vec<(String, String, &str, bool)> = vec![
            // Python that edits files, in the task's own worktrees.
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(1)), PY, "git diff src/app.ts"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(2)), PY, "grep -n \"rule\\[\\\"method\\\"\\] = 'x'\" src/rules.ts"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {}/src-tauri/src && python3 -", wt(3)), PY, "grep -n \"fn handle_request\" main.rs | head"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(4)), PY, ""), "AA", false),
            (wt(5), "timeout 5 curl -s -o /dev/null -w \"%{http_code}\\n\" http://127.0.0.1:47830/api/ready; python3 -c \"import json, sys; print(json.dumps({'ok': True}))\"".into(), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(6)), PY, "cat >> src/extra.ts <<'EOF'\nexport const x = 1;\nEOF\nnode --check src/extra.ts && echo ok"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {}/src-tauri/src; python3 -", wt(7)), PY, "grep -n \"pub fn spawn_worker\" -A5 lib.rs | head"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(8)), PY, ""), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(9)), PY, "sed -n 8,22p src/a.ts; sed -n 30,60p src/a.ts"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(10)), PY, ""), "AA", false),
            // Package checks: a `for` word list is data, and dpkg only looks packages up.
            ("/h/git/app".into(), format!("cd {} && {dpkg_loop}; rustc --version; node --version; /usr/bin/python3 --version; python3 -c 'import sys; print(sys.version)'; which uv", wt(11)), "AA", false),
            ("/h/git/app".into(), format!("{dpkg_loop}; rustc --version; node --version; /usr/bin/python3 --version; which uv"), "AA", false),
            ("/h/git/app".into(), format!("dpkg -s {pkgs} 2>&1 | grep -E \"^(Package|Status):\""), "AA", false),
            ("/h/git/app".into(), heredoc("python3 -", PY, "echo ok"), "AA", false),
            // A gh read with a host setting, and remote shells that only list folders.
            ("/h/git/app".into(), "cd ~/git/app; GH_HOST=git.example gh pr list -R owner/app --state open --json number,title,headRefName,isDraft,updatedAt --jq '.[] | [.number, .title] | @tsv'; timeout 60 box-ssh box-a -- 'ls -R ~/notes | head -40; ls ~/inbox ~/git 2>&1' 2>&1".into(), "AA", false),
            ("/h/git/app".into(), "timeout 60 box-ssh box-a -- 'cd ~/notes && ls -la ~/git' 2>&1".into(), "AA", false),
            ("/h/git/app".into(), "timeout 60 box-ssh box-a -- 'cd ~/notes && git -C ~/notes status --short; ls ~/git' 2>&1".into(), "AA", false),
            ("/h/git/app".into(), "timeout 60 box-ssh box-a -- 'cat ~/notes/todo.md; ls ~/git' 2>&1 | sed -n 1,60p".into(), "AA", false),
            ("/h/inbox".into(), heredoc(&format!("cd {}/src-tauri/src && python3 -", wt(12)), PY, "sed -n 1050,1085p lib.rs; grep -n 'fn open_window' -r . | head"), "AA", false),
            // Code the guard does not read, run only in folders that are no repository.
            (wt(12), format!("{ssh_prefix}\ncd /h/notes && python3 - <<'EOF'\n{PY_OTHER}\nEOF"), "AA", false),
            (wt(12), format!("{ssh_prefix}\nperl -0pi -e 's/Draft one/Draft two/g' /h/notes/plan.md; grep -c O1 /h/notes/plan.md"), "AA", false),
            ("/h/git/app".into(), heredoc("cd /tmp && python3 -", PY_PATH, ""), "AA", false),
            // The one real landing: a squash merge of the task repository's PR.
            ("/tmp/orch".into(), format!("GH_HOST=git.example gh pr merge 176 -R owner/app --squash --match-head-commit {sha}"), "DA", false),
            ("/h/inbox".into(), format!("python3 - <<'EOF'\n{PY_OTHER}\nEOF\ncat > ~/inbox/$(date -u +%Y%m%dT%H%M%S)-note.md <<'EOF'\n# Note\nbody\nEOF\necho ok"), "AA", false),
            ("/h/notes/plans".into(), "cat >> log.md <<'EOF'\nentry\nEOF\npython3 - <<'EOF'\nimport platform\nprint(platform.system())\nEOF".into(), "AA", false),
            ("/h/notes/plans".into(), heredoc(&format!("cd {} && python3 -", wt(13)), PY_GIT, "grep -n \"export function\" -A20 src/app.ts"), "AA", false),
            ("/h/notes/plans".into(), heredoc(&format!("cd {} && python3 -", wt(14)), PY, "grep -n \"const rows = useRows\" src/app.ts"), "AA", false),
            ("/h/notes/plans".into(), heredoc(&format!("cd {} && python3 -", wt(15)), PY, ""), "AA", false),
            // Node code that runs git itself, in a checkout with work at stake: stays refused.
            ("/h/inbox".into(), format!("cd {}; cat > /dev/null <<'EOF'\nEOF\nnode -e '\nconst {{execFileSync:x}}=require(\"child_process\");\nconst revs=process.argv.slice(1);\nfor(const r of revs){{\n const files=x(\"git\",[\"ls-tree\",\"-r\",\"--name-only\",r,\"src/\"]).toString().split(\" \");\n console.log(r, files.length);\n}}' HEAD", wt(16)), "DD", false),
            ("/h/inbox".into(), format!("cd {} && node -e 'const cp=require(\"child_process\"); for (const r of process.argv.slice(1)) console.log(cp.execFileSync(\"git\",[\"show\",\"--stat\",r]).toString())' abc123 def456 HEAD", wt(16)), "DD", false),
            ("/h/git/app".into(), "mkdir -p /h/notes/boxes && cd /h/notes/boxes && C=remote-cli\n$C ssh box-c -- cat /h/notes/a.md > a.md\n$C ssh box-c -- cat /h/notes/b.md > b.md\n$C ssh box-d -- cat /h/notes/c.md > c.md\nwc -l *.md".into(), "AA", true),
            ("/h/git/app".into(), "cd /h/git/app; gh pr view 170 --json title,body,comments,reviews > /tmp/pr170.json; python3 -c \"\nimport json;d=json.load(open('/tmp/pr170.json'));print(d['title']);print(d['body'][:4000]);print('COMMENTS',len(d['comments']))\nfor c in d['comments']: print('---',c['author']['login'],c['createdAt']);print(c['body'][:5000])\n\"".into(), "AA", false),
            ("/h/git/app".into(), format!("cd /h/notes/plans; python3 -I - <<'EOF'\n{PY_OTHER}\nEOF\ngrep -n \"focus\" -A2 plan.md"), "AA", false),
            ("/h/git/app".into(), format!("cd /h/notes/plans && cp a.md b.md && python3 - <<'PY'\n{PY_GIT}\nPY\ngrep -n \"git log\" b.md"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(17)), PY, "node --test src/app.test.ts 2>&1 | grep -E '^(not ok|# pass|# fail)'"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(18)), PY_GIT, "git diff src/app.ts; grep -n 't=$(rp' src/app.ts"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(19)), PY, "node --test src/a.test.ts 2>&1 | grep -E \"^not ok|actual|expected\" | head -30"), "AA", false),
            ("/h/git/app".into(), format!("cd {} && python3 - <<'PY'\n{PY}\nPY\ngit diff --stat -- src/app.ts", wt(20)), "AA", false),
            ("/h/git/app".into(), heredoc("python3 -I -", PY_PATH, &format!("cd {} && sed -n 82,104p src/app.ts", wt(18))), "AA", false),
            ("/h/git/app".into(), format!("cd /tmp/review; python3 -I - <<'EOF'\n{PY_OTHER}\nEOF\ngrep -n -i \"board\\|TESTING\\|amendment\" *.new; sed -n 1,40p 194.new"), "AA", false),
            // A remote command with a substitution, fed from a file: stays refused.
            ("/h/git/app".into(), "printf 'line one\\nline two\\n' > /tmp/msg.txt; ~/.remote/box-ssh box-d -- 'cat > ~/inbox/msg.txt && echo \"$(date) received\"' < /tmp/msg.txt && echo sent".into(), "DD", false),
            ("/h/git/app".into(), heredoc("cd ~/git/app-worktrees/wt21/src-tauri/src/browser/tests; python3 -", PY, "git diff --stat"), "AA", false),
            // Inline node code in a worktree of the task's repository: stays refused.
            ("/h/git/app".into(), format!("cd {} && node -e \"const l=require('lucide-react'); console.log(Object.keys(l).length)\" && cat > src/icons.ts <<'EOF'\nexport const icons = [];\nEOF", wt(22)), "DD", false),
            ("/h/git/app".into(), format!("cd ~/git/app-worktrees/wt21/src-tauri/src/browser; cat > /tmp/empty.txt <<'EOF'\nEOF\n{perl_lines}"), "AA", false),
            ("/h/git/app".into(), format!("cd ~/git/app-worktrees/wt21/src-tauri/src/browser\n{perl_lines}"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && sed -i \"s/oldTitle/newTitle/g\" src/app.ts && python3 -", wt(22)), PY_GIT, "git diff src/app.ts | head -30; npx tsc -b 2>&1 | head -30"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && sed -i \"s/oldTitle/newTitle/g\" src/app.ts && python3 -", wt(22)), PY_GIT, "grep -n \"newTitle\" src/app.ts"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(23)), PY, "git diff --stat"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(24)), PY, &format!("git -C {} diff --stat src/a.rs; sed -n 1188,1194p src/a.rs | cut -c1-200", wt(24))), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(24)), PY, "sed -n 1188,1192p src/a.rs | cut -c1-160"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd /tmp/rv-main && git checkout -q --detach origin/main && git merge -q --no-edit {sha} 2>&1 | tail -2; git log --oneline -1; python3 -I -"), PY, "git diff 0123abc origin/main -- src/a.rs src/b.rs | head -60; for f in src/a.rs src/b.rs; do rustfmt --edition 2021 --check $f; done"), "AA", true),
            ("/h/git/app".into(), format!("cd /tmp && mkdir -p prbody && gh pr view 195 --json body -q .body > prbody/195.md && python3 -I - <<'E'\n{PY_OTHER}\nE\ngrep -n \"Testing\" prbody/195.md; gh pr edit 195 --body-file prbody/195.md && git -C /h/git/app ls-remote origin u/sidebar-folder-subtitle"), "AA", false),
            // A local fast-forward of main to its own upstream lands nothing.
            ("/h/git/app".into(), "git status --short | head; git branch --show-current; git fetch origin 2>&1 | tail -2; git merge --ff-only origin/main 2>&1 | tail -2; git log --oneline -3; df -h /h; git show origin/main:src/app.ts | awk 'NR<=20 {print NR\": \"$0}'".into(), "AA", false),
            ("/h/git/app".into(), format!("df -h /tmp; rustc --version; cargo --version; node --version; python3 -c 'import sys; print(sys.version_info[:2])'; which uv; ls ~/.local/bin 2>/dev/null|head; {dpkg_loop}"), "AA", false),
            ("/h/git/app".into(), format!("df -h /tmp; rustc --version; cargo --version; node --version; which uv; ls ~/.local/bin 2>/dev/null|head; {dpkg_loop}; ldd --version|head -1"), "AA", false),
            ("/h/git/app".into(), "dpkg -s build-essential pkg-config curl file 2>&1 | grep -E '^(Package|Status|Version):'".into(), "AA", false),
            ("/h/git/app".into(), "dpkg-query -W -f='${Package} ${Status}\\n' libwebkit2gtk-4.1-dev curl 2>&1".into(), "AA", false),
            ("/h/git/app".into(), "cd /tmp && dpkg-query -W -f='${Package} ${Status}\\n' libwebkit2gtk-4.1-dev curl 2>&1".into(), "AA", false),
            ("/h/git/app".into(), "cd /tmp && dpkg -l libwebkit2gtk-4.1-dev curl file 2>&1 | tail -12".into(), "AA", false),
            ("/h/git/app".into(), format!("mkdir -p /tmp/prs && cd /tmp/prs\ngh pr view 198 --json body -q .body > 198.md\ngh pr view 201 --json body -q .body > 201.md\npython3 -I - <<'EOF'\n{PY}\nEOF\ngrep -nE \"A[0-9]\\b|plan|Lane|builder|board\" 198.md 201.md"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(21)), PY, "git diff --stat; CARGO_TARGET_DIR=/h/build-cache cargo test --manifest-path src-tauri/Cargo.toml --lib browser::tests::agent_pages 2>&1 | grep -E \"test result|FAILED\" | head -30; git checkout src/a.ts; git status --short"), "AA", false),
            ("/h/git/app".into(), format!("cd {} && diff <(git diff origin/main abc1234 | grep -v '^index' | grep -v '^@@') <(git diff origin/main def5678 | grep -v '^index'| grep -v '^@@') | head; git diff origin/main abc1234 | git patch-id --stable; echo markers-done; git diff origin/main abc1234 -- src/a.ts src/b.ts", wt(21)), "AA", false),
            ("/h/git/app".into(), format!("cd /h/notes && python3 - <<'EOF'\n{PY_OTHER}\nEOF"), "AA", false),
            // awk regexes and strings are text.
            ("/h/git/app".into(), "mkdir -p /tmp/apt && cd /tmp/apt && for s in jammy-updates jammy-security; do curl -sS --max-time 60 -o $s.gz https://artifacts.example/apt/dists/$s/main/binary-amd64/Packages.gz && echo \"fetched $s\"; done; for s in jammy-updates jammy-security; do zcat $s.gz | awk '/^Package: (libwebkit2gtk|libsoup)/ {p=$2} /^Version:/ && p {print p, $2; p=\"\"}' | sort -u; done".into(), "AA", false),
            // Inline node code in the tracked main checkout: stays refused.
            ("/h/git/app".into(), "cd /h/git/app/scripts; ls a.json b.json 2>&1; head -5 rows.json | cut -c1-200; cat > /tmp/empty.json <<'EOF'\n{}\nEOF\nnode -e '\nconst rows=require(\"./rows.json\");\nconst otherRunner=new Set([\"native-scroll\"]);\nconsole.log(rows.filter(r=>!otherRunner.has(r.name)).length);\n'".into(), "DD", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(25)), PY, ""), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {}; python3 -", wt(26)), PY_PATH, "git diff --stat; node --check src/a.js && node --check src/b.js && echo syntax-ok"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(27)), PY, "git diff src/app.ts | head -60"), "AA", false),
            // `sed -f FILE` runs a script file, which the guard leaves alone.
            ("/h/git/app".into(), format!("cd {} && cat > /tmp/edit.sed <<'EOF'\ns/git log/git log --oneline/\nEOF\nsed -i -f /tmp/edit.sed docs/notes.md && grep -n \"note\" docs/notes.md", wt(27)), "AA", false),
            ("/h/git/app".into(), "cd /tmp/scratch && rm -rf u && mkdir u && cd u && git init -q -b main && mkdir pkg && printf 'staged\\n' > pkg/a.txt && git add pkg && printf 'unstaged\\n' >> pkg/a.txt && git status --porcelain=v1 -z | tr '\\0' '|'; echo".into(), "AA", false),
            ("/h/git/app".into(), "sleep 5; curl -s -o /dev/null -w \"%{http_code}\\n\" \"http://127.0.0.1:47830/\"; for c in a b; do echo \"$c: $(python3 -I -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d[\"ok\"])' /tmp/runner/base/$c.json 2>&1 | cut -c1-400)\"; done".into(), "AA", false),
            // Python that opens a connection, in the tracked main checkout: stays refused.
            ("/h/git/app".into(), "python3 -c \"\nimport urllib.request;print(urllib.request.urlopen('http://127.0.0.1:5173/').status)\n\" && curl -s 'http://127.0.0.1:5173/src/main.tsx?import' | head -c 300".into(), "DD", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(28)), PY, "npx prettier --version >/dev/null 2>&1; sed -n '/export function/,/^}/p' src/a.ts"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(25)), PY, "sed -n 55,70p src/a.ts"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && /usr/bin/python3 -I -B -", wt(28)), PY_PATH, "timeout 300 nice -n 10 node --test --test-name-pattern=\"renders rows\" src/a.test.ts 2>&1 | grep -E \"^(ok|not ok)\""), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(27)), PY_GIT, ""), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(27)), PY_GIT, ""), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(29)), PY, "node --test src/a.test.ts src/b.test.ts 2>&1 | tail -40"), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(28)), PY, "grep -n \"useWorkflow\" -A2 src/app.ts"), "AA", false),
            // A commit message from an unquoted heredoc, which bash expands: stays refused.
            ("/h/git/app".into(), format!("cd {} && git reset -q && T=\"fix: the sidebar subtitle\" && git add src/a.ts src/b.ts && git commit -q -F - <<EOF\n$T\n\nKeeps git status quiet.\nEOF\ngit add src/c.ts && git commit -q -m \"second\"", wt(27)), "DD", false),
            ("/h/git/app".into(), "sed -i 's/see PR 12/see PR #12/; s/PR 7 /PR #7 /' docs/CHANGES.md; grep -n \"PR #\" docs/CHANGES.md | head".into(), "AA", false),
            ("/h/git/app".into(), format!("set -e; rm -rf /tmp/rt/tree; mkdir -p /tmp/rt/tree; cd {} && git archive HEAD | tar -x -C /tmp/rt/tree; ln -s /h/git/app/node_modules /tmp/rt/tree/node_modules; cd /tmp/rt && node -e '\nconst fs=require(\"fs\");const T=\"tree/manifest.json\";\nconst m=JSON.parse(fs.readFileSync(T));console.log(Object.keys(m).length);\n'", wt(28)), "AA", false),
            ("/h/git/app".into(), format!("set -e; rm -rf /tmp/rt/tree; mkdir -p /tmp/rt/tree; git -C {} archive {sha} | tar -x -C /tmp/rt/tree; ln -s /h/git/app/node_modules /tmp/rt/tree/node_modules; cat > /tmp/rt/check.mjs <<'EOF'\nconsole.log(\"check\");\nEOF\ncd /tmp/rt/tree && node /tmp/rt/check.mjs && node -e 'console.log(require(\"./package.json\").name)'", wt(28)), "AA", false),
            ("/h/git/app".into(), "cd /tmp/plugin && sed -i \"s/\\\"version\\\": \\\"1\\\"/\\\"version\\\": \\\"2\\\"/\" manifest.json && node --input-type=module -e 'import(\"./loader.mjs\").then(m=>{const x=m.loadManifest(\"manifest.json\");console.log(m.validateManifest(x))})'".into(), "AA", false),
            ("/h/git/app".into(), format!("cd /tmp/infer/tools && python3 - <<'EOF'\n{PY_OTHER}\nEOF\ncd /tmp/infer && node infer.mjs /tmp/infer/repo out.tsv infer-table.tsv && grep -E \"^(src|lib)/\" infer-table.tsv | cut -f1,4 | head -60"), "AA", false),
            ("/h/git/app".into(), "git status --short | head -3; git merge --ff-only -q origin/main && git log --oneline -1; grep -n -A25 \"test:linux\" package.json | head -45; grep -n '\"test:linux\"' package.json".into(), "AA", false),
            // xargs running a git command that only reads.
            ("/h/git/app".into(), "cd /h/git/app; git fetch -q origin u/changes-sections; git log --oneline origin/main..origin/u/changes-sections; git merge-base origin/main origin/u/changes-sections | xargs git log --oneline -1; git merge-tree --write-tree --name-only origin/main origin/u/changes-sections | head -20".into(), "AA", false),
            ("/h/git/app".into(), "cd /h/git/app; T=$(git merge-tree --write-tree origin/main FETCH_HEAD | head -1); git show $T:src/app.ts | awk '/<<<|>>>|===/ {print NR\": \"$0}' | cut -c1-900".into(), "AA", false),
            // A folder set once at the start of the line is read as that folder.
            ("/h/git/app".into(), format!("W={}; git -C $W status --short | head; git -C $W log --oneline -6; git -C $W fetch origin && git -C $W merge origin/main 2>&1 | tail -5", wt(27)), "AA", false),
            ("/h/git/app".into(), format!("cat /h/notes/handoff.md; WT={}; ls $WT >/dev/null && git -C $WT status --short | head; git -C $WT fetch origin && git -C $WT merge --ff-only origin/main; git -C $WT log --oneline -3", wt(30)), "AA", false),
            ("/h/git/app".into(), format!("W={}; git -C $W status; git -C $W log --oneline -3; git -C $W fetch origin 2>&1|tail -1; git -C $W merge --ff-only origin/main 2>&1 | tail -2; git -C $W log --oneline -1; ls -la $W/node_modules", wt(31)), "AA", false),
            ("/h/git/app".into(), format!("W={}; ls $W/scripts/ui-check* 2>&1; git -C $W log --oneline -1 origin/main; git -C $W merge-base HEAD origin/main | xargs git -C $W log --oneline -1; git -C $W show origin/main:package.json | head -120", wt(32)), "AA", false),
            ("/h/git/app".into(), format!("cd {}; python3 - <<'PY'\n{PY_GIT}\nPY\ngit -C {} diff --stat src/a.ts", wt(30), wt(30)), "AA", false),
            ("/h/git/app".into(), format!("cd {}; python3 - <<'PY'\n{PY_GIT}\nPY", wt(30)), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {}; python3 -", wt(33)), PY, ""), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {}/src-tauri/src && python3 -", wt(32)), PY_GIT, "grep -n \"fn route_request\" -A22 lib.rs | head -60"), "AA", false),
            ("/h/git/app".into(), format!("cd {} && python3 - <<'PY'\n{PY_PATH}\nPY\ngrep -n \"fn route_request\" lib.rs", wt(31)), "AA", false),
            ("/h/git/app".into(), heredoc(&format!("cd {} && python3 -", wt(34)), PY, "grep -n \"const hash\" src/app.ts"), "AA", false),
            ("/h/git/app".into(), format!("cd /tmp/golden && python3 - <<'PY'\n{PY_GIT}\nPY\ngrep -n '^## \\|golden' notes.md | head -30"), "AA", false),
            ("/h/git/app".into(), format!("cd {} && sed -i \"s/expected: 3/expected: 4/\" src/a.test.ts && node -e \"const d=require('./report.json');console.log(d.pass,d.failures)\"", wt(34)), "AA", false),
            ("/h/git/app".into(), format!("cd /tmp/prs && python3 - <<'EOF'\n{PY_OTHER}\nEOF\ncd {} && gh pr edit 204 --body-file /tmp/prs/204.md >/dev/null && echo updated; git -C {} rev-parse HEAD", wt(31), wt(31)), "AA", false),
            ("/h/git/app".into(), format!("W={}; git -C $W push -q -u origin u/wt30-followups 2>&1 | tail -2; gh label list -R owner/app 2>/dev/null | grep -iE \"review|draft\"", wt(30)), "AA", false),
        ];
        let v = |v: &Verdict| if *v == Verdict::Allow { 'A' } else { 'D' };
        let mut reached = 0;
        let mut wrong = Vec::new();
        for (n, (cwd, cmd, want, later)) in rows.iter().enumerate() {
            let [run, outside, free] = places(cwd);
            let (a, b) = (judge(cmd, &run, &Machine), judge(cmd, &outside, &Machine));
            let got: String = [v(&a), v(&b)].iter().collect();
            // Still refused today when `later`; flip the row once the guard reads that shape.
            let expect = if *later { "DD" } else { want };
            if got != expect || judge(cmd, &free, &Machine) != Verdict::Allow {
                wrong.push(format!(
                    "row {}: got {got}, want {expect}: {a:?} / {b:?}
{cmd}",
                    n + 1
                ));
            } else if !later {
                reached += 1;
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n\n"));
        assert_eq!(rows.len(), 102);
        assert_eq!(reached, 100);
    }

    #[test]
    fn programs_that_run_commands_and_other_ways_to_land_are_read() {
        let c = ctx(true);
        // Each lands, or may land, in the task's repository.
        let lands = [
            "git pm",
            "git-push origin HEAD:main",
            "git -c core.sshCommand=x push origin feat-x",
            "git checkout main && git merge --continue",
            "curl -XPUT -d x https://api.github.com/repos/o/r/pulls/7/merge",
            "curl --json '{}' https://api.github.com/repos/o/r/merges",
            "curl -dbase=main https://api.github.com/repos/o/r/merges",
            "curl -T body.json https://api.github.com/repos/o/r/contents/a",
            "curl -X \"$M\" https://api.github.com/repos/o/r/pulls/7/merge",
            "gh pr edit 7 --remove-label=needs-review",
            "gh issue edit 7 --remove-label needs-review",
            "gh pr merge -Ro/r 9",
            "gh pr merge -R o/free https://github.com/o/r/pull/9",
            "echo x | xargs git push origin",
            "echo x | xargs -I{} git push origin {}:main",
            "echo x | xargs -I{} git {} log",
            "echo x | xargs sh -c 'git push origin HEAD:main'",
            "find . -exec git push origin HEAD:main \\;",
            "watch git push origin HEAD:main",
            "flock /tmp/l git push origin HEAD:main",
            "awk 'BEGIN { system(\"git push origin HEAD:main\") }'",
            "awk 'BEGIN { print \"x\" | \"sh\" }' # push",
            "sed -n '1e git push origin HEAD:main' f",
            "sed 's/x/git push origin HEAD:main/e' f",
            "perl -we 'system(q(git push origin HEAD:main))'",
            "python -Ic 'import os; os.system(\"git push origin HEAD:main\")'",
            "node --eval 'require(\"child_process\").execSync(\"git push origin HEAD:main\")'",
            "python3 -c 'import subprocess; subprocess.run([\"git\", \"push\"])'",
            "python3 - <<'EOF'\nimport os as o\no.system('true')\nEOF",
            "ssh box 'cd repo && git push origin HEAD:main'",
            "PERL5OPT=-Mx perl -pi -e 's/a/b/' f && git push origin feat-x",
        ]
        .into_iter()
        .filter(|cmd| judge(cmd, &c, &F) == Verdict::Allow)
        .collect::<Vec<_>>();
        assert!(lands.is_empty(), "allowed: {lands:#?}");
        let refused = [
            "git status && git push origin feat-x",
            "echo x | xargs git log --oneline -1",
            "echo x | xargs -I{} git log {} --oneline",
            "find . -name '*.rs' -exec grep -l push {} \\;",
            "awk '/push|merge/ { n++ } END { print n }' log.txt",
            "sed -i 's/push/pull/g' notes.md",
            "perl -0pi -e 's/push the branch/pull the branch/g' notes.md",
            "python3 -c 'import re, sys; print(re.sub(\"push\", \"pull\", sys.argv[1]))' x",
            "command -v git && git push origin feat-x",
            "ls ~/git && git push origin feat-x",
            "dpkg -s git curl | grep Status && git push origin feat-x",
            "ssh box 'cd repo && git log -1' && git push origin feat-x",
            "gh pr merge 9 --repo o/free",
        ]
        .into_iter()
        .map(|cmd| (cmd, judge(cmd, &c, &F)))
        .filter(|(_, v)| *v != Verdict::Allow)
        .collect::<Vec<_>>();
        assert!(refused.is_empty(), "refused: {refused:#?}");
        // Code the guard doesn't read is at stake only in the folder it runs in.
        let unread = "python3 - <<'EOF'\nimport platform\nprint(platform.node())\nEOF";
        assert_eq!(rule(judge(unread, &c, &F)), "fail_closed");
        let elsewhere = Ctx {
            tracked: vec![],
            task_repos: vec!["github.com/o/other".into()],
            ..ctx(true)
        };
        assert_eq!(judge(unread, &elsewhere, &F), Verdict::Allow);
        // `~` is the home folder; a folder set once on the line is read as that folder.
        let home = Ctx {
            home: Some(PathBuf::from("C:/h")),
            ..ctx(false)
        };
        assert_eq!(
            rule(judge(
                "cd ~/tracked && git push origin trunk",
                &home,
                &ByFolder
            )),
            "containment"
        );
        assert_eq!(
            rule(judge(
                "D=tracked; git -C $D push origin trunk",
                &home,
                &ByFolder
            )),
            "containment"
        );
        assert_eq!(
            judge("D=free; git -C $D push origin trunk", &home, &ByFolder),
            Verdict::Allow
        );
        // Set after `&&`, or more than once, a variable stays computed.
        for cmd in [
            "true && D=free; git -C $D push origin trunk",
            "D=free; D=tracked; git -C $D push origin trunk",
            "D=free; read D; git -C $D push origin trunk",
        ] {
            assert_eq!(rule(judge(cmd, &home, &ByFolder)), "fail_closed", "{cmd}");
        }
        // PowerShell: plain lines are read; anything PowerShell reads its own way fails closed.
        let ps = |cmd| judge_as(cmd, ShellKind::PowerShell, &c, &F);
        assert_eq!(rule(ps("git push origin HEAD:main")), "task_repo");
        assert_eq!(ps("git push origin feat-x"), Verdict::Allow);
        assert_eq!(rule(ps("git push origin $env:B")), "fail_closed");
        assert_eq!(ps("Get-ChildItem $env:TEMP"), Verdict::Allow);
    }
}
