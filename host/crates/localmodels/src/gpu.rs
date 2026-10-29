//! How much graphics memory is free, for the check before a model loads.
//!
//! The real answer comes from `nvidia-smi` (the exe, never a shim). With no such program, or an
//! answer Helm cannot read, there is no figure and the check is skipped: the server then decides
//! for itself, as it would without Helm.

use std::time::Duration;

pub trait Gpu: Send + Sync {
    /// Free graphics memory in bytes, summed over the cards, or `None` when it can't be told.
    fn free_bytes(&self) -> Option<u64>;
}

/// The memory figures from `nvidia-smi`.
pub struct NvidiaSmi;

const ASK_FOR: Duration = Duration::from_secs(5);
const MIB: u64 = 1024 * 1024;

/// Free bytes from the command's `memory.total,memory.used` lines (MiB, no header, no units).
pub fn parse_free(output: &str) -> Option<u64> {
    let mut free = 0u64;
    let mut cards = 0;
    for line in output.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (total, used) = line.split_once(',')?;
        let total: u64 = total.trim().parse().ok()?;
        let used: u64 = used.trim().parse().ok()?;
        free += total.saturating_sub(used) * MIB;
        cards += 1;
    }
    (cards > 0).then_some(free)
}

impl Gpu for NvidiaSmi {
    fn free_bytes(&self) -> Option<u64> {
        let mut cmd = std::process::Command::new("nvidia-smi");
        cmd.args([
            "--query-gpu=memory.total,memory.used",
            "--format=csv,noheader,nounits",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // no console window
        }
        let mut child = cmd.spawn().ok()?;
        let mut stdout = child.stdout.take()?;
        // Read on another thread so a stuck program cannot hold the check past its time.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut out = String::new();
            let _ = std::io::Read::read_to_string(&mut stdout, &mut out);
            let _ = tx.send(out);
        });
        match rx.recv_timeout(ASK_FOR) {
            Ok(out) => {
                let _ = child.wait();
                parse_free(&out)
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_memory_is_read_from_the_csv_lines() {
        assert_eq!(parse_free("24564, 1200\n"), Some((24564 - 1200) * MIB));
        assert_eq!(parse_free("8192, 100\n8192, 8192\r\n"), Some(8092 * MIB));
        for bad in ["", "\n", "N/A, N/A", "100", "100, x"] {
            assert_eq!(parse_free(bad), None, "{bad:?}");
        }
    }
}
