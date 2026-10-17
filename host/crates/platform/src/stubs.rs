//! Capabilities not used yet. Each is a trait so a real Windows
//! implementation (keep-awake via `SetThreadExecutionState`, taskbar overlay badge, sounds) can be swapped in without
//! touching callers.

pub trait TaskbarBadge: Send + Sync {
    /// Show `count` on the taskbar icon; 0 clears it.
    fn set_count(&self, count: u32);
}

pub trait SoundOut: Send + Sync {
    fn play(&self, name: &str);
}

pub struct NoopBadge;
impl TaskbarBadge for NoopBadge {
    fn set_count(&self, _count: u32) {}
}

pub struct NoopSound;
impl SoundOut for NoopSound {
    fn play(&self, _name: &str) {}
}
