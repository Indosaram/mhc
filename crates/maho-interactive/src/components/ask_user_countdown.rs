//! Absolute countdown driven by the host clock, as with the foundation timers.
#[derive(Debug, Clone)]
pub struct AskUserCountdown { deadline: u64, external: bool, disposed: bool }
impl AskUserCountdown {
    pub fn new(timeout_ms: u64, now_ms: u64, external_deadline: Option<u64>) -> Self {
        Self { deadline: external_deadline.unwrap_or(now_ms.saturating_add(timeout_ms)), external: external_deadline.is_some(), disposed: false }
    }
    pub fn tick(&mut self, now_ms: u64, external_deadline: Option<u64>) -> Option<(u64, bool)> {
        if self.disposed { return None; }
        let remaining = external_deadline.unwrap_or(self.deadline).saturating_sub(now_ms);
        let expired = remaining == 0 && !self.external;
        if expired { self.dispose(); }
        Some((remaining, expired))
    }
    pub fn dispose(&mut self) { self.disposed = true; }
}
