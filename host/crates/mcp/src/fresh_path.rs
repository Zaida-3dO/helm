//! Finding a program the way the system would now, not as it was when Helm started.
//!
//! A program installed while Helm runs (`uv`, say) lands on the system's PATH, but Helm and the
//! chats it starts keep the PATH they began with, so the new program stays invisible until Helm
//! is restarted. When a server's program is not on Helm's own PATH, this module reads the PATH
//! the system keeps now (the user's and the machine's, from the registry on Windows) and, if the
//! program is there, names it by its full path. Nothing is installed, and no process environment
//! is changed.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long one read of the system PATH is reused, so a burst of resolves reads it once.
const KEEP: Duration = Duration::from_secs(10);

static FRESH: Mutex<Option<(Instant, Vec<PathBuf>)>> = Mutex::new(None);

/// The value of a `Path` line in `reg query` output, if the output has one.
fn reg_path_value(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        if !words.next()?.eq_ignore_ascii_case("path") {
            return None;
        }
        let kind = words.next()?;
        if kind != "REG_SZ" && kind != "REG_EXPAND_SZ" {
            return None;
        }
        // The value keeps its own spacing: it is whatever follows the type word.
        let after_kind = line.find(kind)? + kind.len();
        let value = line[after_kind..].trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

/// `%NAME%` references in a registry value, filled from this process's environment. One that
/// names nothing is left as written (it names no folder, so nothing is found there).
fn expand(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        match tail.find('%') {
            Some(end) if end > 0 => match std::env::var(&tail[..end]) {
                Ok(v) => {
                    out.push_str(&v);
                    rest = &tail[end + 1..];
                }
                Err(_) => {
                    out.push('%');
                    rest = tail;
                }
            },
            _ => {
                out.push('%');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

fn dirs_of(value: &str) -> Vec<PathBuf> {
    expand(value)
        .split(';')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .collect()
}

#[cfg(windows)]
fn read_system_path() -> Vec<PathBuf> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let reg = std::env::var_os("SystemRoot")
        .map(|r| PathBuf::from(r).join("System32").join("reg.exe"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("reg.exe"));
    // The user's own entries first, as the system puts them, then the machine's.
    let mut dirs = Vec::new();
    for key in [
        r"HKCU\Environment",
        r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
    ] {
        let out = Command::new(&reg)
            .args(["query", key, "/v", "Path"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        if let Ok(o) = out {
            if let Some(v) = reg_path_value(&String::from_utf8_lossy(&o.stdout)) {
                dirs.extend(dirs_of(&v));
            }
        }
    }
    dirs
}

#[cfg(not(windows))]
fn read_system_path() -> Vec<PathBuf> {
    Vec::new()
}

/// The folders on the system's PATH now (reused for a few seconds).
fn system_dirs() -> Vec<PathBuf> {
    let mut slot = FRESH.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((at, dirs)) = slot.as_ref() {
        if at.elapsed() < KEEP {
            return dirs.clone();
        }
    }
    let dirs = read_system_path();
    *slot = Some((Instant::now(), dirs.clone()));
    dirs
}

fn process_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}

/// The file `name` names in the first of `dirs` that has it: `name.exe` or `name` itself.
/// Real executables only; a `.cmd` or `.bat` shim is never started. On Windows the `.exe` comes
/// first and a file with no extension is not taken at all: npm leaves a shell script of that
/// kind beside its `.cmd` shim, and it cannot be started.
fn find_in(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    let has_exe = name.to_ascii_lowercase().ends_with(".exe");
    let with_exe = (!has_exe).then(|| format!("{name}.exe"));
    let extensionless_ok = !cfg!(windows) || std::path::Path::new(name).extension().is_some();
    dirs.iter().find_map(|d| {
        if cfg!(windows) {
            if let Some(p) = with_exe
                .as_deref()
                .map(|n| d.join(n))
                .filter(|p| p.is_file())
            {
                return Some(p);
            }
        }
        let plain = d.join(name);
        if extensionless_ok && plain.is_file() {
            return Some(plain);
        }
        if cfg!(windows) {
            return None;
        }
        with_exe
            .as_deref()
            .map(|n| d.join(n))
            .filter(|p| p.is_file())
    })
}

/// Whether `command` is a bare program name (no folder in it).
fn is_bare(command: &str) -> bool {
    !command.is_empty() && !command.contains(['/', '\\'])
}

/// The command to run in place of `command`: its full path when it is a bare name that Helm's
/// own PATH does not have but the system's PATH does. `None` means leave the command as it is.
fn resolve_with(
    command: &str,
    process: &[PathBuf],
    system: &dyn Fn() -> Vec<PathBuf>,
) -> Option<String> {
    if !is_bare(command) || find_in(process, command).is_some() {
        return None;
    }
    find_in(&system(), command).map(|p| p.to_string_lossy().into_owned())
}

/// See [`resolve_with`], against this process's PATH and the system's.
pub fn resolved_command(command: &str) -> Option<String> {
    resolve_with(command, &process_dirs(), &system_dirs)
}

/// Whether `name` (a bare program name) is on Helm's PATH or the system's now.
pub fn on_path(name: &str) -> bool {
    find_in(&process_dirs(), name).is_some() || find_in(&system_dirs(), name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reading a program the system's PATH has but Helm's does not: the registry line is
    /// parsed with its variables filled, a bare name is found as `name` or `name.exe` (never a
    /// shim), a program already on Helm's PATH or given by path is left alone, and a name found
    /// nowhere stays as it is.
    #[test]
    fn a_program_installed_after_start_is_found_on_the_system_path() {
        let out = "\r\nHKEY_CURRENT_USER\\Environment\r\n    Path    REG_EXPAND_SZ    C:\\a b\\bin;%FRESH_TEST_HOME%\\tools\r\n\r\n";
        assert_eq!(
            reg_path_value(out).as_deref(),
            Some("C:\\a b\\bin;%FRESH_TEST_HOME%\\tools")
        );
        assert_eq!(reg_path_value("    TEMP    REG_SZ    C:\\t\r\n"), None);
        assert_eq!(reg_path_value("ERROR: The system was unable to find"), None);
        std::env::set_var("FRESH_TEST_HOME", "D:\\home");
        let dirs = dirs_of("C:\\a b\\bin;%FRESH_TEST_HOME%\\tools;;%NOT_SET_ANYWHERE%\\x");
        assert_eq!(dirs[0], PathBuf::from("C:\\a b\\bin"));
        assert_eq!(dirs[1], PathBuf::from("D:\\home\\tools"));
        assert_eq!(dirs[2], PathBuf::from("%NOT_SET_ANYWHERE%\\x"));
        std::env::remove_var("FRESH_TEST_HOME");

        let new = tempfile::tempdir().unwrap();
        let old = tempfile::tempdir().unwrap();
        std::fs::write(new.path().join("uvx.exe"), b"x").unwrap();
        std::fs::write(new.path().join("shimmed.cmd"), b"x").unwrap();
        std::fs::write(old.path().join("here.exe"), b"x").unwrap();
        std::fs::write(new.path().join("here.exe"), b"x").unwrap();
        let process = vec![old.path().to_path_buf()];
        let system = || vec![new.path().to_path_buf()];
        let found = resolve_with("uvx", &process, &system).unwrap();
        assert_eq!(PathBuf::from(&found), new.path().join("uvx.exe"));
        assert_eq!(
            resolve_with("uvx.exe", &process, &system).as_deref(),
            Some(found.as_str())
        );
        for left_alone in ["here", "here.exe", "shimmed", "nowhere", "", "sub/uvx"] {
            assert_eq!(
                resolve_with(left_alone, &process, &system),
                None,
                "{left_alone}"
            );
        }
        // npm leaves an extensionless shell script beside its `.cmd` shim: on Windows the
        // `.exe` wins and a script on its own is not a program.
        std::fs::write(new.path().join("npmtool"), b"x").unwrap();
        assert_eq!(
            find_in(&[new.path().to_path_buf()], "npmtool").is_none(),
            cfg!(windows)
        );
        std::fs::write(new.path().join("npmtool.exe"), b"x").unwrap();
        assert_eq!(
            find_in(&[new.path().to_path_buf()], "npmtool"),
            Some(new.path().join("npmtool.exe"))
        );
        let by_path = new.path().join("uvx.exe").to_string_lossy().into_owned();
        assert_eq!(resolve_with(&by_path, &process, &system), None);
    }
}
