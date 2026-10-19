//! The worker's environment.
//!
//! Windows has no login shell to capture, so the worker gets the **registry** environment: the
//! system variables, then the user's, then the session's volatile ones (`USERPROFILE`, `APPDATA`,
//! ...), each overriding the last, with `REG_EXPAND_SZ` values expanded against the merged set and
//! `PATH` joined as system then user. That is what a freshly opened terminal would see, so a tool
//! installed after Helm started is still found. The base is Helm's own environment, so variables
//! only a running process has (for example `HELM_*` test settings) carry over.
//!
//! Linux keeps the inherited environment for now (the login-shell capture is a follow-up).
//!
//! On top come the worker identity variables, [`WORKER_CHAT_ID`] and [`WORKER_DATA_ROOT`].
//! Credential stripping for the claude child happens in the worker, which relies on the
//! host never setting `apiKeyMode`.

use std::collections::BTreeMap;
use std::ffi::OsString;

pub const WORKER_CHAT_ID: &str = "HELM_CHAT_ID";
pub const WORKER_DATA_ROOT: &str = "HELM_DATA_ROOT";
/// Node's on-disk compile cache for Helm's own scripts (it about halves the
/// worker's start). The worker and helpers drop it before starting claude, so nothing the agent
/// runs writes into Helm's folder.
pub const NODE_COMPILE_CACHE: &str = "NODE_COMPILE_CACHE";

/// `<data root>/runtime/node-cache`, beside the staged worker.
pub fn compile_cache_dir(data_root: &str) -> String {
    std::path::Path::new(data_root)
        .join("runtime")
        .join("node-cache")
        .to_string_lossy()
        .into_owned()
}

/// Variables are compared case-insensitively on Windows, so keys are kept upper-cased for lookup
/// alongside their original spelling.
#[derive(Debug, Clone, Default)]
pub struct EnvMap {
    vars: BTreeMap<String, (String, String)>,
}

impl EnvMap {
    fn key(k: &str) -> String {
        if cfg!(windows) {
            k.to_ascii_uppercase()
        } else {
            k.to_owned()
        }
    }

    pub fn set(&mut self, k: &str, v: impl Into<String>) {
        self.vars.insert(Self::key(k), (k.to_owned(), v.into()));
    }

    pub fn get(&self, k: &str) -> Option<&str> {
        self.vars.get(&Self::key(k)).map(|(_, v)| v.as_str())
    }

    pub fn remove(&mut self, k: &str) {
        self.vars.remove(&Self::key(k));
    }

    pub fn from_process() -> Self {
        let mut m = Self::default();
        for (k, v) in std::env::vars_os() {
            if let (Ok(k), Ok(v)) = (k.into_string(), v.into_string()) {
                m.set(&k, v);
            }
        }
        m
    }

