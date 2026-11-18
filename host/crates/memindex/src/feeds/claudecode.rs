//! Claude Code's own session records: one doc per main session file, filed under this PC's home
//! person and shown to the household unless the source's scope says otherwise. A second source can
//! claim some project folders (its `include` patterns, matched on the folder's name) for another
//! profile on this PC, so that person's sessions are filed under them; the first source leaves
//! those folders to it. A session Helm ran itself is already indexed as a Helm chat, so it is left
//! out: its id is in the chat's binding file or in a usage line of the chat's session log.

use super::{file_stamp, glob_match, Audience, Built, Entry, Env, Feed, Listing};
use crate::model::{Kind, Source};
use crate::sources::claude_code::build_titled_session_doc;
use crate::store;
use helm_chats::session_log::ClaudeBinding;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A record bigger than this is not read.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

pub struct ClaudeCodeFeed {
    src: Source,
    env: Arc<Env>,
    paths: Mutex<HashMap<String, PathBuf>>,
    /// The desktop app's titles, read at each listing.
    titles: Mutex<HashMap<String, String>>,
}

impl ClaudeCodeFeed {
    pub fn new(src: Source, env: Arc<Env>) -> Self {
        ClaudeCodeFeed {
            src,
            env,
            paths: Mutex::new(HashMap::new()),
            titles: Mutex::new(HashMap::new()),
        }
    }

    /// Whether this source leaves out a project folder (matched on its name): an exclude pattern
    /// names it; the source claims folders by `include` and this is not one of them; or the
    /// source claims none and another Claude Code source has claimed this one.
    fn leaves_out(&self, project: &str, claimed: &[String]) -> bool {
        if self.src.exclude.iter().any(|g| glob_match(g, project)) {
            return true;
        }
        if self.src.include.is_empty() {
            return claimed.iter().any(|g| glob_match(g, project));
        }
        !self.src.include.iter().any(|g| glob_match(g, project))
    }

    /// The folder patterns the other Claude Code sources claim, whether or not they are switched
    /// on: a person's sessions never fall back to the first source when theirs is paused.
    fn claimed_elsewhere(&self) -> Vec<String> {
        let all = self.env.db.snapshot(store::sources).unwrap_or_default();
        all.into_iter()
            .filter(|s| s.kind == Kind::ClaudeCode && s.id != self.src.id)
            .flat_map(|s| s.include)
            .collect()
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
        let claimed = self.claimed_elsewhere();
        let titles = self
            .env
            .desktop_sessions
            .as_deref()
            .map(super::desktop::titles)
            .unwrap_or_default();
        let mut found: Vec<(i64, Entry, PathBuf)> = Vec::new();
        let mut complete = true;
        let Ok(projects) = std::fs::read_dir(&root) else {
            return Ok(Listing::default());
        };
        for project in projects.flatten() {
            if !project.file_type().is_ok_and(|t| t.is_dir())
                || self.leaves_out(&project.file_name().to_string_lossy(), &claimed)
            {
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
                        key: id.clone(),
                        stamp: format!(
                            "{}|{}",
                            file_stamp(&meta),
                            titles.get(&id).map_or("", String::as_str)
                        ),
                    },
                    path,
                ));
            }
        }
        // Newest first, so a first pass finds recent work soonest.
        found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.key.cmp(&b.1.key)));
        *self.titles.lock().unwrap_or_else(|p| p.into_inner()) = titles;
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
        let title = self
            .titles
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(key)
            .cloned();
        let owner = if self.src.person_id.is_empty() {
            None
        } else {
            match self.env.profile(&self.src.person_id) {
                Some(p) => match self.env.owner_of(&p) {
                    Ok(o) => o,
                    Err(why) => return Ok(Built::Skip(why)),
                },
                None => {
                    return Ok(Built::Skip(
                        "the person for these sessions is not set up here".into(),
                    ))
                }
            }
        };
        let Some(doc) =
            build_titled_session_doc(key, &String::from_utf8_lossy(&text), title.as_deref())
        else {
            return Ok(Built::Skip("nothing was asked".into()));
        };
        Ok(super::finish(
            doc,
            &Audience::from_scope(owner, self.src.scope),
            "conversation",
            &format!("claude-code:{key}"),
        ))
    }
}
