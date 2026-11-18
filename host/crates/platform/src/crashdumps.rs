//! A last word when the host dies inside native code, where no log line can be reached.
//!
//! [`install`] puts one exception filter on the process. When Windows is about to end the host
//! after an unhandled native fault, the filter writes a crash dump into the profile's
//! `diagnostics` folder and hands on to whatever handler was there before. Dumps roll: the
//! three newest `crash-<time>.dmp` files are kept.
//!
//! The smallest dump kind is written — the thread stacks and the loaded-module list, nothing
//! more. A dump holds the process's memory at that moment (that is its whole point), which is
//! why it stays in the profile's own diagnostics folder, never leaves the machine, and is
//! limited to a bounded few files.
//!
//! Off Windows there is nothing to install.

#[cfg(windows)]
mod imp {
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;
    use windows_sys::Win32::System::Diagnostics::Debug::{
        MiniDumpNormal, MiniDumpWriteDump, SetUnhandledExceptionFilter, EXCEPTION_POINTERS,
        LPTOP_LEVEL_EXCEPTION_FILTER, MINIDUMP_EXCEPTION_INFORMATION,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId,
    };

    /// Where dumps go, as set by [`super::install`].
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    /// The filter that was in place before Helm's, if any (a debugger, the runtime's own).
    static PREVIOUS: OnceLock<LPTOP_LEVEL_EXCEPTION_FILTER> = OnceLock::new();
    /// How many dumps to keep.
    const KEEP: usize = 3;
    /// Windows: keep going with the normal unhandled-exception behaviour.
    const CONTINUE_SEARCH: i32 = 0;

    pub fn install(diagnostics: &Path) {
        if DIR.set(diagnostics.to_path_buf()).is_err() {
            return;
        }
        // SAFETY: the filter is a plain function pointer, valid for the life of the process.
        let previous = unsafe { SetUnhandledExceptionFilter(Some(filter)) };
        let _ = PREVIOUS.set(previous);
    }

    unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
        let dir = match DIR.get() {
            Some(d) => d.clone(),
            None => return CONTINUE_SEARCH,
        };
        let _ = std::fs::create_dir_all(&dir);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let path = dir.join(format!("crash-{now_ms}.dmp"));
        if let Ok(file) = std::fs::File::create(&path) {
            // The dump names this thread and points at the fault's records.
            let details = (!info.is_null()).then(|| MINIDUMP_EXCEPTION_INFORMATION {
                ThreadId: GetCurrentThreadId(),
                ExceptionPointers: info as *mut _,
                ClientPointers: 1,
            });
            let ok = MiniDumpWriteDump(
                GetCurrentProcess(),
                GetCurrentProcessId(),
                file.as_raw_handle(),
                MiniDumpNormal,
                details.as_ref().map_or(std::ptr::null(), |d| d),
                std::ptr::null(),
                std::ptr::null(),
            );
            if ok == 0 {
                let _ = std::fs::remove_file(&path);
            }
        }
        // Roll: the name starts with the time, so sorting the names is sorting by time.
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut dumps: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .map(|n| {
                            let n = n.to_string_lossy();
                            n.starts_with("crash-") && n.ends_with(".dmp")
                        })
                        .unwrap_or(false)
                })
                .collect();
            dumps.sort();
            for old in dumps.iter().rev().skip(KEEP) {
                let _ = std::fs::remove_file(old);
            }
        }
        match PREVIOUS.get().and_then(|f| *f) {
            Some(next) => next(info),
            None => CONTINUE_SEARCH,
        }
    }
}

/// Start writing crash dumps into `diagnostics`. Safe to call once.
#[cfg(windows)]
pub fn install(diagnostics: &std::path::Path) {
    imp::install(diagnostics)
}

/// Off Windows there is nothing to install.
#[cfg(not(windows))]
pub fn install(_diagnostics: &std::path::Path) {}
