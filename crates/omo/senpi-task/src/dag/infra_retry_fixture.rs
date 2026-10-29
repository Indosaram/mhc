//! `dag/infra-retry-fixture.ts`: the controllable retry clock and node helpers for infrastructure
//! retry tests. The end-to-end `scheduler-infra-retry.test.ts` cases are not mirrored here because
//! the Rust runner layer cannot yet produce a typed `child-unresponsive`/`host_unavailable` task
//! failure (no `RunnerFailureKind` variant and the outcome tracker does not persist `failure_kind`).

#![allow(dead_code)] // consumed by the end-to-end infra-retry cases once typed failures are producible

use std::sync::{Arc, Mutex, PoisonError};

use crate::dag::graph::DagNodeInput;
use crate::dag::infrastructure_retry::{RetryCancel, RetrySchedule};
use crate::dag::types::DagNodeTarget;

/// `dagNode(id, dependsOn)`: a `quick` category node whose prompt is its id.
pub(crate) fn dag_node(id: &str, depends_on: &[&str]) -> DagNodeInput {
    DagNodeInput {
        id: id.to_string(),
        prompt: id.to_string(),
        target: DagNodeTarget::Category("quick".to_string()),
        label: None,
        depends_on: (!depends_on.is_empty())
            .then(|| depends_on.iter().map(|d| (*d).to_string()).collect()),
        task_summary: None,
        description: None,
        load_skills: None,
    }
}

struct Timer {
    callback: Option<Box<dyn FnOnce() + Send>>,
    at: i64,
    cancelled: bool,
}

#[derive(Default)]
struct ClockState {
    delays: Vec<u64>,
    timers: Vec<Arc<Mutex<Timer>>>,
    time: i64,
}

/// Deterministic backoff clock: timers fire only on `advance`.
pub(crate) struct RetryClock {
    state: Arc<Mutex<ClockState>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl RetryClock {
    /// Starts at 2026-10-19T00:00:00Z.
    pub(crate) fn new() -> Self {
        let start = chrono::DateTime::parse_from_rfc3339("2026-10-19T00:00:00.000Z")
            .map_or(0, |parsed| parsed.timestamp_millis());
        Self {
            state: Arc::new(Mutex::new(ClockState {
                time: start,
                ..ClockState::default()
            })),
        }
    }

    pub(crate) fn delays(&self) -> Vec<u64> {
        lock(&self.state).delays.clone()
    }

    pub(crate) fn now(&self) -> Arc<dyn Fn() -> i64 + Send + Sync> {
        let state = Arc::clone(&self.state);
        Arc::new(move || lock(&state).time)
    }

    pub(crate) fn schedule(&self) -> RetrySchedule {
        let state = Arc::clone(&self.state);
        Arc::new(
            move |callback: Box<dyn FnOnce() + Send>, delay_ms: u64| -> RetryCancel {
                let mut clock = lock(&state);
                let at = clock.time + i64::try_from(delay_ms).unwrap_or(i64::MAX);
                let timer = Arc::new(Mutex::new(Timer {
                    callback: Some(callback),
                    at,
                    cancelled: false,
                }));
                clock.delays.push(delay_ms);
                clock.timers.push(Arc::clone(&timer));
                Box::new(move || lock(&timer).cancelled = true)
            },
        )
    }

    /// Fires the oldest scheduled timer (unless cancelled); `false` when none is scheduled.
    pub(crate) fn advance(&self) -> bool {
        let timer = {
            let mut clock = lock(&self.state);
            if clock.timers.is_empty() {
                return false;
            }
            let timer = clock.timers.remove(0);
            clock.time = lock(&timer).at;
            timer
        };
        let callback = {
            let mut guard = lock(&timer);
            if guard.cancelled {
                None
            } else {
                guard.callback.take()
            }
        };
        if let Some(callback) = callback {
            callback();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn advance_fires_timers_in_order_and_skips_cancelled_ones() {
        let clock = RetryClock::new();
        let schedule = clock.schedule();
        let fired = Arc::new(AtomicUsize::new(0));
        let first = Arc::clone(&fired);
        let _keep = schedule(
            Box::new(move || {
                first.fetch_add(1, Ordering::SeqCst);
            }),
            1000,
        );
        let second = Arc::clone(&fired);
        let cancel = schedule(
            Box::new(move || {
                second.fetch_add(10, Ordering::SeqCst);
            }),
            2000,
        );
        cancel();
        let start = (clock.now())();
        assert!(clock.advance());
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert_eq!((clock.now())() - start, 1000);
        assert!(clock.advance());
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert!(!clock.advance());
        assert_eq!(clock.delays(), vec![1000, 2000]);
    }

    #[test]
    fn dag_node_builds_a_quick_category_node() {
        let node = dag_node("child", &["root"]);
        assert_eq!(node.prompt, "child");
        assert_eq!(node.depends_on, Some(vec!["root".to_string()]));
    }
}
