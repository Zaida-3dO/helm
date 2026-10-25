//! Settings → Orchestrator: defaults, strict validation and the one-row store.
//!
//! - Validation walks the JSON itself so a refusal names the field path, and **unknown keys are
//!   refused at every level**. One bad field refuses the whole save.
//! - A stored value that no longer validates falls back to the defaults, never to something looser
//!   (and the defaults have the master switch off).
//! - The stored form leaves out what is at its quiet default: `moveFast` only while true,
//!   `landingGuard` only while false,
//!   `dispatchGate` only while false,
//!   `newChats` only when not `auto` (the default), `landing.method` only when
//!   not `merge`, so a value saved without those fields is byte-identical. A stored `always` is
//!   written out and read back as `always`. A value stored before the default changed, with no
//!   `newChats`, reads as the new default; chats already used keep their pinned state either way.
//! - The save carries no revision: last writer wins.

use crate::types::*;
use rusqlite::{OptionalExtension, Transaction};
use serde_json::{Map, Value};

pub const MAX_ROUNDS: u32 = 20;
pub const DEFAULT_ROUNDS: u32 = 3;
pub const MAX_AGENT_LIST: usize = 32;
pub const MAX_REPOS: usize = 50;
pub const MAX_LABELS: usize = 8;
pub const MAX_LABEL_CHARS: usize = 50;
pub const MAX_PROTECTED: usize = 32;

pub fn init(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS workflow_settings (
             id         INTEGER PRIMARY KEY CHECK (id = 1),
             value      TEXT NOT NULL,
             updated_at INTEGER NOT NULL
         );",
    )
}

impl Default for WorkflowSettings {
    fn default() -> Self {
        let s = |use_: StageUse, agent: &str| PlanStage {
            use_,
            agent: agent.into(),
        };
        let r = |use_: StageUse, agent: &str| RoundStage {
            use_,
            agent: agent.into(),
            max_rounds: DEFAULT_ROUNDS,
        };
        WorkflowSettings {
            enabled: false,
            stages: Stages {
                plan: s(StageUse::Required, "planner"),
                plan_review: r(StageUse::Required, "plan-reviewer"),
                build: BuildStage {
                    agent: "coder".into(),
                    start_agents: vec!["coder".into(), "worker".into(), "general-purpose".into()],
                    build_exempt: Vec::new(),
                },
                review: r(StageUse::Required, "reviewer"),
                cleanup: s(StageUse::Required, "pr-cleanup"),
                smoketest: r(StageUse::Auto, "smoketester"),
            },
            orchestrator_edits: EditsRule::Deny,
            plan_sign_off: SignOff::Review,
            home_repos: Vec::new(),
            landing: Landing {
                default: LandMode::Draft,
                method: MergeMethod::Merge,
                repos: Vec::new(),
                draft_labels: Vec::new(),
                flip_label: None,
                protected_branches: vec!["main".into(), "master".into()],
            },
            move_fast: false,
            landing_guard: true,
            dispatch_gate: true,
            // Auto by default.
            new_chats: NewChats::Auto,
        }
    }
}

// ---- validation ---------------------------------------------------------------------------

/// Walks one JSON object: each `take` consumes a key, `done` refuses whatever is left.
struct Obj {
    path: String,
    map: Map<String, Value>,
}

fn err(path: &str, msg: &str) -> String {
    format!("{path}: {msg}")
}

fn obj(path: &str, v: Option<Value>) -> Result<Option<Obj>, String> {
    match v {
        None => Ok(None),
        Some(Value::Object(map)) => Ok(Some(Obj {
            path: path.to_owned(),
            map,
        })),
        Some(_) => Err(err(path, "must be an object")),
    }
}

impl Obj {
    fn sub(&self, key: &str) -> String {
        if self.path.is_empty() {
            key.to_owned()
        } else {
            format!("{}.{key}", self.path)
        }
    }

    fn take(&mut self, key: &str) -> Option<Value> {
        self.map.remove(key)
    }