    pub fn into_pairs(self) -> Vec<(OsString, OsString)> {
        self.vars
            .into_values()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    /// Expand `%NAME%` references against this map; unknown names are left as written.
    pub fn expand(&self, s: &str) -> String {
        let mut out = String::new();
        let mut rest = s;
        while let Some(start) = rest.find('%') {
            out.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            match after.find('%') {
                Some(end) if end > 0 => {
                    let name = &after[..end];
                    match self.get(name) {
                        Some(v) => out.push_str(v),
                        None => {
                            out.push('%');
                            out.push_str(name);
                            out.push('%');
                        }
                    }
                    rest = &after[end + 1..];
                }
                _ => {
                    out.push('%');
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }
}

/// One registry value: name, data, and whether it was `REG_EXPAND_SZ`.
#[derive(Debug, Clone)]
pub struct RegValue {
    pub name: String,
    pub data: String,
    pub expand: bool,
}

/// Merge the registry layers over `base`. Pure, so it is testable on any platform.
pub fn merge_registry(
    mut base: EnvMap,
    system: &[RegValue],
    user: &[RegValue],
    volatile: &[RegValue],
) -> EnvMap {
    let path_of = |vals: &[RegValue]| {
        vals.iter()
            .find(|v| v.name.eq_ignore_ascii_case("PATH"))
            .cloned()
    };
    let sys_path = path_of(system);
    let user_path = path_of(user);
    for layer in [volatile, system, user, volatile] {
        // Volatile first so system values can refer to %USERPROFILE%; then again last so the
        // session's own values win.
        for v in layer {
            if v.name.eq_ignore_ascii_case("PATH") {
                continue;
            }
            let data = if v.expand {
                base.expand(&v.data)
            } else {
                v.data.clone()
            };
            base.set(&v.name, data);
        }
    }
    if sys_path.is_some() || user_path.is_some() {
        let parts: Vec<String> = [sys_path, user_path]
            .into_iter()
            .flatten()
            .map(|v| {
                if v.expand {
                    base.expand(&v.data)
                } else {
                    v.data
                }
            })
            .filter(|s| !s.is_empty())
            .collect();
        base.set("PATH", parts.join(";"));
    }
    base
}

#[cfg(windows)]
fn read_key(root: winreg::HKEY, path: &str) -> Vec<RegValue> {
    use winreg::enums::RegType;
    let Ok(key) = winreg::RegKey::predef(root).open_subkey(path) else {
        return Vec::new();
    };
    key.enum_values()
        .filter_map(|r| r.ok())
        .filter_map(|(name, value)| {
            let expand = value.vtype == RegType::REG_EXPAND_SZ;
            if !matches!(value.vtype, RegType::REG_SZ | RegType::REG_EXPAND_SZ) {
                return None;
            }
            // REG_SZ data is UTF-16LE, NUL-terminated.
            let wide: Vec<u16> = value
                .bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .take_while(|&c| c != 0)
                .collect();
            Some(RegValue {
                name,
                data: String::from_utf16_lossy(&wide),
                expand,
            })
        })
        .collect()
}

/// The environment a worker starts with, before identity variables.
pub fn base_environment() -> EnvMap {
    #[cfg(windows)]
    {
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        let system = read_key(
            HKEY_LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
        );
        let user = read_key(HKEY_CURRENT_USER, "Environment");
        let volatile = read_key(HKEY_CURRENT_USER, "Volatile Environment");
        merge_registry(EnvMap::from_process(), &system, &user, &volatile)
    }
    #[cfg(not(windows))]
    {
        EnvMap::from_process()
    }
}

/// The full worker environment for `chat_id`.
pub fn worker_environment(chat_id: &str, data_root: &str) -> EnvMap {
    let mut env = base_environment();
    env.set(WORKER_CHAT_ID, chat_id);
    env.set(WORKER_DATA_ROOT, data_root);
    env.set(NODE_COMPILE_CACHE, compile_cache_dir(data_root));
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rv(name: &str, data: &str, expand: bool) -> RegValue {
        RegValue {
            name: name.into(),
            data: data.into(),
            expand,
        }
    }

    #[test]
    fn layers_override_expand_and_join_path() {
        let mut base = EnvMap::default();
        base.set("PATH", "C:\\stale");
        base.set("KEEP", "from-process");
        let system = [
            rv("Path", "%SystemRoot%\\system32", true),
            rv("SystemRoot", "C:\\Windows", false),
            rv("WHO", "system", false),
        ];
        let user = [
            rv("PATH", "%USERPROFILE%\\bin", true),
            rv("WHO", "user", false),
            rv("TEMP", "%USERPROFILE%\\Temp", true),
        ];
        let volatile = [rv("USERPROFILE", "C:\\Users\\me", false)];
        let env = merge_registry(base, &system, &user, &volatile);
        assert_eq!(env.get("WHO"), Some("user"), "user overrides system");
        assert_eq!(env.get("TEMP"), Some("C:\\Users\\me\\Temp"));
        assert_eq!(env.get("KEEP"), Some("from-process"));
        let path = env.get("PATH").unwrap();
        assert!(path.starts_with("C:\\Windows\\system32;"), "{path}");
        assert!(path.ends_with("C:\\Users\\me\\bin"), "{path}");
        assert!(!path.contains("stale"));
    }

    #[test]
    fn unknown_references_stay_as_written() {
        let env = EnvMap::default();
        assert_eq!(env.expand("a%NOPE%b%"), "a%NOPE%b%");
    }

    #[test]
    fn identity_variables_are_set() {
        let env = worker_environment("chat-1", "C:\\data");
        assert_eq!(env.get(WORKER_CHAT_ID), Some("chat-1"));
        assert_eq!(env.get(WORKER_DATA_ROOT), Some("C:\\data"));
        assert!(env.get("PATH").is_some());
        let cache = env.get(NODE_COMPILE_CACHE).unwrap();
        assert!(
            cache.starts_with("C:\\data") && cache.ends_with("node-cache"),
            "{cache}"
        );
    }
}
