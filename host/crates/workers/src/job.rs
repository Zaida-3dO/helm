//! One kill-on-close group per worker tree (process groups become Job Objects on Windows).
//!
//! On Windows a [`Job`] is a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: the worker is
//! assigned to it straight after spawning, every process the worker starts (the claude CLI and
//! anything claude runs) joins it automatically, and closing the last handle, or
//! [`Job::terminate`], ends the whole tree. On Linux the worker leads its own process group and
//! `terminate` sends `SIGKILL` to the group.
//!
//! The assignment happens after `CreateProcess` returns, not from a suspended start: Node takes
//! tens of milliseconds before it can start a child, so nothing escapes the job in practice.

use std::io;

#[cfg(windows)]
pub use win::Job;

#[cfg(unix)]
pub use unix::Job;

#[cfg(windows)]
mod win {
    use super::*;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// A Job Object that kills its processes when dropped.
    pub struct Job {
        handle: HANDLE,
    }

    // A job handle may be used from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn new() -> io::Result<Self> {
            // SAFETY: plain Win32 calls; the handle is owned by the returned value.
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let job = Job { handle };
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job.handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(job)
            }
        }

        /// Put a spawned process (and so its future children) into the job.
        pub fn assign(&self, child: &tokio::process::Child) -> io::Result<()> {
            let raw = child
                .raw_handle()
                .ok_or_else(|| io::Error::other("the process has already exited"))?;
            // SAFETY: `raw` is the live process handle owned by `child`.
            let ok = unsafe { AssignProcessToJobObject(self.handle, raw as HANDLE) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        /// End every process in the job now.
        pub fn terminate(&self) {
            // SAFETY: the handle is valid for the lifetime of `self`.
            unsafe {
                TerminateJobObject(self.handle, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: closing our own handle; kill-on-close ends anything left in the job.
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::sync::atomic::{AtomicI32, Ordering};

    /// The worker's process group. The command must be spawned with `process_group(0)`.
    pub struct Job {
        pgid: AtomicI32,
    }

    impl Job {
        pub fn new() -> io::Result<Self> {
            Ok(Job {
                pgid: AtomicI32::new(0),
            })
        }

        pub fn assign(&self, child: &tokio::process::Child) -> io::Result<()> {
            let pid = child
                .id()
                .ok_or_else(|| io::Error::other("the process has already exited"))?;
            self.pgid.store(pid as i32, Ordering::SeqCst);
            Ok(())
        }

        pub fn terminate(&self) {
            let pgid = self.pgid.load(Ordering::SeqCst);
            if pgid > 0 {
                // SAFETY: signalling our own child's process group.
                unsafe {
                    libc::kill(-pgid, libc::SIGKILL);
                }
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            self.terminate();
        }
    }
}

/// Make a command start in a fresh group where the platform needs it (Linux).
pub fn prepare(cmd: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: the release app has no console, and a worker must not open one.
        cmd.creation_flags(0x0800_0000);
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Killing the job ends a grandchild that the child started.
    #[tokio::test]
    async fn terminating_the_job_ends_a_grandchild() {
        let node = crate::node::resolve_node(&crate::node::NodeLookup::from_env())
            .expect("node must be installed to run the host tests");
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("grandchild.pid");
        // The child starts a long-sleeping grandchild, writes its pid, then sleeps itself.
        let script = format!(
            "const cp=require('child_process');const fs=require('fs');\
             const g=cp.spawn(process.execPath,['-e','setTimeout(()=>{{}},120000)'],{{stdio:'ignore'}});\
             fs.writeFileSync({:?}, String(g.pid));setTimeout(()=>{{}},120000);",
            pid_file.to_string_lossy()
        );
        let mut cmd = tokio::process::Command::new(&node.path);
        cmd.arg("-e").arg(script);
        prepare(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let job = Job::new().unwrap();
        job.assign(&child).unwrap();
        let mut grandchild = None;
        for _ in 0..100 {
            if let Ok(s) = std::fs::read_to_string(&pid_file) {
                if let Ok(p) = s.trim().parse::<u32>() {
                    grandchild = Some(p);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let grandchild = grandchild.expect("grandchild started");
        assert!(process_alive(grandchild), "grandchild runs before the kill");
        job.terminate();
        let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
        let mut gone = false;
        for _ in 0..50 {
            if !process_alive(grandchild) {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(gone, "the grandchild was ended with the job");
    }

    fn process_alive(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: query-only handle, closed before returning.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code);
            CloseHandle(h);
            ok != 0 && code == STILL_ACTIVE as u32
        }
    }
}
