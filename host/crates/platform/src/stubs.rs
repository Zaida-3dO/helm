//! Capabilities not used yet. Each is a trait so a real Windows
//! implementation (keep-awake via `SetThreadExecutionState`, taskbar overlay badge, sounds) can be swapped in without
//! touching callers.

pub trait PowerAssertions: Send + Sync {
    /// Keep the machine awake while `on` is true.
    fn set_keep_awake(&self, on: bool);
}

pub trait TaskbarBadge: Send + Sync {
    /// Show `count` on the taskbar icon; 0 clears it.
    fn set_count(&self, count: u32);
}

pub trait SoundOut: Send + Sync {
    fn play(&self, name: &str);
}

pub struct NoopPower;
impl PowerAssertions for NoopPower {
    fn set_keep_awake(&self, _on: bool) {}
}

pub struct NoopBadge;
impl TaskbarBadge for NoopBadge {
    fn set_count(&self, _count: u32) {}
}

pub struct NoopSound;
impl SoundOut for NoopSound {
    fn play(&self, _name: &str) {}
}
