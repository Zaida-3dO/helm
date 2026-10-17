//! Keep-awake: one power request that keeps the
//! machine out of *idle system sleep* only. The display may still sleep and the screen may lock.
//!
//! Windows: `PowerCreateRequest` once (with a reason string Windows lists in `powercfg
//! /requests`), then `PowerSetRequest` / `PowerClearRequest(PowerRequestSystemRequired)`. The
//! request handle is not tied to a thread, unlike `SetThreadExecutionState`.

use std::sync::Mutex;

pub trait PowerAssertions: Send + Sync {
    /// Hold (true) or release (false) the keep-awake request. Idempotent.
    fn set_keep_awake(&self, on: bool) -> Result<(), String>;
}

pub struct NoopPower;
impl PowerAssertions for NoopPower {
    fn set_keep_awake(&self, _on: bool) -> Result<(), String> {
        Ok(())
    }
}

/// The reason string shown in the system's power-request list.
pub const REASON: &str = "Helm: running tasks";

#[derive(Default)]
pub struct NativePower {
    state: Mutex<PowerState>,
}

#[derive(Default)]
struct PowerState {
    #[cfg(windows)]
    handle: Option<isize>,
    on: bool,
}

impl NativePower {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the request is currently held.
    pub fn is_on(&self) -> bool {
        self.state.lock().unwrap().on
    }
}

#[cfg(windows)]
impl PowerAssertions for NativePower {
    fn set_keep_awake(&self, on: bool) -> Result<(), String> {
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::System::Power::{
            PowerClearRequest, PowerCreateRequest, PowerRequestSystemRequired, PowerSetRequest,
        };
        use windows_sys::Win32::System::SystemServices::POWER_REQUEST_CONTEXT_VERSION;
        use windows_sys::Win32::System::Threading::{
            POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0,
        };
        let mut st = self.state.lock().unwrap();
        if st.on == on {
            return Ok(());
        }
        if st.handle.is_none() {
            let mut reason = crate::fs::wide(REASON);
            let ctx = REASON_CONTEXT {
                Version: POWER_REQUEST_CONTEXT_VERSION,
                Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
                Reason: REASON_CONTEXT_0 {
                    SimpleReasonString: reason.as_mut_ptr(),
                },
            };
            // SAFETY: ctx and its string live across the call; the returned handle is owned here.
            let h = unsafe { PowerCreateRequest(&ctx) };
            if h.is_null() || h as isize == -1 {
                return Err(format!(
                    "PowerCreateRequest failed: {}",
                    std::io::Error::last_os_error()
                ));
            }
            st.handle = Some(h as isize);
        }
        let h = st.handle.unwrap_or_default() as HANDLE;
        // SAFETY: h is the live request handle created above.
        let ok = unsafe {
            if on {
                PowerSetRequest(h, PowerRequestSystemRequired)
            } else {
                PowerClearRequest(h, PowerRequestSystemRequired)
            }
        };
        if ok == 0 {
            return Err(format!(
                "power request failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        st.on = on;
        Ok(())
    }
}

#[cfg(not(windows))]
impl PowerAssertions for NativePower {
    fn set_keep_awake(&self, on: bool) -> Result<(), String> {
        // Linux: no idle inhibitor yet (Linux gets cheap stubs). Recorded, not an error.
        self.state.lock().unwrap().on = on;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_awake_sets_and_clears() {
        let p = NativePower::new();
        p.set_keep_awake(true).unwrap();
        assert!(p.is_on());
        p.set_keep_awake(true).unwrap();
        p.set_keep_awake(false).unwrap();
        assert!(!p.is_on());
    }
}