    fn done(self) -> Result<(), String> {
        match self.map.keys().next() {
            Some(k) => Err(err(&self.sub(k), "unknown setting")),
            None => Ok(()),
        }
    }

    fn bool(&mut self, key: &str, default: bool) -> Result<bool, String> {
        match self.take(key) {
            None => Ok(default),
            Some(Value::Bool(b)) => Ok(b),
            Some(_) => Err(err(&self.sub(key), "must be true or false")),
        }
    }

    fn word<T: Copy>(&mut self, key: &str, default: T, allowed: &[(&str, T)]) -> Result<T, String> {
        match self.take(key) {
            None => Ok(default),
            Some(Value::String(s)) => allowed
                .iter()
                .find(|(w, _)| *w == s)
                .map(|(_, t)| *t)
                .ok_or_else(|| {
                    let words: Vec<&str> = allowed.iter().map(|(w, _)| *w).collect();
                    err(&self.sub(key), &format!("must be {}", or_list(&words)))
                }),
            Some(_) => Err(err(&self.sub(key), "must be a word")),
        }
    }

    fn string(&mut self, key: &str) -> Result<Option<String>, String> {
        match self.take(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(err(&self.sub(key), "must be text")),
        }
    }

    fn strings(&mut self, key: &str) -> Result<Option<Vec<String>>, String> {
        match self.take(key) {
            None => Ok(None),
            Some(Value::Array(a)) => a
                .into_iter()
                .map(|v| match v {
                    Value::String(s) => Ok(s.trim().to_owned()),
                    _ => Err(err(&self.sub(key), "must be a list of text")),
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            Some(_) => Err(err(&self.sub(key), "must be a list")),
        }
    }

    fn rounds(&mut self, key: &str, default: u32) -> Result<u32, String> {
        match self.take(key) {
            None => Ok(default),
            Some(v) => v
                .as_u64()
                .filter(|n| (1..=MAX_ROUNDS as u64).contains(n))
                .map(|n| n as u32)
                .ok_or_else(|| err(&self.sub(key), "must be a whole number from 1 to 20")),
        }
    }
}

fn or_list(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [init @ .., last] => format!("{} or {last}", init.join(", ")),
    }
}

/// `^[a-z0-9][a-z0-9-]{0,63}$`.
pub fn is_agent_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `host/owner/name`, three non-empty parts of `[A-Za-z0-9._-]`. Returned lowercased.
pub fn normalize_repo(s: &str) -> Option<String> {
    let s = s.trim().to_ascii_lowercase();
    let parts: Vec<&str> = s.split('/').collect();
    let ok = parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && *p != "."
                && *p != ".."
                && p.len() <= 100
                && p.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        })
        && parts[0].contains('.');
    ok.then_some(s)
}

