//! Parsed chat logs: one doc per weekly file. A subfolder named `<name>-telegram` holds one
//! person's own chat and is filed under that person (the hub may still refuse to let this PC act
//! for them, and then the doc is refused with its reason); every other log is this PC's home
//! person's. The source's scope says who finds them: the household by default.

use super::{file_stamp, rel_path, Audience, Built, Entry, Env, Feed, Listing};
use crate::model::Source;
use crate::sources::chat_logs::build_log_doc;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

pub struct ChatLogsFeed {
    src: Source,
    env: Arc<Env>,
}

impl ChatLogsFeed {
    pub fn new(src: Source, env: Arc<Env>) -> Self {
        ChatLogsFeed { src, env }
    }

    fn walk(dir: &Path, root: &Path, out: &mut Vec<Entry>, complete: &mut bool) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            *complete = false;
            return;
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else {
                *complete = false;
                continue;
            };
            if ft.is_symlink() || e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if ft.is_dir() {
                Self::walk(&e.path(), root, out, complete);
            } else if e.path().extension().and_then(|x| x.to_str()) == Some("md") {
                match (e.metadata(), rel_path(root, &e.path())) {
                    (Ok(m), Some(key)) if m.len() <= MAX_FILE_BYTES => out.push(Entry {
                        key,
                        stamp: file_stamp(&m),
                    }),
                    (Err(_), _) => *complete = false,
                    _ => {}
                }
            }
        }
    }
}

impl Feed for ChatLogsFeed {
    fn list(&self) -> Result<Listing, String> {
        let root = PathBuf::from(&self.src.path);
        if !root.is_dir() {
            return Ok(Listing {
                entries: Vec::new(),
                complete: false,
                note: Some("The folder cannot be found.".into()),
            });
        }
        let (mut entries, mut complete) = (Vec::new(), true);
        Self::walk(&root, &root, &mut entries, &mut complete);
        entries.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(Listing {
            entries,
            complete,
            note: (!complete).then(|| "Part of the folder could not be read.".into()),
        })
    }

    fn build(&self, key: &str) -> Result<Built, String> {
        let owner = match key
            .split_once('/')
            .and_then(|(dir, _)| dir.strip_suffix("-telegram"))
        {
            Some(name) => {
                let who = self
                    .env
                    .db
                    .snapshot(|tx| helm_profiles::store::list(tx))
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .find(|p| p.name.eq_ignore_ascii_case(name));
                match who.map(|p| self.env.owner_of(&p)) {
                    Some(Ok(o)) => o,
                    Some(Err(why)) => return Ok(Built::Skip(why)),
                    None => return Ok(Built::Skip(format!("{name} has no profile here"))),
                }
            }
            None => None,
        };
        let path = Path::new(&self.src.path).join(key);
        let text = std::fs::read(&path).map_err(|e| e.to_string())?;
        let Some(doc) = build_log_doc(key, &String::from_utf8_lossy(&text)) else {
            return Ok(Built::Skip("no messages".into()));
        };
        Ok(super::finish(
            doc,
            &Audience::from_scope(owner, self.src.scope),
            "conversation",
            key,
        ))
    }
}
