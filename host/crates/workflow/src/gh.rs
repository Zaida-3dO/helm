//! Running `gh` for the land step. The real `gh.exe` under the user's own login, one
//! call at a time with a 20-second deadline, output capped at 256 KiB. Tests
//! use a scripted [`Gh`].

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const CALL_TIMEOUT: Duration = Duration::from_secs(20);
pub const MAX_OUTPUT: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Out {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GhError {
    /// The call ran past its deadline (the land step waits and tries again).
    Timeout,
    /// `gh` could not be started.
    Missing(String),
}

pub trait Gh: Send + Sync {
    fn run(&self, args: &[String]) -> Result<Out, GhError>;
}

/// The real `gh`.
pub struct GhCli;

impl Gh for GhCli {
    fn run(&self, args: &[String]) -> Result<Out, GhError> {
        let mut child = Command::new("gh")
            .args(args)
            .env("GH_PROMPT_DISABLED", "1")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| GhError::Missing(e.to_string()))?;
        let mut so = child.stdout.take().expect("piped");
        let mut se = child.stderr.take().expect("piped");
        let t_out = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = so.by_ref().take(MAX_OUTPUT as u64).read_to_end(&mut b);
            b
        });
        let t_err = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = se.by_ref().take(16 * 1024).read_to_end(&mut b);
            b
        });
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if started.elapsed() < CALL_TIMEOUT => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(GhError::Timeout);
                }
            }
        };
        let stdout = String::from_utf8_lossy(&t_out.join().unwrap_or_default()).into_owned();
        let stderr = String::from_utf8_lossy(&t_err.join().unwrap_or_default()).into_owned();
        Ok(Out {
            ok: status.success(),
            stdout,
            stderr,
        })
    }
}

/// `owner/name` for a `github.com/owner/name` repository; other hosts are not landed.
pub fn gh_repo(repo: &str) -> Option<String> {
    repo.strip_prefix("github.com/")
        .filter(|r| r.matches('/').count() == 1)
        .map(str::to_owned)
}

/// The PR number of a `https://github.com/<owner>/<name>/pull/<n>` URL.
pub fn pr_number(pr: &str) -> Option<String> {
    let (_, n) = pr.rsplit_once("/pull/")?;
    let n = n.trim_end_matches('/');
    (!n.is_empty() && n.len() <= 9 && n.bytes().all(|b| b.is_ascii_digit())).then(|| n.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repos_and_pr_numbers_are_read_strictly() {
        assert_eq!(gh_repo("github.com/o/r").as_deref(), Some("o/r"));
        assert_eq!(gh_repo("gitlab.com/o/r"), None);
        assert_eq!(
            pr_number("https://github.com/o/r/pull/12").as_deref(),
            Some("12")
        );
        assert_eq!(pr_number("https://github.com/o/r/pull/12x"), None);
        assert_eq!(pr_number("#12"), None);
    }
}
