//! Native confirmation dialogs (the quit "sheet").

pub trait Dialogs: Send + Sync {
    /// Ask a yes/no question. Blocks the calling thread; call it off the UI event loop.
    /// Returns true when the user confirms.
    fn confirm(&self, title: &str, message: &str) -> bool;
}

pub struct NativeDialogs;

#[cfg(windows)]
impl Dialogs for NativeDialogs {
    fn confirm(&self, title: &str, message: &str) -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            MessageBoxW, IDOK, MB_ICONQUESTION, MB_OKCANCEL, MB_SETFOREGROUND, MB_TOPMOST,
        };
        let title = crate::fs::wide(title);
        let message = crate::fs::wide(message);
        // SAFETY: both strings are NUL-terminated UTF-16 buffers that outlive the call.
        let answer = unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                message.as_ptr(),
                title.as_ptr(),
                MB_OKCANCEL | MB_ICONQUESTION | MB_SETFOREGROUND | MB_TOPMOST,
            )
        };
        answer == IDOK
    }
}

#[cfg(not(windows))]
impl Dialogs for NativeDialogs {
    fn confirm(&self, title: &str, message: &str) -> bool {
        // Linux has no native dialog yet; the quit path proceeds and says so in the log.
        log::warn!("no native dialog on this platform; confirming '{title}': {message}");
        true
    }
}

/// A scripted answer, for tests and QA builds.
pub struct FixedDialogs(pub bool);
impl Dialogs for FixedDialogs {
    fn confirm(&self, _title: &str, _message: &str) -> bool {
        self.0
    }
}
