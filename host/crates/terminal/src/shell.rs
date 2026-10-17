//! Which shell a terminal runs: PowerShell 7 (`pwsh.exe`), then Windows PowerShell;
//! Git Bash when chosen in Settings → Shell. Always a real `.exe`, found on PATH or in the
//! standard install folders.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellProgram {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// "pwsh", "powershell" or "bash".
    pub title: String,
}

/// Find `name` on PATH (exact file name, so `.cmd` shims are never picked).
pub fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

fn first_file(cands: &[PathBuf]) -> Option<PathBuf> {
    cands.iter().find(|p| p.is_file()).cloned()
}

fn program_files() -> Vec<PathBuf> {
    ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|k| std::env::var_os(k).map(PathBuf::from))
        .collect()
}

fn pwsh() -> Option<ShellProgram> {
    let p = on_path("pwsh.exe").or_else(|| {
        first_file(
            &program_files()
                .iter()
                .map(|d| d.join("PowerShell").join("7").join("pwsh.exe"))
                .collect::<Vec<_>>(),
        )
    })?;
    Some(ShellProgram {
        program: p,
        args: vec!["-NoLogo".into()],
        title: "pwsh".into(),
    })
}

fn windows_powershell() -> Option<ShellProgram> {
    let sys = std::env::var_os("SystemRoot").map(PathBuf::from);
    let p = on_path("powershell.exe").or_else(|| {
        sys.map(|s| s.join("System32\\WindowsPowerShell\\v1.0\\powershell.exe"))
            .filter(|p| p.is_file())
    })?;
    Some(ShellProgram {
        program: p,
        args: vec!["-NoLogo".into()],
        title: "powershell".into(),
    })
}

fn git_bash() -> Option<ShellProgram> {
    let p = first_file(
        &program_files()
            .iter()
            .map(|d| d.join("Git").join("bin").join("bash.exe"))
            .collect::<Vec<_>>(),
    )
    .or_else(|| on_path("bash.exe").filter(|p| !is_wsl_launcher(p)))?;
    Some(ShellProgram {
        program: p,
        args: vec!["--login".into(), "-i".into()],
        title: "bash".into(),
    })
}

/// `C:\Windows\System32\bash.exe` starts WSL, not Git Bash.
fn is_wsl_launcher(p: &Path) -> bool {
    p.to_string_lossy()
        .to_ascii_lowercase()
        .contains("\\system32\\")
}

/// The shell for a preference value; falls back pwsh → Windows PowerShell (→ sh on Linux).
pub fn resolve(pref: &str) -> Option<ShellProgram> {
    let chosen = match pref {
        "gitbash" => git_bash(),
        "powershell" => windows_powershell(),
        _ => pwsh(),
    };
    chosen
        .or_else(pwsh)
        .or_else(windows_powershell)
        .or_else(|| {
            let sh = std::env::var_os("SHELL")
                .map(PathBuf::from)
                .or_else(|| on_path("bash"))?;
            Some(ShellProgram {
                program: sh,
                args: vec!["-l".into()],
                title: "bash".into(),
            })
        })
}

/// Environment variables internal to the app are not passed to the shell: Helm's
/// own, Tauri's and WebView2's, plus the Claude session markers the worker strips (CLAUDE.md
/// hard rule 2).
pub fn is_internal_env(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.starts_with("HELM_")
        || n.starts_with("TAURI_")
        || n.starts_with("WEBVIEW2_")
        || n == "CLAUDECODE"
        || n.starts_with("CLAUDE_CODE_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_variables_are_stripped_and_others_kept() {
        for k in [
            "HELM_TEST_PROBE",
            "helm_profile_dir",
            "TAURI_ENV_DEBUG",
            "WEBVIEW2_USER_DATA_FOLDER",
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
        ] {
            assert!(is_internal_env(k), "{k}");
        }
        for k in ["PATH", "USERPROFILE", "TERM", "GIT_DIR", "HELMET"] {
            assert!(!is_internal_env(k), "{k}");
        }
    }

    #[test]
    #[cfg(windows)]
    fn some_powershell_is_always_found_on_windows() {
        let s = resolve("pwsh").expect("a shell");
        assert!(
            s.program.extension().is_some_and(|e| e == "exe"),
            "a real .exe"
        );
        assert!(["pwsh", "powershell"].contains(&s.title.as_str()));
        assert!(!is_wsl_launcher(Path::new(
            "C:\\Program Files\\Git\\bin\\bash.exe"
        )));
        assert!(is_wsl_launcher(Path::new(
            "C:\\Windows\\System32\\bash.exe"
        )));
    }
}
