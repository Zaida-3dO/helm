//! The few Windows calls the launcher and the app need: is a process Helm and alive, wait for it
//! to end, one launcher per profile (a named mutex), the stop signal (a named event), starting a
//! program detached, and whether Windows is shutting down. Other systems get inert stand-ins: the
//! launcher is Windows only.

use std::ffi::OsStr;
use std::io;
use std::path::Path;

/// The mutex one launcher per profile holds. Named from the profile's hash, so two profiles each
/// get their own launcher.
pub fn instance_name(profile_hash: &str) -> String {
    format!(
        "Local\\HelmLauncher-{}",
        &profile_hash[..profile_hash.len().min(32)]
    )
}

/// The event that asks that profile's launcher to stop.
pub fn stop_name(profile_hash: &str) -> String {
    format!(
        "Local\\HelmLauncherStop-{}",
        &profile_hash[..profile_hash.len().min(32)]
    )
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, STILL_ACTIVE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateMutexW, GetExitCodeProcess, OpenEventW, OpenMutexW, OpenProcess,
        QueryFullProcessImageNameW, ResetEvent, SetEvent, WaitForSingleObject,
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS,
        EVENT_MODIFY_STATE, INFINITE, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, SYNCHRONIZATION_SYNCHRONIZE,
    };

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    /// A kernel handle closed on drop.
    pub struct Handle(HANDLE);

    // SAFETY: a kernel handle may be used and closed from any thread.
    unsafe impl Send for Handle {}
    unsafe impl Sync for Handle {}

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle is ours and is closed once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    impl Handle {
        fn open(h: HANDLE) -> Option<Handle> {
            (!h.is_null()).then_some(Handle(h))
        }

        /// Block until the object is signalled.
        pub fn wait(&self) {
            // SAFETY: a valid handle opened with SYNCHRONIZE.
            unsafe {
                WaitForSingleObject(self.0, INFINITE);
            }
        }

        /// Whether the object is signalled now, without waiting.
        pub fn signalled(&self) -> bool {
            // SAFETY: as above, with no wait.
            unsafe { WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 }
        }
    }

    /// The process is alive and its program file is named `exe_name` (any case).
    pub fn process_is(pid: u32, exe_name: &str) -> bool {
        // SAFETY: a query-only handle, closed on drop; the name buffer is sized for the call.
        unsafe {
            let Some(h) = Handle::open(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid))
            else {
                return false;
            };
            let mut code = 0u32;
            if GetExitCodeProcess(h.0, &mut code) == 0 || code != STILL_ACTIVE as u32 {
                return false;
            }
            let mut buf = vec![0u16; 1024];
            let mut len = buf.len() as u32;
            if QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) == 0
            {
                return false;
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            Path::new(&path)
                .file_name()
                .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(exe_name))
        }
    }

    pub fn process_alive(pid: u32) -> bool {
        // SAFETY: a query-only handle, closed on drop.
        unsafe {
            let Some(h) = Handle::open(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid))
            else {
                return false;
            };
            let mut code = 0u32;
            GetExitCodeProcess(h.0, &mut code) != 0 && code == STILL_ACTIVE as u32
        }
    }

    /// A handle to wait on the process's end, or `None` when it is already gone.
    pub fn process_handle(pid: u32) -> Option<Handle> {
        // SAFETY: a synchronise-only handle, closed on drop.
        unsafe { Handle::open(OpenProcess(PROCESS_SYNCHRONIZE, 0, pid)) }
    }

    /// Take the named mutex: `None` when another process already holds it.
    pub fn take_instance(name: &str) -> Option<Handle> {
        let n = wide(name);
        // SAFETY: a named mutex with default security; the name lives through the call.
        unsafe {
            let h = Handle::open(CreateMutexW(std::ptr::null(), 0, n.as_ptr()))?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                return None;
            }
            Some(h)
        }
    }

    /// Whether some process holds the named mutex.
    pub fn instance_exists(name: &str) -> bool {
        let n = wide(name);
        // SAFETY: open-only, closed on drop.
        unsafe { Handle::open(OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, n.as_ptr())).is_some() }
    }

    /// Create the stop event (manual reset), cleared: a stop meant for a launcher that already
    /// exited does not stop this one.
    pub fn stop_event(name: &str) -> Option<Handle> {
        let n = wide(name);
        // SAFETY: a named event with default security; the name lives through the call.
        unsafe {
            let h = Handle::open(CreateEventW(std::ptr::null(), 1, 0, n.as_ptr()))?;
            ResetEvent(h.0);
            Some(h)
        }
    }

    /// Ask that launcher to stop. `false` when none is listening.
    pub fn signal_stop(name: &str) -> bool {
        let n = wide(name);
        // SAFETY: open for set-state only, closed on drop.
        unsafe {
            match Handle::open(OpenEventW(EVENT_MODIFY_STATE, 0, n.as_ptr())) {
                Some(h) => SetEvent(h.0) != 0,
                None => false,
            }
        }
    }

    /// Start a program on its own: no console, no window of ours, and out of our job when the job
    /// allows it, so it outlives us. `env` sets a variable (`Some`) or removes it (`None`).
    /// Returns its process id.
    pub fn spawn_detached(
        exe: &Path,
        args: &[&OsStr],
        env: &[(&str, Option<&OsStr>)],
    ) -> io::Result<u32> {
        let build = |flags: u32| {
            let mut c = std::process::Command::new(exe);
            c.args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .creation_flags(flags);
            if let Some(dir) = exe.parent() {
                c.current_dir(dir);
            }
            for (k, v) in env {
                match v {
                    Some(v) => c.env(k, v),
                    None => c.env_remove(k),
                };
            }
            c
        };
        let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
        match build(base | CREATE_BREAKAWAY_FROM_JOB).spawn() {
            Ok(child) => Ok(child.id()),
            // A job that does not allow breaking away refuses the flag: start it inside the job.
            Err(_) => build(base).spawn().map(|c| c.id()),
        }
    }

    pub fn shutting_down() -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SHUTTINGDOWN};
        // SAFETY: a plain query.
        unsafe { GetSystemMetrics(SM_SHUTTINGDOWN) != 0 }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub struct Handle;

    impl Handle {
        pub fn wait(&self) {}
        pub fn signalled(&self) -> bool {
            false
        }
    }

    pub fn process_is(_pid: u32, _exe_name: &str) -> bool {
        false
    }
    pub fn process_alive(_pid: u32) -> bool {
        false
    }
    pub fn process_handle(_pid: u32) -> Option<Handle> {
        None
    }
    pub fn take_instance(_name: &str) -> Option<Handle> {
        None
    }
    pub fn instance_exists(_name: &str) -> bool {
        false
    }
    pub fn stop_event(_name: &str) -> Option<Handle> {
        None
    }
    pub fn signal_stop(_name: &str) -> bool {
        false
    }
    pub fn spawn_detached(
        _exe: &Path,
        _args: &[&OsStr],
        _env: &[(&str, Option<&OsStr>)],
    ) -> io::Result<u32> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the launcher is Windows only",
        ))
    }
    pub fn shutting_down() -> bool {
        false
    }
}

pub use imp::*;
