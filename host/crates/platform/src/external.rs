//! Opening a URL in the user's default handler. The scheme
//! check is the caller's (helm-shell re-checks http/https/mailto); this only hands the string to
//! the OS.

pub trait OpenExternal: Send + Sync {
    fn open_url(&self, url: &str) -> Result<(), String>;
}

pub struct NativeOpen;

#[cfg(windows)]
impl OpenExternal for NativeOpen {
    fn open_url(&self, url: &str) -> Result<(), String> {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let verb = crate::fs::wide("open");
        let target = crate::fs::wide(url);
        // SAFETY: NUL-terminated buffers outlive the call; no parameters or directory are passed.
        let r = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if (r as isize) <= 32 {
            return Err(format!(
                "the system could not open the link (code {})",
                r as isize
            ));
        }
        Ok(())
    }
}

#[cfg(not(windows))]
impl OpenExternal for NativeOpen {
    fn open_url(&self, url: &str) -> Result<(), String> {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("xdg-open: {e}"))
    }
}

/// Records what it was asked to open; for tests and the debug probe.
#[derive(Default)]
pub struct RecordingOpen(pub std::sync::Mutex<Vec<String>>);
impl OpenExternal for RecordingOpen {
    fn open_url(&self, url: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(url.to_owned());
        Ok(())
    }
}
