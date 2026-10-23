//! Port of senpi packages/coding-agent/src/core/extensions/shutdown-quit-budget.ts.

use std::time::{Duration, Instant};

/// One handler's `(warn, timeout)` limits in milliseconds; 0 disables that limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandlerBudget {
    pub warn_ms: u64,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuitHandlerBudget {
    pub budget: HandlerBudget,
    pub ends_at_quit_deadline: bool,
}

/// One deadline shared by every `session_shutdown` handler of a quit that ends in process exit (senpi#3711),
/// counted from the moment the shutdown emission is armed. A handler that starts after the deadline gets 1 ms.
pub struct QuitDeadline<N: Fn() -> Instant> {
    deadline: Instant,
    now: N,
}

impl<N: Fn() -> Instant> QuitDeadline<N> {
    pub fn new(total_ms: u64, now: N, started_at: Instant) -> Self {
        Self {
            deadline: started_at + Duration::from_millis(total_ms),
            now,
        }
    }

    pub fn budget_for(&self, base: HandlerBudget) -> QuitHandlerBudget {
        let remaining = u64::try_from(
            self.deadline
                .saturating_duration_since((self.now)())
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
        .max(1);
        if base.timeout_ms > 0 && base.timeout_ms <= remaining {
            return QuitHandlerBudget {
                budget: base,
                ends_at_quit_deadline: false,
            };
        }
        let warn_ms = if base.warn_ms < remaining {
            base.warn_ms
        } else {
            0
        };
        QuitHandlerBudget {
            budget: HandlerBudget {
                warn_ms,
                timeout_ms: remaining,
            },
            ends_at_quit_deadline: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn clock(start: Instant) -> (Arc<Mutex<Instant>>, impl Fn() -> Instant) {
        let cell = Arc::new(Mutex::new(start));
        let reader = Arc::clone(&cell);
        (cell, move || reader.lock().map(|now| *now).unwrap_or(start))
    }

    #[test]
    fn keeps_base_when_cap_fits_and_clamps_otherwise() {
        let start = Instant::now();
        let (cell, now) = clock(start);
        let deadline = QuitDeadline::new(2_000, now, start);
        let base = HandlerBudget {
            warn_ms: 500,
            timeout_ms: 1_000,
        };
        assert_eq!(
            deadline.budget_for(base),
            QuitHandlerBudget {
                budget: base,
                ends_at_quit_deadline: false
            }
        );
        if let Ok(mut now) = cell.lock() {
            *now = start + Duration::from_millis(1_500);
        }
        let clamped = deadline.budget_for(base);
        assert_eq!(
            clamped.budget,
            HandlerBudget {
                warn_ms: 0,
                timeout_ms: 500
            }
        );
        assert!(clamped.ends_at_quit_deadline);
        let unbounded = deadline.budget_for(HandlerBudget {
            warn_ms: 100,
            timeout_ms: 0,
        });
        assert_eq!(
            unbounded.budget,
            HandlerBudget {
                warn_ms: 100,
                timeout_ms: 500
            }
        );
    }

    #[test]
    fn late_handlers_get_one_millisecond() {
        let start = Instant::now();
        let (cell, now) = clock(start);
        let deadline = QuitDeadline::new(10, now, start);
        if let Ok(mut now) = cell.lock() {
            *now = start + Duration::from_millis(50);
        }
        assert_eq!(
            deadline
                .budget_for(HandlerBudget {
                    warn_ms: 5,
                    timeout_ms: 0
                })
                .budget,
            HandlerBudget {
                warn_ms: 0,
                timeout_ms: 1
            }
        );
    }
}
