use crate::kibitzer_contract::{KIBITZER_WAKE_DEADLINE_MS, KIBITZER_WAKE_MAX_TOTAL_MS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WakeBounds {
    pub started_at: i64,
    pub deadline_ms: i64,
    pub max_total_ms: i64,
}

impl WakeBounds {
    pub fn new(started_at: i64) -> Self {
        Self { started_at, deadline_ms: KIBITZER_WAKE_DEADLINE_MS, max_total_ms: KIBITZER_WAKE_MAX_TOTAL_MS }
    }

    pub fn with_limits(started_at: i64, deadline_ms: i64, max_total_ms: i64) -> Self {
        Self { started_at, deadline_ms, max_total_ms }
    }

    pub fn deadline_at(&self, now: i64) -> i64 {
        (now + self.deadline_ms).min(self.started_at + self.max_total_ms)
    }

    pub fn total_deadline_at(&self) -> i64 {
        self.started_at + self.max_total_ms
    }

    pub fn at_ceiling(&self, now: i64) -> bool {
        now >= self.total_deadline_at()
    }
}

#[cfg(test)]
#[path = "kibitzer_bounds_tests.rs"]
mod tests;
