//! A folder of markdown notes: one doc per file.

use super::{
    file_stamp, glob_match, mtime_ms, rel_path, Audience, Built, Entry, Env, Feed, Listing,
};
use crate::model::Source;
use crate::sources::markdown::chunk_markdown;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A note bigger than this is not read.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

pub struct FolderFeed {
    src: Source,
    env: Arc<Env>,
}

impl FolderFeed {
    pub fn new(src: Source, env: Arc<Env>) -> Self {
        FolderFeed { src, env }
    }

    fn root(&self) -> PathBuf {
        PathBuf::from(&self.src.path)
    }

    fn wanted(&self, rel: &str) -> bool {
        rel.to_ascii_lowercase().ends_with(".md")
            && (self.src.include.is_empty() || self.src.include.iter().any(|g| glob_match(g, rel)))
            && !self.src.exclude.iter().any(|g| glob_match(g, rel))
    }

    /// Walk without following links. Hidden folders and files and `node_modules` are skipped.
    fn walk(&self, dir: &Path, root: &Path, out: &mut Vec<Entry>, complete: &mut bool) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            *complete = false;
            return;
        };
        for e in rd.flatten() {
            let path = e.path();
            let Ok(ft) = e.file_type() else {
                *complete = false;
                continue;
            };
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(rel) = rel_path(root, &path) else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if name.starts_with('.')
                    || name == "node_modules"
                    || self.src.exclude.iter().any(|g| glob_match(g, &rel))
                {
                    continue;
                }
                self.walk(&path, root, out, complete);
            } else if ft.is_file() && !name.starts_with('.') && self.wanted(&rel) {
                match e.metadata() {
                    Ok(m) if m.len() <= MAX_FILE_BYTES => out.push(Entry {
                        key: rel,
                        stamp: file_stamp(&m),
                    }),
                    Ok(_) => {}
                    Err(_) => *complete = false,
                }
            }
        }
    }
}

impl Feed for FolderFeed {
    fn list(&self) -> Result<Listing, String> {
        let root = self.root();
        if !root.is_dir() {
            return Ok(Listing {
                entries: Vec::new(),
                complete: false,
                note: Some("The folder cannot be found.".into()),
            });
        }
        let (mut entries, mut complete) = (Vec::new(), true);
        self.walk(&root, &root, &mut entries, &mut complete);
        entries.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(Listing {
            entries,
            complete,
            note: (!complete).then(|| "Part of the folder could not be read.".into()),
        })
    }

    fn build(&self, key: &str) -> Result<Built, String> {
        let profile = self
            .env
            .profile(&self.src.person_id)
            .ok_or("The person for this folder is not set up here.")?;
        let owner = match self.env.owner_of(&profile) {
            Ok(o) => o,
            Err(why) => return Ok(Built::Skip(why)),
        };
        let path = self.root().join(key);
        let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        let doc = chunk_markdown(key, &String::from_utf8_lossy(&bytes), mtime_ms(&meta));
        if doc.chunks.is_empty() {
            return Ok(Built::Skip("empty".into()));
        }
        let audience = Audience::from_scope(owner, self.src.scope);
        Ok(super::finish(doc, &audience, "note", key))
    }
}
