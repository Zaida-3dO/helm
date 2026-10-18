//! Request modes: Fast, Standard and
//! unknown model-request counts over a range, from the native transcripts of the chats Helm
//! started (Claude Code only for now). The counting is `helpers/usage`; this module finds which
//! transcripts are Helm's (the chat bindings in `<profile>/sessions`) and checks the range.
//!
//! Op `request_modes {since, until}` (heavy): `since >= 0`, `until > since`, `until` at most 60 s
//! ahead of now; answers [`RequestModes`]. The reply is historical metadata only.

use helm_ipc::ApiError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use ts_rs::TS;

/// The job's host deadline.
pub const TIMEOUT: Duration = Duration::from_secs(120);
const MAX_BINDINGS: usize = 5_000;
const BINDING_MAX_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RequestModes {
    #[ts(type = "number")]
    pub fast: u32,
    #[ts(type = "number")]
    pub standard: u32,
    #[ts(type = "number")]
    pub unknown: u32,
    /// Transcript files read.
    #[ts(type = "number")]
    pub files: u32,
    /// Lines that were not JSON and records over the size cap: for the coverage note.
    #[ts(type = "number")]
    pub malformed: u32,
    #[ts(type = "number")]
    pub oversized: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModesArgs {
    pub since: i64,
    pub until: i64,
}

/// Runs one helper job.
pub trait JobRunner: Send + Sync {
    fn run(&self, job: &Value, timeout: Duration) -> Result<Value, String>;
}

impl JobRunner for crate::helper::ProcessReader {
    fn run(&self, job: &Value, timeout: Duration) -> Result<Value, String> {
        self.run_job(job, timeout)
    }
}

pub struct ModesService {
    runner: std::sync::Arc<dyn JobRunner>,
    sessions_dir: PathBuf,
    projects_dir: Option<PathBuf>,
}

/// The Claude Code config folder's `projects` directory: `CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn default_projects_dir() -> Option<PathBuf> {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".claude"))
        })?;
    Some(base.join("projects"))
}

/// The native sessions Helm started: `(sessionId, cwd)` from each `<chat>.claude.json`.
pub fn bound_sessions(sessions_dir: &Path) -> Vec<(String, String)> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Binding {
        native_session_id: Option<String>,
        #[serde(default)]
        cwd: String,
    }
    let Ok(rd) = std::fs::read_dir(sessions_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        if out.len() >= MAX_BINDINGS {
            break;
        }
        let name = e.file_name();
        if !name.to_string_lossy().ends_with(".claude.json") {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() || meta.len() > BINDING_MAX_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(e.path()) else {
            continue;
        };
        if let Ok(Binding {
            native_session_id: Some(id),
            cwd,
        }) = serde_json::from_slice::<Binding>(&bytes)
        {
            out.push((id, cwd));
        }
    }
    out
}

impl ModesService {
    pub fn new(
        runner: std::sync::Arc<dyn JobRunner>,
        sessions_dir: PathBuf,
        projects_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            runner,
            sessions_dir,
            projects_dir,
        }
    }

    pub fn request_modes(&self, a: &ModesArgs, now: i64) -> Result<RequestModes, ApiError> {
        if a.since < 0 || a.until <= a.since {
            return Err(ApiError::bad_args(
                "The range must start at 0 or later and end after it starts.",
            ));
        }
        if a.until > now + 60_000 {
            return Err(ApiError::bad_args("The range cannot end in the future."));
        }
        let Some(projects) = &self.projects_dir else {
            return Err(ApiError::new(
                "unavailable",
                "Helm could not find the Claude Code folder, so it cannot count requests.",
            ));
        };
        let sessions: Vec<Value> = bound_sessions(&self.sessions_dir)
            .into_iter()
            .map(|(id, cwd)| json!({ "sessionId": id, "cwd": cwd }))
            .collect();
        let job = json!({
            "kind": "modes",
            "projectsDir": projects.to_string_lossy(),
            "sessions": sessions,
            "since": a.since,
            "until": a.until,
        });
        let v = self
            .runner
            .run(&job, TIMEOUT)
            .map_err(|e| ApiError::new("unavailable", format!("Request modes: {e}")))?;
        if v.get("ok").and_then(Value::as_bool) != Some(true) {
            let why = v
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("no answer");
            return Err(ApiError::new(
                "unavailable",
                format!("Request modes: {why}"),
            ));
        }
        serde_json::from_value(v["modes"].clone())
            .map_err(|_| ApiError::internal("The request-modes helper's answer was malformed."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Fake(Mutex<Vec<Value>>, Value);
    impl JobRunner for Fake {
        fn run(&self, job: &Value, _: Duration) -> Result<Value, String> {
            self.0.lock().unwrap().push(job.clone());
            Ok(self.1.clone())
        }
    }

    fn svc(dir: &Path, answer: Value) -> (ModesService, Arc<Fake>) {
        let f = Arc::new(Fake(Mutex::new(vec![]), answer));
        (
            ModesService::new(f.clone(), dir.join("sessions"), Some(dir.join("projects"))),
            f,
        )
    }

    #[test]
    fn only_helm_bound_sessions_go_to_the_helper_and_the_counts_come_back() {
        let d = tempfile::tempdir().unwrap();
        let s = d.path().join("sessions");
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(
            s.join("a.claude.json"),
            r#"{"nativeSessionId":"abc-123","cwd":"C:/w/one","updatedAt":1}"#,
        )
        .unwrap();
        std::fs::write(
            s.join("b.claude.json"),
            r#"{"nativeSessionId":null,"cwd":"x"}"#,
        )
        .unwrap();
        std::fs::write(s.join("c.jsonl"), "{}").unwrap();
        std::fs::write(s.join("junk.claude.json"), "not json").unwrap();
        let (m, fake) = svc(
            d.path(),
            json!({"ok": true, "modes": {"fast": 2, "standard": 5, "unknown": 1, "files": 1, "malformed": 0, "oversized": 0}}),
        );
        let r = m
            .request_modes(
                &ModesArgs {
                    since: 10,
                    until: 20,
                },
                1_000,
            )
            .unwrap();
        assert_eq!((r.fast, r.standard, r.unknown), (2, 5, 1));
        let job = fake.0.lock().unwrap()[0].clone();
        assert_eq!(job["kind"], "modes");
        assert_eq!(
            job["sessions"],
            json!([{"sessionId": "abc-123", "cwd": "C:/w/one"}])
        );
        assert_eq!(
            (job["since"].clone(), job["until"].clone()),
            (json!(10), json!(20))
        );
    }

    #[test]
    fn a_bad_range_is_refused_before_any_helper_runs() {
        let d = tempfile::tempdir().unwrap();
        let (m, fake) = svc(d.path(), json!({"ok": true}));
        for (since, until) in [(-1, 5), (5, 5), (9, 3), (0, 1_000 + 60_001)] {
            let e = m
                .request_modes(&ModesArgs { since, until }, 1_000)
                .unwrap_err();
            assert_eq!(e.code, "bad_args", "{since}..{until}");
        }
        assert!(fake.0.lock().unwrap().is_empty());
    }

    #[test]
    fn a_helper_failure_is_unavailable_with_its_reason() {
        let d = tempfile::tempdir().unwrap();
        let (m, _) = svc(
            d.path(),
            json!({"ok": false, "reason": "error", "message": "bad modes job"}),
        );
        let e = m
            .request_modes(&ModesArgs { since: 0, until: 5 }, 100)
            .unwrap_err();
        assert_eq!(e.code, "unavailable");
        assert!(e.message.contains("bad modes job"));
    }
}
