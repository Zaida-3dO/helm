//! The host's own log written to a file, for release builds where stderr is lost
//! under `windows_subsystem = "windows"`.
//!
//! The file rolls like the other diagnostics logs: past [`FILE_CAP`] it is renamed to
//! `<name>.old.log` (replacing any older one) and a fresh file starts. A log line that
//! cannot be written is dropped: the logger never takes the app down.

use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Mutex;

/// The log file is kept under this size; one older file is kept.
pub const FILE_CAP: u64 = 2 * 1024 * 1024;

/// A size-capped rolling file behind a mutex, usable as an `env_logger` sink.
pub struct RollingFile {
    path: PathBuf,
    cap: u64,
    inner: Mutex<Option<Open>>,
}

struct Open {
    file: File,
    written: u64,
}

impl RollingFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        RollingFile {
            path: path.into(),
            cap: FILE_CAP,
            inner: Mutex::new(None),
        }
    }

    #[cfg(test)]
    fn with_cap(mut self, cap: u64) -> Self {
        self.cap = cap;
        self
    }

    /// Open the file for appending, rolling the old one away first when it has passed the cap.
    fn open_fresh(&self) -> io::Result<Open> {
        if let Some(parent) = self.path.parent() {
            crate::privfs::create_private_dir_all(parent)?;
        }
        let len = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        let written = if len > self.cap {
            let old = self.path.with_extension("old.log");
            let _ = std::fs::remove_file(&old);
            std::fs::rename(&self.path, &old)?;
            0
        } else {
            len
        };
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        Ok(Open { file, written })
    }
}

impl Write for RollingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if guard
            .as_ref()
            .is_none_or(|o| o.written + buf.len() as u64 > self.cap)
        {
            if let Some(mut open) = guard.take() {
                let _ = open.file.flush();
            }
            *guard = Some(self.open_fresh()?);
        }
        let open = guard.as_mut().expect("opened above");
        open.file.write_all(buf)?;
        open.written += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
            Some(open) => open.file.flush(),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn lines_append_and_the_file_rolls_once_past_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostics").join("host.log");
        let mut log = RollingFile::new(&path).with_cap(40);
        for i in 0..8 {
            writeln!(log, "line {i:0>2} padded to size").unwrap();
        }
        log.flush().unwrap();
        // A restart over a file past the cap also rolls it before appending.
        let mut again = RollingFile::new(&path).with_cap(40);
        writeln!(again, "the newest line").unwrap();
        again.flush().unwrap();
        let current = std::fs::read_to_string(&path).unwrap();
        assert!(current.contains("the newest line"));
        assert!(
            current.len() <= 40 + 16,
            "the current file stays near the cap"
        );
        let rolled = dir.path().join("diagnostics").join("host.old.log");
        let old = std::fs::read_to_string(&rolled).unwrap();
        assert!(old.contains("line 0"), "older lines survive, once");
        assert!(!old.contains("the newest line"));
        // And one log plus one older file is all the folder holds.
        let files: Vec<_> = std::fs::read_dir(dir.path().join("diagnostics"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files, ["host.log", "host.old.log"]);
    }
}
