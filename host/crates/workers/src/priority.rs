//! Running agents below normal priority, so a game or any other foreground program on the same PC
//! stays responsive while chats work.
//!
//! On Windows the priority class is set when a process is created ([`creation_flags`]) and
//! children inherit it; the worker's job also caps the class for everything inside it
//! (`job::Job::new`). On Linux the process is niced right after it starts
//! ([`lower_after_spawn`]) and children inherit that. Neither needs elevated rights: below normal
//! is always allowed.

/// `CREATE_NO_WINDOW`: the release app has no console and a helper or worker must not open one.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// `BELOW_NORMAL_PRIORITY_CLASS`.
#[cfg(windows)]
const BELOW_NORMAL: u32 = 0x0000_4000;

/// The nice value a lowered process gets on Linux.
#[cfg(unix)]
const NICE: i32 = 10;

/// The Windows process-creation flags for a worker or helper: no console window, and below-normal
/// priority when `low` is set. Always 0 away from Windows.
pub fn creation_flags(low: bool) -> u32 {
    #[cfg(windows)]
    {
        CREATE_NO_WINDOW | if low { BELOW_NORMAL } else { 0 }
    }
    #[cfg(not(windows))]
    {
        let _ = low;
        0
    }
}

/// Lower a process that has just started (Linux). Every thread of the process is niced, since the
/// kernel keeps one value per thread and some already run; threads and children made later
/// inherit it. A failure is logged and otherwise ignored: the agent still runs, at normal
/// priority. Does nothing on Windows (the creation flag did it) or when `low` is off.
pub fn lower_after_spawn(pid: Option<u32>, low: bool) {
    #[cfg(unix)]
    if let (true, Some(pid)) = (low, pid) {
        let mut ids = std::fs::read_dir(format!("/proc/{pid}/task"))
            .map(|d| {
                d.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if ids.is_empty() {
            ids.push(pid);
        }
        for id in ids {
            // SAFETY: plain syscall on a thread id of a process this host just started.
            let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS as _, id as _, NICE) };
            if rc != 0 {
                log::warn!(
                    "could not lower the priority of process {pid}: {}",
                    std::io::Error::last_os_error()
                );
                break;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (pid, low);
}
