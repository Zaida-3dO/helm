//! One kill-on-close group per worker tree (process groups become Job Objects on Windows).
//!
//! On Windows a [`Job`] is a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: the worker is
//! assigned to it straight after spawning, every process the worker starts (the claude CLI and
//! anything claude runs) joins it automatically, and closing the last handle, or
//! [`Job::terminate`], ends the whole tree. A job made for low priority also holds every process
//! in it at below-normal priority, however it was started. On Linux the worker leads its own process group and
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
        AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicProcessIdList,
        JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
        TerminateJobObject, JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PRIORITY_CLASS,
    };
    use windows_sys::Win32::System::Threading::BELOW_NORMAL_PRIORITY_CLASS;

    /// A Job Object that kills its processes when dropped.
    pub struct Job {
        handle: HANDLE,
    }

    // A job handle may be used from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        /// `low`: also hold every process in the job at below-normal priority.
        pub fn new(low: bool) -> io::Result<Self> {
            // SAFETY: plain Win32 calls; the handle is owned by the returned value.
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let job = Job { handle };
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if low {
                    info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PRIORITY_CLASS;
                    info.BasicLimitInformation.PriorityClass = BELOW_NORMAL_PRIORITY_CLASS;
                }
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

        /// How many processes in the job are still running, not counting the console host
        /// Windows attaches to a windowless console program (it ends on its own once its
        /// program has). Read while the job is still held: once it is closed, kill-on-close has
        /// already ended them.
        pub fn active_processes(&self) -> Option<u32> {
            const MAX_IDS: usize = 512;
            // Room for the two counts ahead of the list, in whole `usize`s so the buffer has the
            // structure's alignment.
            let head = (2 * std::mem::size_of::<u32>()).div_ceil(std::mem::size_of::<usize>());
            let mut buf = vec![0usize; head + MAX_IDS];
            // SAFETY: the buffer is as long as the size passed and aligned for the structure the
            // call fills; the list is read only up to the count the call wrote.
            let ids: Vec<u32> = unsafe {
                let ok = QueryInformationJobObject(
                    self.handle,
                    JobObjectBasicProcessIdList,
                    buf.as_mut_ptr() as *mut _,
                    (buf.len() * std::mem::size_of::<usize>()) as u32,
                    std::ptr::null_mut(),
                );
                if ok == 0 {
                    return None;
                }
                let list = &*(buf.as_ptr() as *const JOBOBJECT_BASIC_PROCESS_ID_LIST);
                let n = (list.NumberOfProcessIdsInList as usize).min(MAX_IDS);
                // The list runs on past the structure's one-element array, so it is read from the
                // buffer itself rather than through that array.
                let first = buf
                    .as_ptr()
                    .cast::<u8>()
                    .add(std::mem::offset_of!(
                        JOBOBJECT_BASIC_PROCESS_ID_LIST,
                        ProcessIdList
                    ))
                    .cast::<usize>();
                std::slice::from_raw_parts(first, n)
                    .iter()
                    .map(|&id| id as u32)
                    .collect()
            };
            Some(ids.into_iter().filter(|&id| !is_console_host(id)).count() as u32)
        }
    }

    /// Whether `pid` is Windows' console host. A process that cannot be read (already gone) is
    /// not one, and counts as whatever it was.
    fn is_console_host(pid: u32) -> bool {
        use windows_sys::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: a query-only handle, closed before returning; the name buffer's length is
        // passed in and the call writes at most that many characters.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut name = [0u16; 1024];
            let mut len = name.len() as u32;
            let ok = QueryFullProcessImageNameW(h, 0, name.as_mut_ptr(), &mut len);
            CloseHandle(h);
            if ok == 0 {
                return false;
            }
            let path = String::from_utf16_lossy(&name[..len as usize]);
            path.rsplit(['\\', '/'])
                .next()
                .is_some_and(|f| f.eq_ignore_ascii_case("conhost.exe"))
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
        /// `low` is applied to the process itself ([`crate::priority::lower_after_spawn`]); a
        /// process group has no priority limit of its own.
        pub fn new(_low: bool) -> io::Result<Self> {
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

        /// Not counted on this platform.
        pub fn active_processes(&self) -> Option<u32> {
            None
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

/// Make a command start in a fresh group where the platform needs it (Linux), and below normal
/// priority on Windows when `low` is set.
pub fn prepare(cmd: &mut tokio::process::Command, low: bool) {
    #[cfg(unix)]
    {
        let _ = low;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        // No console window (the release app has none and a worker must not open one), and the
        // priority class, which every process the worker starts inherits.
        cmd.creation_flags(crate::priority::creation_flags(low));
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Killing the job ends a grandchild that the child started, and a job made for low priority
    /// holds that grandchild at below-normal priority (without it the grandchild keeps its
    /// parent's class).
    #[tokio::test]
    async fn terminating_the_job_ends_a_grandchild_and_low_priority_reaches_it() {
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, GetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS,
        };
        let node = crate::node::resolve_node(&crate::node::NodeLookup::from_env())
            .expect("node must be installed to run the host tests");
        // SAFETY: a pseudo handle for this process needs no closing.
        let own_class = unsafe { GetPriorityClass(GetCurrentProcess()) };
        assert_eq!(crate::priority::creation_flags(false), 0x0800_0000);
        assert_eq!(
            crate::priority::creation_flags(true),
            0x0800_0000 | BELOW_NORMAL_PRIORITY_CLASS
        );
        for (low, expected) in [(true, BELOW_NORMAL_PRIORITY_CLASS), (false, own_class)] {
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
            prepare(&mut cmd, low);
            let mut child = cmd.spawn().unwrap();
            let job = Job::new(low).unwrap();
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
            assert_eq!(
                job.active_processes(),
                Some(2),
                "the child and the grandchild"
            );
            assert_eq!(
                priority_class(grandchild),
                expected,
                "the grandchild's priority class with low priority {low}"
            );
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
            assert_eq!(job.active_processes(), Some(0));
        }
    }

    fn priority_class(pid: u32) -> u32 {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            GetPriorityClass, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: query-only handle, closed before returning.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            assert!(!h.is_null(), "the process can be opened");
            let class = GetPriorityClass(h);
            CloseHandle(h);
            class
        }
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
