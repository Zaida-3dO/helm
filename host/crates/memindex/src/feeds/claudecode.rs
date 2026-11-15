//! Claude Code's own session records: one doc per main session file, private to this PC's home
//! person. A session Helm ran itself is already indexed as a Helm chat, so it is left out: its id
//! is in the chat's binding file or in a usage line of the chat's session log.

use super::{file_stamp, Audience, Built, Entry, Env, Feed, Listing};
use crate::sources::claude_code::build_session_doc;
use helm_chats::session_log::ClaudeBinding;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A record bigger than this is not read.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

pub struct ClaudeCodeFeed {
    env: Arc<Env>,
    paths: Mutex<HashMap<String, PathBuf>>,
}

impl ClaudeCodeFeed {
    pub fn new(env: Arc<Env>) -> Self {
        ClaudeCodeFeed {
            env,
            paths: Mutex::new(HashMap::new()),
        }
    }
}

/// The native session ids named in one session log's usage lines.
fn log_sessions(path: &Path) -> Vec<String> {
    let Ok(f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        if !line.contains("\"nativeSession\"") {
            continue;
        }
        if let Some(id) = serde_json::from_str::<Value>(&line)
            .ok()
            .and_then(|v| v.get("nativeSession")?.as_str().map(str::to_owned))
        {
            out.push(id);
        }
    }
    out
}

/// Every Claude Code session id a Helm chat has used.
pub fn helm_sessions(env: &Env) -> HashSet<String> {
    let mut ids = HashSet::new();
    let Ok(rd) = std::fs::read_dir(&env.sessions) else {
        return ids;
    };
    let mut cache = env.native_cache.lock().unwrap_or_else(|p| p.into_inner());
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(chat) = name.strip_suffix(".claude.json") {
            if let Some(id) =
                ClaudeBinding::load(&env.sessions, chat).and_then(|b| b.native_session_id)
            {
                ids.insert(id);
            }
        } else if name.ends_with(".jsonl") {
            let Ok(meta) = e.metadata() else { continue };
            let stamp = file_stamp(&meta);
            let path = e.path();
            match cache.get(&path) {
                Some((s, found)) if *s == stamp => ids.extend(found.iter().cloned()),
                _ => {
                    let found = log_sessions(&path);
                    ids.extend(found.iter().cloned());
                    cache.insert(path, (stamp, found));
                }
            }
        }
    }
    ids
}

impl Feed for ClaudeCodeFeed {
    fn list(&self) -> Result<Listing, String> {
        let Some(root) = self.env.claude_projects.clone().filter(|r| r.is_dir()) else {
            return Ok(Listing {
                entries: Vec::new(),
                complete: false,
                note: Some("Claude Code's records were not found on this PC.".into()),
            });
        };
        let own = helm_sessions(&self.env);
        let mut found: Vec<(i64, Entry, PathBuf)> = Vec::new();
        let mut complete = true;
        let Ok(projects) = std::fs::read_dir(&root) else {
            return Ok(Listing::default());
        };
        for project in projects.flatten() {
            if !project.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let Ok(files) = std::fs::read_dir(project.path()) else {
                complete = false;
                continue;
            };
            for f in files.flatten() {
                let path = f.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
                    continue;
                };
                let Ok(meta) = f.metadata() else {
                    complete = false;
                    continue;
                };
                if own.contains(&id) || meta.len() > MAX_FILE_BYTES || !meta.is_file() {
                    continue;
                }
                let at = super::mtime_ms(&meta);
                found.push((
                    at,
                    Entry {
                        key: id,
                        stamp: file_stamp(&meta),
                    },
                    path,
                ));
            }
        }
        // Newest first, so a first pass finds recent work soonest.
        found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.key.cmp(&b.1.key)));
        let mut paths = self.paths.lock().unwrap_or_else(|p| p.into_inner());
        paths.clear();
        let entries = found
            .into_iter()
            .map(|(_, e, p)| {
                paths.insert(e.key.clone(), p);
                e
            })
            .collect();
        Ok(Listing {
            entries,
            complete,
            note: None,
        })
    }

    fn build(&self, key: &str) -> Result<Built, String> {
        let path = self
            .paths
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(key)
            .cloned()
            .ok_or("The session record is gone.")?;
        let text = std::fs::read(&path).map_err(|e| e.to_string())?;
        let Some(doc) = build_session_doc(key, &String::from_utf8_lossy(&text)) else {
            return Ok(Built::Skip("nothing was asked".into()));
        };
        Ok(super::finish(
            doc,
            &Audience::private(None),
            "conversation",
            &format!("claude-code:{key}"),
        ))
    }
}
