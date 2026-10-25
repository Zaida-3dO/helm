//! Finding a newer setup file in the update folder.
//!
//! The installer build writes `Helm_<version>_x64-setup.exe`. A candidate must carry exactly that
//! name, be a plain file (not a link) that starts like a Windows program, and have a higher
//! version than the running app. Nothing here reads the network, and nothing runs the file.

use std::cmp::Ordering;
use std::io::Read;
use std::path::{Path, PathBuf};

const PREFIX: &str = "Helm_";
const SUFFIX: &str = "_x64-setup.exe";

/// A version as the installer names it: `major.minor.patch` with an optional `-pre.release` part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    core: [u64; 3],
    pre: Vec<String>,
}

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let nums: Vec<u64> = core
            .split('.')
            .map(|n| {
                if !n.is_empty() && n.len() <= 9 && n.bytes().all(|b| b.is_ascii_digit()) {
                    n.parse().ok()
                } else {
                    None
                }
            })
            .collect::<Option<_>>()?;
        let core: [u64; 3] = nums.try_into().ok()?;
        let pre = match pre {
            None => Vec::new(),
            Some(p) => {
                let ids: Vec<String> = p.split('.').map(str::to_owned).collect();
                let ok = ids
                    .iter()
                    .all(|i| !i.is_empty() && i.bytes().all(|b| b.is_ascii_alphanumeric()));
                if !ok {
                    return None;
                }
                ids
            }
        };
        Some(Version { core, pre })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core.cmp(&other.core).then_with(|| {
            // A release outranks any pre-release of the same numbers.
            match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => cmp_pre(&self.pre, &other.pre),
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn cmp_pre(a: &[String], b: &[String]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        let o = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => x.cmp(y),
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    a.len().cmp(&b.len())
}

/// The version inside a setup file name, or `None` when the name is not exactly the installer's.
pub fn version_of_setup_name(name: &str) -> Option<Version> {
    Version::parse(name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?)
}

/// A setup file that is newer than the running app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// The version text as it appears in the file name.
    pub version: String,
    pub file_name: String,
}

/// The newest file name in `names` whose version is higher than `current`.
pub fn pick_newest<'a>(
    names: impl IntoIterator<Item = &'a str>,
    current: &Version,
) -> Option<Candidate> {
    names
        .into_iter()
        .filter_map(|n| version_of_setup_name(n).map(|v| (v, n)))
        .filter(|(v, _)| v > current)
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, n)| Candidate {
            version: n[PREFIX.len()..n.len() - SUFFIX.len()].to_owned(),
            file_name: n.to_owned(),
        })
}

fn is_link(meta: &std::fs::Metadata) -> bool {
    helm_platform::fs::is_link_like(meta)
}

/// Check that `dir` is a real folder (not a link).
fn check_folder(dir: &Path) -> Result<(), String> {
    if !dir.is_absolute() {
        return Err("The update folder must be an absolute path.".into());
    }
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|_| "The update folder does not exist.".to_owned())?;
    if !meta.is_dir() || is_link(&meta) {
        return Err("The update folder is not a plain folder.".into());
    }
    Ok(())
}

/// Look in `dir` for the newest setup file that passes the file checks.
pub fn scan(dir: &Path, current: &Version) -> Result<Option<Candidate>, String> {
    check_folder(dir)?;
    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("Could not read the folder: {e}"))? {
        let Ok(entry) = entry else { continue };
        if let Some(name) = entry.file_name().to_str() {
            names.push(name.to_owned());
        }
    }
    // Try the candidates from the newest down, so one bad file does not hide an older good one.
    let mut remaining: Vec<&str> = names.iter().map(String::as_str).collect();
    while let Some(c) = pick_newest(remaining.iter().copied(), current) {
        if validate(dir, &c.file_name, current).is_ok() {
            return Ok(Some(c));
        }
        remaining.retain(|n| *n != c.file_name);
    }
    Ok(None)
}

