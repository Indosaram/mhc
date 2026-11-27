//! Operator error-streak alert dispatcher (`thread/run-alerts/run-alerts.ts`).

use std::collections::HashMap;
use std::sync::Mutex;

use super::super::completion::AgentEndFacts;
use super::error_streak::{ErrorStreakDetector, ErrorStreakEvent, run_end_of};

pub struct RunAlerts {
    detector: Mutex<ErrorStreakDetector>,
    pending: Mutex<HashMap<String, ErrorStreakEvent>>,
    now: fn() -> i64,
}

impl RunAlerts {
    pub fn new(threshold: usize, realert_ms: i64, now: fn() -> i64) -> Self {
        Self {
            detector: Mutex::new(ErrorStreakDetector::new(threshold, realert_ms)),
            pending: Mutex::new(HashMap::new()),
            now,
        }
    }

    pub fn agent_end(&self, durable_id: &str, facts: &AgentEndFacts) {
        let end = run_end_of(facts);
        let now = (self.now)();
        if let Some(ev) = self
            .detector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .observe(durable_id, end, now)
        {
            self.pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(durable_id.to_owned(), ev);
        }
    }

    pub fn take_settled(&self, durable_id: &str) -> Option<ErrorStreakEvent> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(durable_id)
    }

    pub fn ended(&self, durable_id: &str) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(durable_id);
        self.detector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .forget(durable_id);
    }
}
