//! Staging the worker's source tree.
//!
//! The whole `worker/src` tree, kept as a tree (never flattened, see `worker/PROTOCOL-NOTES.md`), is
//! copied once per content hash into `<profile>/runtime/worker/<hash>/src/`, and every worker of
//! that build runs `node <staged>/src/main.ts <launch.json>`. A finished copy carries a `.complete`
//! marker; a half-written one (a crash mid-copy) is ignored and replaced.
//!
//! The worker is dependency-free (only `node:` built-ins), so `src` is all it needs. If it ever
//! gains npm dependencies, staging must copy them too.
//!
//! Source: debug builds read the repo's `worker/src` (or `HELM_WORKER_SRC`); release builds read
//! the copy Tauri bundles as a resource (`tauri.conf.json` `bundle.resources`).

use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};

const MARKER: &str = ".complete";

/// Every regular file under `root`, as sorted relative paths with `/` separators.
fn list_files(root: &Path) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            let path = entry.path();
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .map_err(io::Error::other)?
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(rel);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// A hash of the tree's paths and bytes (first 16 hex characters of SHA-256).
pub fn content_hash(root: &Path) -> io::Result<String> {
    let mut h = Sha256::new();
    for rel in list_files(root)? {
        h.update(rel.as_bytes());
        h.update([0]);
        h.update(std::fs::read(root.join(&rel))?);
        h.update([0]);
    }
    Ok(hex::encode(h.finalize())[..16].to_owned())
}

/// Stage `source` (a `worker/src` folder) under `runtime_root` and return the staged `main.ts`.
pub fn stage_worker(source: &Path, runtime_root: &Path) -> io::Result<PathBuf> {
    if !source.join("main.ts").is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no worker at {}", source.display()),
        ));
    }
    let hash = content_hash(source)?;
    let base = runtime_root.join("worker");
    let dest = base.join(&hash);
    let main = dest.join("src").join("main.ts");
    if dest.join(MARKER).is_file() && main.is_file() {
        return Ok(main);
    }
    helm_core::privfs::create_private_dir_all(&base)?;
    let temp = base.join(format!(".{hash}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let copy = (|| {
        for rel in list_files(source)? {
            let to = temp.join("src").join(&rel);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(source.join(&rel), &to)?;
        }
        std::fs::write(temp.join(MARKER), hash.as_bytes())
    })();
    if let Err(e) = copy {
        let _ = std::fs::remove_dir_all(&temp);
        return Err(e);
    }
    if dest.join(MARKER).is_file() {
        // Another launch finished staging the same hash meanwhile: use its copy, never delete it
        // (a worker may already be loading files from it).
        let _ = std::fs::remove_dir_all(&temp);
        return Ok(main);
    }
    if dest.exists() {
        // A half-staged copy without its marker: replace it.
        let _ = std::fs::remove_dir_all(&dest);
    }
    match std::fs::rename(&temp, &dest) {
        Ok(()) => {}
        Err(_) if dest.join(MARKER).is_file() => {
            // Another launch staged the same hash first.
            let _ = std::fs::remove_dir_all(&temp);
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp);
            return Err(e);
        }
    }
    Ok(main)
}

/// Where the worker source comes from in this build.
pub fn default_source(debug: bool, resource_dir: Option<&Path>) -> Option<PathBuf> {
    if debug {
        if let Some(p) = std::env::var_os("HELM_WORKER_SRC").filter(|p| !p.is_empty()) {
            return Some(PathBuf::from(p));
        }
        return Some(repo_worker_src());
    }
    resource_dir.map(|r| plain_path(r).join("worker").join("src"))
}

/// Tauri reports the app's resource folder as a Windows verbatim path (`\\?\C:\...`). Node cannot
/// run a script given that way (it fails with `EISDIR` on `C:`), so every helper started from the
/// resource folder needs the plain drive path. Anything else (a UNC path, a relative path, a path
/// on another platform) is returned unchanged.
pub fn plain_path(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
            return PathBuf::from(rest);
        }
    }
    p.to_path_buf()
}

/// The repository's `worker/src`, as seen from this crate at build time.
pub fn repo_worker_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../worker/src")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(dir: &Path, files: &[(&str, &str)]) {
        for (rel, text) in files {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
    }

    #[test]
    fn stages_the_tree_once_per_hash_and_keeps_subfolders() {
        let src = tempfile::tempdir().unwrap();
        let rt = tempfile::tempdir().unwrap();
        tree(
            src.path(),
            &[("main.ts", "main"), ("protocol/lines.ts", "lines")],
        );
        let main = stage_worker(src.path(), rt.path()).unwrap();
        assert!(main.ends_with("src/main.ts") || main.ends_with("src\\main.ts"));
        let staged = main.parent().unwrap();
        assert_eq!(
            std::fs::read_to_string(staged.join("protocol/lines.ts")).unwrap(),
            "lines"
        );
        // Same content: same folder, not re-copied.
        assert_eq!(stage_worker(src.path(), rt.path()).unwrap(), main);
        // Changed content: a new folder.
        std::fs::write(src.path().join("protocol/lines.ts"), "lines v2").unwrap();
        let main2 = stage_worker(src.path(), rt.path()).unwrap();
        assert_ne!(main2, main);
    }

    #[test]
    fn a_copy_without_its_marker_is_replaced() {
        let src = tempfile::tempdir().unwrap();
        let rt = tempfile::tempdir().unwrap();
        tree(src.path(), &[("main.ts", "main")]);
        let hash = content_hash(src.path()).unwrap();
        let half = rt.path().join("worker").join(&hash).join("src");
        std::fs::create_dir_all(&half).unwrap();
        std::fs::write(half.join("main.ts"), "TRUNCATED").unwrap();
        let main = stage_worker(src.path(), rt.path()).unwrap();
        assert_eq!(std::fs::read_to_string(main).unwrap(), "main");
    }

    #[test]
    fn a_verbatim_resource_folder_becomes_a_plain_drive_path() {
        assert_eq!(
            plain_path(Path::new(r"\\?\C:\data\x\AppData\Local\Programs\Helm")),
            Path::new(r"C:\data\x\AppData\Local\Programs\Helm")
        );
        // Not a drive path: left alone, as is an ordinary path.
        for same in [
            r"\\?\UNC\server\share\Helm",
            r"\\?\Volume{1}\Helm",
            r"C:\Helm",
            "rel/Helm",
            "/opt/helm",
        ] {
            assert_eq!(plain_path(Path::new(same)), Path::new(same));
        }
        // The release worker source is built from the plain path.
        let src = default_source(false, Some(Path::new(r"\\?\C:\Helm"))).unwrap();
        assert_eq!(src, Path::new(r"C:\Helm").join("worker").join("src"));
    }

    #[test]
    fn the_repo_worker_stages() {
        let rt = tempfile::tempdir().unwrap();
        let main = stage_worker(&repo_worker_src(), rt.path()).unwrap();
        assert!(main.is_file());
        assert!(main.parent().unwrap().join("protocol").is_dir());
    }
}