/// Re-check a candidate right before it is used: the name, that it is newer, that it sits directly
/// in the folder, and that it is a plain file that starts like a Windows program. Returns its path.
pub fn validate(dir: &Path, file_name: &str, current: &Version) -> Result<PathBuf, String> {
    check_folder(dir)?;
    let plain_name = Path::new(file_name)
        .file_name()
        .is_some_and(|n| n == std::ffi::OsStr::new(file_name));
    let newer = version_of_setup_name(file_name).is_some_and(|v| &v > current);
    if !plain_name || !newer {
        return Err("That is not a newer Helm setup file.".into());
    }
    let path = dir.join(file_name);
    let meta = std::fs::symlink_metadata(&path)
        .map_err(|_| "The setup file is no longer there.".to_owned())?;
    if !meta.is_file() || is_link(&meta) {
        return Err("The setup file is not a plain file.".into());
    }
    let mut head = [0u8; 2];
    std::fs::File::open(&path)
        .and_then(|mut f| f.read_exact(&mut head))
        .map_err(|_| "The setup file could not be read.".to_owned())?;
    if &head != b"MZ" {
        return Err("The setup file is not a Windows program.".into());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn names_versions_and_the_newest_pick() {
        // (file names in the folder, running version, expected pick)
        let cases: &[(&[&str], &str, Option<&str>)] = &[
            (&[], "0.3.0", None),
            (&["Helm_0.3.0_x64-setup.exe"], "0.3.0", None), // same version: not an update
            (&["Helm_0.2.9_x64-setup.exe"], "0.3.0", None),
            (&["Helm_0.3.1_x64-setup.exe"], "0.3.0", Some("0.3.1")),
            (
                &[
                    "Helm_0.3.1_x64-setup.exe",
                    "Helm_0.10.0_x64-setup.exe",
                    "Helm_0.9.0_x64-setup.exe",
                ],
                "0.3.0",
                Some("0.10.0"), // numeric, not text, order
            ),
            (&["Helm_1.0.0-beta.2_x64-setup.exe"], "1.0.0", None), // a pre-release is older
            (&["Helm_1.0.0_x64-setup.exe"], "1.0.0-beta.2", Some("1.0.0")),
            (
                &[
                    "Helm_1.0.0-beta.10_x64-setup.exe",
                    "Helm_1.0.0-beta.9_x64-setup.exe",
                ],
                "0.9.0",
                Some("1.0.0-beta.10"),
            ),
            // Anything that is not exactly the installer's name is ignored.
            (
                &[
                    "Helm_0.4.0_x64-setup.exe.part",
                    "helm_0.4.0_x64-setup.exe",
                    "Helm_0.4_x64-setup.exe",
                ],
                "0.3.0",
                None,
            ),
            (
                &[
                    "Helm_0.4.0_x64_en-US.msi",
                    "Helm_x_x64-setup.exe",
                    "Helm__x64-setup.exe",
                    "notes.txt",
                ],
                "0.3.0",
                None,
            ),
            (
                &[
                    "Helm_0.4.0+build_x64-setup.exe",
                    "Helm_0.4.0-_x64-setup.exe",
                ],
                "0.3.0",
                None,
            ),
        ];
        for (names, current, want) in cases {
            let got = pick_newest(names.iter().copied(), &v(current)).map(|c| c.version);
            assert_eq!(got.as_deref(), *want, "{names:?} against {current}");
        }
    }

    #[test]
    fn the_folder_scan_and_the_install_time_recheck() {
        let dir = tempfile::tempdir().unwrap();
        let cur = v("0.3.0");
        let put = |name: &str, bytes: &[u8]| std::fs::write(dir.path().join(name), bytes).unwrap();
        // A newer-looking file that is not a program, a folder with a setup name, and a real one.
        put("Helm_0.9.0_x64-setup.exe", b"not a program");
        std::fs::create_dir(dir.path().join("Helm_0.8.0_x64-setup.exe")).unwrap();
        put("Helm_0.4.0_x64-setup.exe", b"MZ pretend");
        let found = scan(dir.path(), &cur).unwrap().unwrap();
        assert_eq!(found.file_name, "Helm_0.4.0_x64-setup.exe");
        assert!(validate(dir.path(), &found.file_name, &cur).is_ok());
        // The install-time check refuses what the scan would not have offered.
        for bad in [
            "Helm_0.9.0_x64-setup.exe",     // not a program
            "Helm_0.8.0_x64-setup.exe",     // a folder
            "Helm_0.2.0_x64-setup.exe",     // missing and older
            "..\\Helm_0.4.0_x64-setup.exe", // outside the folder
            "sub/Helm_0.4.0_x64-setup.exe",
            "evil.exe",
        ] {
            assert!(validate(dir.path(), bad, &cur).is_err(), "{bad}");
        }
        // The running app caught up after the file was found: no longer an update.
        assert!(validate(dir.path(), &found.file_name, &v("0.4.0")).is_err());
        // A missing or relative folder is an error, not "nothing found".
        assert!(scan(&dir.path().join("gone"), &cur).is_err());
        assert!(scan(Path::new("relative"), &cur).is_err());
    }
}
