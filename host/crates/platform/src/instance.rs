//! Single-instance signalling.
//!
//! The lock itself is `app.lock` in the profile (held by `helm-core`). This module is the channel a
//! second launch uses to tell the first one to show its window: a named pipe on Windows
//! (`\\.\pipe\helm-<hash>`, local clients only), a Unix socket in the profile on Linux. The endpoint
//! name comes from the profile, so two profiles never signal each other.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The one message the channel carries.
pub const FOCUS_MESSAGE: &[u8] = b"focus\n";

/// Where the first instance listens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// Windows: the full pipe name. Linux: the socket path.
    pub name: String,
}

impl Endpoint {
    /// `profile_hash` is a stable hex hash of the profile path; `profile_dir` is the profile.
    pub fn for_profile(profile_dir: &Path, profile_hash: &str) -> Self {
        let short = &profile_hash[..profile_hash.len().min(16)];
        if cfg!(windows) {
            let _ = profile_dir;
            Self {
                name: format!(r"\\.\pipe\helm-{short}"),
            }
        } else {
            let path: PathBuf = profile_dir.join("app.sock");
            Self {
                name: path.to_string_lossy().into_owned(),
            }
        }
    }
}

/// Second instance: ask the first to focus. Retries for up to `wait` while the first is still
/// starting its listener.
pub fn signal_focus(endpoint: &Endpoint, wait: Duration) -> io::Result<()> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        match imp::send(endpoint) {
            Ok(()) => return Ok(()),
            Err(e) if std::time::Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// First instance: listen forever, calling `on_focus` for each focus request. Runs on the tokio
/// runtime the caller provides.
pub async fn serve(
    endpoint: Endpoint,
    on_focus: impl Fn() + Send + Sync + 'static,
) -> io::Result<()> {
    imp::serve(endpoint, on_focus).await
}

#[cfg(windows)]
mod imp {
    use super::{Endpoint, FOCUS_MESSAGE};
    use std::io::{self, Write};
    use tokio::io::AsyncReadExt;
    use tokio::net::windows::named_pipe::{PipeMode, ServerOptions};

    pub fn send(endpoint: &Endpoint) -> io::Result<()> {
        let mut pipe = std::fs::OpenOptions::new()
            .write(true)
            .open(&endpoint.name)?;
        pipe.write_all(FOCUS_MESSAGE)?;
        pipe.flush()
    }

    pub async fn serve(
        endpoint: Endpoint,
        on_focus: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<()> {
        let mut opts = ServerOptions::new();
        opts.access_inbound(true)
            .access_outbound(false)
            .pipe_mode(PipeMode::Byte)
            .reject_remote_clients(true);
        let mut server = opts
            .clone()
            .first_pipe_instance(true)
            .create(&endpoint.name)?;
        loop {
            server.connect().await?;
            let mut connected = server;
            server = opts.create(&endpoint.name)?;
            let mut buf = [0u8; 16];
            // Bounded read: anything other than the focus line is ignored.
            let n =
                tokio::time::timeout(std::time::Duration::from_secs(2), connected.read(&mut buf))
                    .await
                    .unwrap_or(Ok(0))
                    .unwrap_or(0);
            if &buf[..n] == FOCUS_MESSAGE {
                on_focus();
            }
        }
    }
}

#[cfg(unix)]
mod imp {
    use super::{Endpoint, FOCUS_MESSAGE};
    use std::io::{self, Write};
    use tokio::io::AsyncReadExt;

    pub fn send(endpoint: &Endpoint) -> io::Result<()> {
        let mut s = std::os::unix::net::UnixStream::connect(&endpoint.name)?;
        s.write_all(FOCUS_MESSAGE)
    }

    pub async fn serve(
        endpoint: Endpoint,
        on_focus: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<()> {
        // The lock is held, so a leftover socket file is stale.
        let _ = std::fs::remove_file(&endpoint.name);
        let listener = tokio::net::UnixListener::bind(&endpoint.name)?;
        crate::fs::restrict_to_owner(std::path::Path::new(&endpoint.name))?;
        loop {
            let (mut stream, _) = listener.accept().await?;
            let mut buf = [0u8; 16];
            let n = tokio::time::timeout(std::time::Duration::from_secs(2), stream.read(&mut buf))
                .await
                .unwrap_or(Ok(0))
                .unwrap_or(0);
            if &buf[..n] == FOCUS_MESSAGE {
                on_focus();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn a_second_launch_signal_reaches_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let hash = uuid::Uuid::new_v4().simple().to_string();
        let endpoint = Endpoint::for_profile(dir.path(), &hash);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        rt.spawn(serve(endpoint.clone(), move || {
            h.fetch_add(1, Ordering::SeqCst);
        }));
        signal_focus(&endpoint, Duration::from_secs(5)).unwrap();
        signal_focus(&endpoint, Duration::from_secs(5)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while hits.load(Ordering::SeqCst) < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn no_listener_means_the_signal_fails() {
        let dir = tempfile::tempdir().unwrap();
        let hash = uuid::Uuid::new_v4().simple().to_string();
        let endpoint = Endpoint::for_profile(dir.path(), &hash);
        assert!(signal_focus(&endpoint, Duration::from_millis(100)).is_err());
    }
}
