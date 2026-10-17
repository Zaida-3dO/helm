//! Probe files for what a DOM check cannot see (used in place of dialogs).
//! In debug builds only, `HELM_TEST_PROBE=<file>` makes the shell append one JSON line
//! per observable effect: `badge {count}`, `keep_awake {on}`, `tray_menu {groups, mics}`,
//! `toast {chatId, mode}`, `hotkey {chord, registered}`. Release builds ignore the variable.

use serde_json::{Map, Value};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

pub const PROBE_ENV: &str = "HELM_TEST_PROBE";

/// The probe path, honoured only in debug builds.
pub fn probe_path(debug: bool, value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    if !debug {
        return None;
    }
    value.filter(|v| !v.is_empty()).map(PathBuf::from)
}

#[derive(Default)]
pub struct Probe {
    path: Option<PathBuf>,
    lock: Mutex<()>,
}

impl Probe {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn at(path: Option<PathBuf>) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    /// From the environment; `debug` is `cfg!(debug_assertions)` at the call site.
    pub fn from_env(debug: bool) -> Self {
        Self::at(probe_path(debug, std::env::var_os(PROBE_ENV)))
    }

    pub fn enabled(&self) -> bool {
        self.path.is_some()
    }

    /// Append `{"probe": kind, "t": ms, ...fields}` as one line. Failures are logged once per call
    /// and never reach the caller: a probe never changes behaviour.
    pub fn record(&self, kind: &str, fields: Value) {
        let Some(path) = &self.path else { return };
        let mut m = match fields {
            Value::Object(m) => m,
            other => {
                let mut m = Map::new();
                m.insert("value".into(), other);
                m
            }
        };
        m.insert("probe".into(), Value::String(kind.to_owned()));
        m.insert("t".into(), Value::from(helm_core::now_ms()));
        let _g = self.lock.lock().unwrap();
        let line = format!("{}\n", Value::Object(m));
        let r = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
        if let Err(e) = r {
            log::warn!("probe write failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn release_builds_ignore_the_variable() {
        assert_eq!(probe_path(false, Some("x.jsonl".into())), None);
        assert_eq!(
            probe_path(true, Some("x.jsonl".into())),
            Some(PathBuf::from("x.jsonl"))
        );
        assert_eq!(probe_path(true, Some("".into())), None);
        assert_eq!(probe_path(true, None), None);
    }

    #[test]
    fn records_one_json_line_per_call() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.jsonl");
        let p = Probe::at(Some(path.clone()));
        p.record("badge", json!({"count": 2}));
        p.record("keep_awake", json!({"on": true}));
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["probe"], "badge");
        assert_eq!(lines[0]["count"], 2);
        assert_eq!(lines[1]["on"], true);
        Probe::disabled().record("badge", json!({"count": 1}));
    }
}
