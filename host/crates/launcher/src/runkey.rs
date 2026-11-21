//! Starting the launcher at logon: one value under the user's own `Run` key.
//!
//! Everything goes through [`RunKey`], so tests use [`MemoryRunKey`] and never the real
//! registry. Debug builds get [`NoRunKey`] unless `HELM_TEST_LAUNCHER_REGISTER=1` is set, so a
//! developer's own logon is never changed by a test run.

use std::io;
use std::path::Path;
use std::sync::Arc;

/// Where Windows starts per-user programs at logon.
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// The value for the default profile (the uninstaller removes this one).
pub const RUN_VALUE: &str = "Helm launcher";
/// Debug builds only: let this run write the real `Run` key.
pub const TEST_REGISTER_ENV: &str = "HELM_TEST_LAUNCHER_REGISTER";

pub trait RunKey: Send + Sync {
    fn get(&self, name: &str) -> Option<String>;
    fn set(&self, name: &str, command: &str) -> io::Result<()>;
    fn remove(&self, name: &str) -> io::Result<()>;
}

/// The value's name: [`RUN_VALUE`] for the default profile, and one with the profile's short hash
/// for any other, so a second profile never takes over the first one's entry.
pub fn value_name(is_default_profile: bool, profile_hash: &str) -> String {
    if is_default_profile {
        RUN_VALUE.to_owned()
    } else {
        format!(
            "{RUN_VALUE} ({})",
            &profile_hash[..profile_hash.len().min(8)]
        )
    }
}

/// A path as Windows shows it, without the verbatim `\\?\` prefix on drive paths.
pub fn plain_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_owned(),
        _ => s.into_owned(),
    }
}

/// The command line the value holds: both paths quoted, so spaces are safe, and `--at-logon`
/// says this start came from Windows at sign-in (Helm starts the same program without it, and
/// the launcher opens Helm at sign-in only for the Windows start).
pub fn command(exe: &Path, profile_dir: &Path) -> String {
    format!(
        "\"{}\" --profile \"{}\" --at-logon",
        plain_path(exe),
        plain_path(profile_dir)
    )
}

/// The registry this build may touch: the real one in release builds (and in a debug build that
/// asked for it), else none.
pub fn for_this_build() -> Arc<dyn RunKey> {
    let allowed =
        !cfg!(debug_assertions) || std::env::var(TEST_REGISTER_ENV).ok().as_deref() == Some("1");
    if allowed {
        native()
    } else {
        Arc::new(NoRunKey)
    }
}

#[cfg(windows)]
fn native() -> Arc<dyn RunKey> {
    Arc::new(NativeRunKey)
}

#[cfg(not(windows))]
fn native() -> Arc<dyn RunKey> {
    Arc::new(NoRunKey)
}

/// Writes nothing and reports nothing registered.
pub struct NoRunKey;

impl RunKey for NoRunKey {
    fn get(&self, _name: &str) -> Option<String> {
        None
    }
    fn set(&self, _name: &str, _command: &str) -> io::Result<()> {
        Ok(())
    }
    fn remove(&self, _name: &str) -> io::Result<()> {
        Ok(())
    }
}

/// `HKEY_CURRENT_USER\...\Run`.
#[cfg(windows)]
pub struct NativeRunKey;

#[cfg(windows)]
impl RunKey for NativeRunKey {
    fn get(&self, name: &str) -> Option<String> {
        use windows_sys::Win32::System::Registry::{
            RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ,
        };
        let (key, value) = (wide(RUN_KEY), wide(name));
        let mut buf = vec![0u16; 2048];
        let mut bytes = (buf.len() * 2) as u32;
        // SAFETY: the buffer's size in bytes is passed with it; the names live through the call.
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if rc != 0 {
            return None;
        }
        let len = (bytes as usize / 2).min(buf.len());
        let s = String::from_utf16_lossy(&buf[..len]);
        Some(s.trim_end_matches('\0').to_owned())
    }

    fn set(&self, name: &str, command: &str) -> io::Result<()> {
        use windows_sys::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
        let (key, value, data) = (wide(RUN_KEY), wide(name), wide(command));
        // SAFETY: the data's size in bytes includes its terminator; the names live through the call.
        let rc = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(rc as i32))
        }
    }

    fn remove(&self, name: &str) -> io::Result<()> {
        use windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND;
        use windows_sys::Win32::System::Registry::{RegDeleteKeyValueW, HKEY_CURRENT_USER};
        let (key, value) = (wide(RUN_KEY), wide(name));
        // SAFETY: the names live through the call.
        let rc = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) };
        if rc == 0 || rc == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(rc as i32))
        }
    }
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(Some(0))
        .collect()
}

/// An in-memory `Run` key for tests.
#[cfg(test)]
#[derive(Default)]
pub struct MemoryRunKey {
    pub values: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
}

#[cfg(test)]
impl RunKey for MemoryRunKey {
    fn get(&self, name: &str) -> Option<String> {
        self.values.lock().unwrap().get(name).cloned()
    }
    fn set(&self, name: &str, command: &str) -> io::Result<()> {
        self.values
            .lock()
            .unwrap()
            .insert(name.to_owned(), command.to_owned());
        Ok(())
    }
    fn remove(&self, name: &str) -> io::Result<()> {
        self.values.lock().unwrap().remove(name);
        Ok(())
    }
}