/// A git branch name under the rules of `git check-ref-format --branch` that matter here.
pub fn is_branch_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && !s.starts_with('-')
        && !s.starts_with('/')
        && !s.ends_with('/')
        && !s.ends_with('.')
        && !s.ends_with(".lock")
        && !s.contains("..")
        && !s.contains("//")
        && !s.contains("@{")
        && s != "@"
        && !s.split('/').any(|p| p.starts_with('.'))
        && !s.chars().any(|c| {
            c.is_control() || c == ' ' || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
}

fn agent(o: &mut Obj, default: &str) -> Result<String, String> {
    match o.string("agent")? {
        None => Ok(default.to_owned()),
        Some(a) if is_agent_name(&a) && !a.contains("--") => Ok(a),
        Some(_) => Err(err(
            &o.sub("agent"),
            "must be an agent name of a-z, 0-9 and -, at most 64 characters, without --",
        )),
    }
}

const PLAN_USES: [(&str, StageUse); 3] = [
    ("required", StageUse::Required),
    ("auto", StageUse::Auto),
    ("off", StageUse::Off),
];

fn plan_stage(path: &str, v: Option<Value>, d: &PlanStage) -> Result<PlanStage, String> {
    let Some(mut o) = obj(path, v)? else {
        return Ok(d.clone());
    };
    let use_ = o.word("use", d.use_, &PLAN_USES)?;
    let agent = agent(&mut o, &d.agent)?;
    o.done()?;
    Ok(PlanStage { use_, agent })
}

fn round_stage(
    path: &str,
    v: Option<Value>,
    d: &RoundStage,
    uses: &[(&str, StageUse)],
) -> Result<RoundStage, String> {
    let Some(mut o) = obj(path, v)? else {
        return Ok(d.clone());
    };
    let use_ = o.word("use", d.use_, uses)?;
    let agent = agent(&mut o, &d.agent)?;
    let max_rounds = o.rounds("maxRounds", d.max_rounds)?;
    o.done()?;
    Ok(RoundStage {
        use_,
        agent,
        max_rounds,
    })
}

fn agent_list(o: &mut Obj, key: &str, default: &[String]) -> Result<Vec<String>, String> {
    let Some(list) = o.strings(key)? else {
        return Ok(default.to_vec());
    };
    let mut out: Vec<String> = Vec::new();
    for a in list {
        if !is_agent_name(&a) {
            return Err(err(&o.sub(key), &format!("{a:?} is not an agent name")));
        }
        if !out.contains(&a) {
            out.push(a);
        }
    }
    if out.len() > MAX_AGENT_LIST {
        return Err(err(&o.sub(key), "holds more than 32 names"));
    }
    Ok(out)
}

fn stages(v: Option<Value>, d: &Stages) -> Result<Stages, String> {
    let Some(mut o) = obj("stages", v)? else {
        return Ok(d.clone());
    };
    let plan = plan_stage("stages.plan", o.take("plan"), &d.plan)?;
    let plan_review = round_stage(
        "stages.planReview",
        o.take("planReview"),
        &d.plan_review,
        &PLAN_USES,
    )?;
    let build = match obj("stages.build", o.take("build"))? {
        None => d.build.clone(),
        Some(mut b) => {
            if b.map.contains_key("use") {
                return Err(err("stages.build.use", "build is always required"));
            }
            let agent = agent(&mut b, &d.build.agent)?;
            let start_agents = agent_list(&mut b, "startAgents", &d.build.start_agents)?;
            let build_exempt = agent_list(&mut b, "buildExempt", &d.build.build_exempt)?;
            b.done()?;
            BuildStage {
                agent,
                start_agents,
                build_exempt,
            }
        }
    };
    let review = round_stage(
        "stages.review",
        o.take("review"),
        &d.review,
        &[("required", StageUse::Required), ("off", StageUse::Off)],
    )?;
    let cleanup = plan_stage("stages.cleanup", o.take("cleanup"), &d.cleanup)?;
    let smoketest = round_stage(
        "stages.smoketest",
        o.take("smoketest"),
        &d.smoketest,
        &PLAN_USES,
    )?;
    o.done()?;
    let s = Stages {
        plan,
        plan_review,
        build,
        review,
        cleanup,
        smoketest,
    };
    // Each stage its own agent, checked in this order; the error names the earlier stage.
    let named = [
        ("plan", &s.plan.agent),
        ("planReview", &s.plan_review.agent),
        ("build", &s.build.agent),
        ("review", &s.review.agent),
        ("cleanup", &s.cleanup.agent),
        ("smoketest", &s.smoketest.agent),
    ];
    for (i, (stage, a)) in named.iter().enumerate() {
        if let Some((later, _)) = named[i + 1..].iter().find(|(_, b)| b == a) {
            return Err(err(
                &format!("stages.{later}.agent"),
                &format!("{a} is already the {stage} stage's agent; each stage needs its own"),
            ));
        }
    }
    Ok(s)
}

fn landing(v: Option<Value>, d: &Landing) -> Result<Landing, String> {
    let Some(mut o) = obj("landing", v)? else {
        return Ok(d.clone());
    };
    const MODES: [(&str, LandMode); 2] = [("draft", LandMode::Draft), ("merge", LandMode::Merge)];
    const METHODS: [(&str, MergeMethod); 3] = [
        ("merge", MergeMethod::Merge),
        ("squash", MergeMethod::Squash),
        ("rebase", MergeMethod::Rebase),
    ];
    let default = o.word("default", d.default, &MODES)?;
    let method = o.word("method", d.method, &METHODS)?;
    let repos = match o.take("repos") {
        None => d.repos.clone(),
        Some(Value::Array(a)) => {
            if a.len() > MAX_REPOS {
                return Err(err("landing.repos", "holds more than 50 rules"));
            }
            let mut out: Vec<RepoRule> = Vec::new();
            for (i, v) in a.into_iter().enumerate() {
                let path = format!("landing.repos[{i}]");
                let mut r = obj(&path, Some(v))?.expect("some");
                let repo = r
                    .string("repo")?
                    .and_then(|s| normalize_repo(&s))
                    .ok_or_else(|| err(&r.sub("repo"), "must be host/owner/name"))?;
                let mode = match r.take("mode") {
                    None => return Err(err(&r.sub("mode"), "is required")),
                    Some(v) => {
                        let mut tmp = Obj {
                            path: path.clone(),
                            map: Map::from_iter([("mode".to_owned(), v)]),
                        };
                        tmp.word("mode", LandMode::Draft, &MODES)?
                    }
                };
                let method = match r.take("method") {
                    None | Some(Value::Null) => None,
                    Some(v) => {
                        if mode == LandMode::Draft {
                            return Err(err(&r.sub("method"), "is allowed only on a merge rule"));
                        }
                        let mut tmp = Obj {
                            path: path.clone(),
                            map: Map::from_iter([("method".to_owned(), v)]),
                        };
                        Some(tmp.word("method", MergeMethod::Merge, &METHODS)?)
                    }
                };
                r.done()?;
                if out.iter().any(|x| x.repo == repo) {
                    return Err(err(&path, &format!("{repo} is named twice")));
                }
                out.push(RepoRule { repo, mode, method });
            }
            out
        }
        Some(_) => return Err(err("landing.repos", "must be a list")),
    };
    let draft_labels = match o.strings("draftLabels")? {
        None => d.draft_labels.clone(),
        Some(list) => {
            let mut out: Vec<String> = Vec::new();
            for l in list {
                if l.is_empty()
                    || l.chars().count() > MAX_LABEL_CHARS
                    || l.chars().any(char::is_control)
                {
                    return Err(err(
                        "landing.draftLabels",
                        "labels are 1-50 plain characters",
                    ));
                }
                if !out.contains(&l) {
                    out.push(l);
                }
            }
            if out.len() > MAX_LABELS {
                return Err(err("landing.draftLabels", "holds more than 8 labels"));
            }
            out
        }
    };
    let flip_label = match o.take("flipLabel") {
        None => d.flip_label.clone(),
        Some(Value::Null) => None,
        Some(Value::String(s)) if s.trim().is_empty() => None,
        Some(Value::String(s))
            if s.trim().chars().count() <= MAX_LABEL_CHARS && !s.chars().any(char::is_control) =>
        {
            Some(s.trim().to_owned())
        }
        Some(_) => {
            return Err(err(
                "landing.flipLabel",
                "must be a label of 1-50 characters or null",
            ))
        }
    };
    let protected_branches = match o.strings("protectedBranches")? {
        None => d.protected_branches.clone(),
        Some(list) => {
            let mut out: Vec<String> = Vec::new();
            for b in list {
                if !is_branch_name(&b) {
                    return Err(err(
                        "landing.protectedBranches",
                        &format!("{b:?} is not a branch name"),
                    ));
                }
                if !out.contains(&b) {
                    out.push(b);
                }
            }
            if out.len() > MAX_PROTECTED {
                return Err(err(
                    "landing.protectedBranches",
                    "holds more than 32 branches",
                ));
            }
            out
        }
    };
    o.done()?;
    Ok(Landing {
        default,
        method,
        repos,
        draft_labels,
        flip_label,
        protected_branches,
    })
}

/// Validate a whole settings value. Missing fields take their defaults; anything present must be
/// valid, and unknown keys are refused at every level.
pub fn validate(v: Value) -> Result<WorkflowSettings, String> {
    let d = WorkflowSettings::default();
    let Some(mut o) = obj("", Some(v)).map_err(|_| "settings must be an object".to_owned())? else {
        unreachable!()
    };
    let enabled = o.bool("enabled", d.enabled)?;
    let stages = stages(o.take("stages"), &d.stages)?;
    let orchestrator_edits = o.word(
        "orchestratorEdits",
        d.orchestrator_edits,
        &[("deny", EditsRule::Deny), ("allow", EditsRule::Allow)],
    )?;
    let plan_sign_off = o.word(
        "planSignOff",
        d.plan_sign_off,
        &[("review", SignOff::Review), ("me", SignOff::Me)],
    )?;
    let home_repos = match o.strings("homeRepos")? {
        None => d.home_repos.clone(),
        Some(list) => {
            let mut out: Vec<String> = Vec::new();
            for r in list {
                let n = normalize_repo(&r)
                    .ok_or_else(|| err("homeRepos", &format!("{r:?} is not host/owner/name")))?;
                if !out.contains(&n) {
                    out.push(n);
                }
            }
            if out.len() > MAX_REPOS {
                return Err(err("homeRepos", "holds more than 50 repositories"));
            }
            out
        }
    };
    let landing = landing(o.take("landing"), &d.landing)?;
    let move_fast = o.bool("moveFast", d.move_fast)?;
    let landing_guard = o.bool("landingGuard", d.landing_guard)?;
    let dispatch_gate = o.bool("dispatchGate", d.dispatch_gate)?;
    let new_chats = match o.take("newChats") {
        None => d.new_chats,
        Some(Value::String(s)) if s == "always" => NewChats::Always,
        Some(Value::String(s)) if s == "never" => NewChats::Never,
        Some(Value::String(s)) if s == "auto" => NewChats::Auto,
        Some(_) => return Err("newChats must be always, never or auto".into()),
    };
    o.done()?;
    Ok(WorkflowSettings {
        enabled,
        stages,
        orchestrator_edits,
        plan_sign_off,
        home_repos,
        landing,
        move_fast,
        landing_guard,
        dispatch_gate,
        new_chats,
    })
}

/// The stored JSON: quiet defaults left out (module header).
pub fn stored_form(s: &WorkflowSettings) -> Value {
    let mut v = serde_json::to_value(s).expect("settings serialise");
    let o = v.as_object_mut().expect("object");
    if !s.move_fast {
        o.remove("moveFast");
    }
    if s.landing_guard {
        o.remove("landingGuard");
    }
    if s.dispatch_gate {
        o.remove("dispatchGate");
    }
    if s.new_chats == NewChats::Auto {
        o.remove("newChats");
    }
    if s.landing.method == MergeMethod::Merge {
        if let Some(l) = o.get_mut("landing").and_then(Value::as_object_mut) {
            l.remove("method");
        }
    }
    v
}

/// The current settings: the stored value if it still validates, else the defaults.
pub fn load(tx: &Transaction<'_>) -> rusqlite::Result<WorkflowSettings> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM workflow_settings WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| match validate(v) {
            Ok(s) => Some(s),
            Err(e) => {
                log::warn!("stored Orchestrator settings no longer validate ({e}); using defaults");
                None
            }
        })
        .unwrap_or_default())
}

