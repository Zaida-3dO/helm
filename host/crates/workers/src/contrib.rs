//! Launch contributors: other crates add to a worker's launch without the
//! workers crate knowing them. Users so far: the panel's git wrapper (a `PATH` prefix) and its
//! vault instruction fragment (a system-prompt fragment).
//!
//! The app collects them through `registrations::launch_contributors` (one line per crate) into a
//! [`LaunchContributors`] list and hands it to the spawner. With nothing contributed, a launch is
//! byte-identical to one without this seam: no `PATH` change, no new variables, no
//! `appendSystemPrompt` in `launch.json`.

use crate::env::{EnvMap, WORKER_CHAT_ID, WORKER_DATA_ROOT};
use std::path::PathBuf;
use std::sync::Arc;

/// What a contributor sees of the launch.
#[derive(Clone, Debug)]
pub struct LaunchCtx<'a> {
    pub chat_id: &'a str,
    /// The folder the worker runs in.
    pub cwd: &'a str,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchExtras {
    /// Folders put in front of the worker's `PATH`, in order.
    pub path_prepend: Vec<PathBuf>,
    /// Extra variables. `PATH` and Helm's own identity variables are refused (logged, skipped).
    pub env: Vec<(String, String)>,
    /// Fragments appended to Claude Code's system prompt, joined by blank lines.
    pub system_prompt: Vec<String>,
}

impl LaunchExtras {
    pub fn is_empty(&self) -> bool {
        self.path_prepend.is_empty() && self.env.is_empty() && self.system_prompt.is_empty()
    }
}

pub trait LaunchContributor: Send + Sync {
    fn contribute(&self, ctx: &LaunchCtx<'_>) -> LaunchExtras;
}

/// The registered contributors, in registration order.
#[derive(Clone, Default)]
pub struct LaunchContributors {
    list: Vec<Arc<dyn LaunchContributor>>,
}

impl LaunchContributors {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, c: Arc<dyn LaunchContributor>) -> &mut Self {
        self.list.push(c);
        self
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Every contributor's extras, merged in order.
    pub fn collect(&self, ctx: &LaunchCtx<'_>) -> LaunchExtras {
        let mut out = LaunchExtras::default();
        for c in &self.list {
            let e = c.contribute(ctx);
            out.path_prepend.extend(e.path_prepend);
            out.env.extend(e.env);
            out.system_prompt
                .extend(e.system_prompt.into_iter().filter(|s| !s.trim().is_empty()));
        }
        out
    }
}

const PATH_SEP: &str = if cfg!(windows) { ";" } else { ":" };

/// Apply `PATH` prefixes and variables to the worker environment.
pub fn apply_env(env: &mut EnvMap, extras: &LaunchExtras) {
    if !extras.path_prepend.is_empty() {
        let mut parts: Vec<String> = extras
            .path_prepend
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        if let Some(old) = env.get("PATH").filter(|p| !p.is_empty()) {
            parts.push(old.to_owned());
        }
        env.set("PATH", parts.join(PATH_SEP));
    }
    for (k, v) in &extras.env {
        let reserved = [WORKER_CHAT_ID, WORKER_DATA_ROOT, "PATH"];
        if reserved.iter().any(|r| r.eq_ignore_ascii_case(k)) {
            log::warn!("a launch contributor tried to set {k}; ignored");
            continue;
        }
        env.set(k, v.clone());
    }
}

/// The system-prompt fragments as one `--append-system-prompt` value, or `None`.
pub fn system_prompt(extras: &LaunchExtras) -> Option<String> {
    (!extras.system_prompt.is_empty()).then(|| extras.system_prompt.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(LaunchExtras);
    impl LaunchContributor for Fixed {
        fn contribute(&self, _ctx: &LaunchCtx<'_>) -> LaunchExtras {
            self.0.clone()
        }
    }

    fn ctx() -> LaunchCtx<'static> {
        LaunchCtx {
            chat_id: "c",
            cwd: "C:/w",
        }
    }

    #[test]
    fn nothing_contributed_changes_nothing() {
        let list = LaunchContributors::new();
        let extras = list.collect(&ctx());
        assert!(extras.is_empty());
        let mut env = EnvMap::default();
        env.set("PATH", "C:/bin");
        apply_env(&mut env, &extras);
        assert_eq!(env.get("PATH"), Some("C:/bin"));
        assert_eq!(system_prompt(&extras), None);
    }

    #[test]
    fn contributions_merge_in_order_and_identity_variables_are_kept() {
        let mut list = LaunchContributors::new();
        list.add(Arc::new(Fixed(LaunchExtras {
            path_prepend: vec![PathBuf::from("C:/wrap")],
            env: vec![
                ("GIT_X".into(), "1".into()),
                (WORKER_CHAT_ID.into(), "evil".into()),
            ],
            system_prompt: vec!["one".into()],
        })))
        .add(Arc::new(Fixed(LaunchExtras {
            path_prepend: vec![PathBuf::from("C:/two")],
            env: vec![],
            system_prompt: vec!["  ".into(), "two".into()],
        })));
        let extras = list.collect(&ctx());
        let mut env = EnvMap::default();
        env.set("PATH", "C:/bin");
        env.set(WORKER_CHAT_ID, "c");
        apply_env(&mut env, &extras);
        assert_eq!(
            env.get("PATH"),
            Some(format!("C:/wrap{PATH_SEP}C:/two{PATH_SEP}C:/bin").as_str())
        );
        assert_eq!(env.get("GIT_X"), Some("1"));
        assert_eq!(env.get(WORKER_CHAT_ID), Some("c"));
        assert_eq!(system_prompt(&extras).as_deref(), Some("one\n\ntwo"));
    }
}