pub fn store(tx: &Transaction<'_>, s: &WorkflowSettings, now: i64) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO workflow_settings(id, value, updated_at) VALUES (1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        rusqlite::params![stored_form(s).to_string(), now],
    )?;
    Ok(())
}

/// The land mode of a repository: its rule, else `landing.default`. Only the host's
/// own computation is ever used.
pub fn land_mode(s: &WorkflowSettings, repo: &str) -> (LandMode, MergeMethod) {
    let repo = repo.to_ascii_lowercase();
    match s.landing.repos.iter().find(|r| r.repo == repo) {
        Some(r) => (r.mode, r.method.unwrap_or(s.landing.method)),
        None => (s.landing.default, s.landing.method),
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_are_off_with_caps_of_three_and_auto_for_new_chats() {
        let d = WorkflowSettings::default();
        assert!(!d.enabled);
        assert_eq!(d.stages.review.max_rounds, 3);
        assert_eq!(d.stages.plan_review.max_rounds, 3);
        assert_eq!(d.stages.smoketest.use_, StageUse::Auto);
        assert_eq!(d.new_chats, NewChats::Auto);
        assert!(!d.move_fast);
        assert_eq!(validate(json!({})).unwrap(), d);
        // The quiet defaults are left out of the stored form.
        let stored = stored_form(&d);
        assert!(stored.get("moveFast").is_none());
        assert!(stored.get("newChats").is_none());
        assert!(stored["landing"].get("method").is_none());
        assert_eq!(validate(stored).unwrap(), d);
    }

    #[test]
    fn a_round_trip_keeps_every_field() {
        let mut s = WorkflowSettings::default();
        s.enabled = true;
        s.move_fast = true;
        s.new_chats = NewChats::Never;
        s.landing.method = MergeMethod::Squash;
        s.landing.repos.push(RepoRule {
            repo: "github.com/o/r".into(),
            mode: LandMode::Merge,
            method: Some(MergeMethod::Rebase),
        });
        let stored = stored_form(&s);
        assert_eq!(stored["moveFast"], true);
        assert_eq!(stored["newChats"], "never");
        assert_eq!(validate(stored).unwrap(), s);
    }

    #[test]
    fn a_stored_always_is_written_out_and_read_back() {
        let mut s = WorkflowSettings::default();
        s.new_chats = NewChats::Always;
        let stored = stored_form(&s);
        assert_eq!(
            stored["newChats"], "always",
            "Always is no longer the quiet default"
        );
        assert_eq!(validate(stored).unwrap().new_chats, NewChats::Always);
        assert_eq!(
            validate(json!({"newChats": "always"})).unwrap().new_chats,
            NewChats::Always
        );
        // Through the table, as Settings > Orchestrator saves it.
        let (_d, db) = crate::mode::tests::db(&[]);
        db.write(|tx| store(tx, &s, 1)).unwrap();
        assert_eq!(db.snapshot(load).unwrap().new_chats, NewChats::Always);
        // Left out, it is the default.
        let auto = WorkflowSettings::default();
        assert!(stored_form(&auto).get("newChats").is_none());
        db.write(|tx| store(tx, &auto, 2)).unwrap();
        assert_eq!(db.snapshot(load).unwrap().new_chats, NewChats::Auto);
    }

    #[test]
    fn unknown_keys_are_refused_at_every_level_with_the_path() {
        for (v, path) in [
            (json!({"bogus": 1}), "bogus"),
            (json!({"stages": {"x": {}}}), "stages.x"),
            (
                json!({"stages": {"review": {"agent": "reviewer", "colour": 1}}}),
                "stages.review.colour",
            ),
            (
                json!({"landing": {"repos": [{"repo": "github.com/o/r", "mode": "merge", "fromBox": true}]}}),
                "landing.repos[0].fromBox",
            ),
            (
                json!({"landing": {"fromBoxes": false}}),
                "landing.fromBoxes",
            ),
        ] {
            let e = validate(v).unwrap_err();
            assert!(e.starts_with(path), "{e} should name {path}");
            assert!(e.contains("unknown"), "{e}");
        }
    }

    #[test]
    fn bad_values_are_refused() {
        let cases = [
            json!({"stages": {"review": {"use": "auto"}}}),
            json!({"stages": {"review": {"maxRounds": 0}}}),
            json!({"stages": {"review": {"maxRounds": 21}}}),
            json!({"stages": {"planReview": {"maxRounds": 2.5}}}),
            json!({"stages": {"build": {"use": "off"}}}),
            json!({"stages": {"plan": {"maxRounds": 3}}}),
            json!({"stages": {"review": {"agent": "Re viewer"}}}),
            json!({"stages": {"review": {"agent": "rev--x"}}}),
            json!({"stages": {"review": {"agent": "planner"}}}),
            json!({"newChats": "sometimes"}),
            json!({"enabled": "yes"}),
            json!({"landing": {"repos": [{"repo": "github.com/o/r", "mode": "draft", "method": "squash"}]}}),
            json!({"landing": {"repos": [{"repo": "o/r", "mode": "merge"}]}}),
            json!({"landing": {"repos": [{"repo": "github.com/o/r", "mode": "merge"}, {"repo": "GitHub.com/O/R", "mode": "draft"}]}}),
            json!({"landing": {"protectedBranches": ["ma in"]}}),
            json!({"landing": {"protectedBranches": ["../x"]}}),
            json!({"landing": {"draftLabels": ["a","b","c","d","e","f","g","h","i"]}}),
            json!({"homeRepos": ["not a repo"]}),
            json!([]),
        ];
        for v in cases {
            assert!(validate(v.clone()).is_err(), "{v} should be refused");
        }
        assert_eq!(
            validate(json!({"newChats": 1})).unwrap_err(),
            "newChats must be always, never or auto"
        );
        let dup = validate(json!({"stages": {"review": {"agent": "planner"}}})).unwrap_err();
        assert!(dup.contains("plan stage"), "{dup}");
    }

    #[test]
    fn lists_are_deduplicated_before_their_caps_and_repos_lowercased() {
        let many: Vec<String> = (0..40).map(|_| "coder".to_owned()).collect();
        let s = validate(json!({
            "stages": {"build": {"startAgents": many}},
            "homeRepos": ["GitHub.com/Me/Notes"],
            "landing": {"draftLabels": ["x","x","x","x","x","x","x","x","x"], "flipLabel": ""}
        }))
        .unwrap();
        assert_eq!(s.stages.build.start_agents, ["coder"]);
        assert_eq!(s.home_repos, ["github.com/me/notes"]);
        assert_eq!(s.landing.draft_labels, ["x"]);
        assert_eq!(s.landing.flip_label, None);
    }

    #[test]
    fn an_invalid_stored_value_falls_back_to_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let mut reg = helm_core::db::MigrationRegistry::new();
        reg.initializer("ws", init);
        let db = helm_core::db::Database::open(&dir.path().join("t.sqlite3"), &reg).unwrap();
        db.write::<_, rusqlite::Error>(|tx| {
            let mut s = WorkflowSettings::default();
            s.enabled = true;
            store(tx, &s, 1)?;
            assert!(load(tx)?.enabled);
            tx.execute(
                "UPDATE workflow_settings SET value = '{\"enabled\":true,\"stages\":{\"review\":{\"use\":\"auto\"}}}'",
                [],
            )?;
            assert_eq!(load(tx)?, WorkflowSettings::default());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn land_mode_follows_the_repo_rule_then_the_default() {
        let mut s = WorkflowSettings::default();
        s.landing.method = MergeMethod::Squash;
        s.landing.repos.push(RepoRule {
            repo: "github.com/o/r".into(),
            mode: LandMode::Merge,
            method: None,
        });
        assert_eq!(
            land_mode(&s, "github.com/O/R"),
            (LandMode::Merge, MergeMethod::Squash)
        );
        assert_eq!(
            land_mode(&s, "github.com/o/x"),
            (LandMode::Draft, MergeMethod::Squash)
        );
    }

    #[test]
    fn names_follow_their_patterns() {
        assert!(is_agent_name("plan-reviewer"));
        assert!(!is_agent_name("-x"));
        assert!(!is_agent_name(&"a".repeat(65)));
        assert!(is_branch_name("feature/x-1"));
        for bad in [
            "", "-x", "a..b", "a.lock", "a b", "a~1", ".x", "x/", "a/.b", "@",
        ] {
            assert!(!is_branch_name(bad), "{bad}");
        }
        assert_eq!(
            normalize_repo(" GitHub.com/A/B "),
            Some("github.com/a/b".into())
        );
        assert_eq!(normalize_repo("github.com/a"), None);
        assert_eq!(normalize_repo("github.com/../b"), None);
    }
}
