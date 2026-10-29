//! Per-wake tool-call budget (latest `kibitzer/tools/budget.ts`).
//!
//! Every closure of one wake shares ONE instance. `charge` admits with a compare-exchange, so
//! concurrent native calls can never exceed the limit (upstream JS `charge` was synchronous).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::kibitzer_contract::KibitzerToolBudget;

pub struct WakeToolBudget {
    limit: usize,
    used: AtomicUsize,
}

impl WakeToolBudget {
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self { limit, used: AtomicUsize::new(0) })
    }
}

impl KibitzerToolBudget for WakeToolBudget {
    fn limit(&self) -> usize {
        self.limit
    }
    fn used(&self) -> usize {
        self.used.load(Ordering::SeqCst)
    }
    fn charge(&self) -> bool {
        self.used
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                if used < self.limit {
                    Some(used + 1)
                } else {
                    None
                }
            })
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    /// Builds a budget whose counter already sits at `limit`, so every charge must refuse.
    /// The struct fields are private, but this inline module is a child of the owning module.
    fn exhausted(limit: usize) -> WakeToolBudget {
        WakeToolBudget { limit, used: AtomicUsize::new(limit) }
    }

    #[test]
    fn given_a_zero_budget_when_charged_then_it_refuses_and_never_counts_a_call() {
        let budget = WakeToolBudget::new(0);
        assert_eq!(budget.limit(), 0);
        assert_eq!(budget.used(), 0);
        assert!(!budget.charge());
        assert!(!budget.charge());
        assert_eq!(budget.used(), 0, "a refused call must not increment the counter");
    }

    #[test]
    fn given_the_counter_at_usize_max_when_charged_then_it_refuses_without_overflow() {
        let budget = exhausted(usize::MAX);
        assert_eq!(budget.limit(), usize::MAX);
        assert_eq!(budget.used(), usize::MAX);
        assert!(!budget.charge(), "used == limit refuses the call");
        assert!(!budget.charge());
        assert_eq!(budget.used(), usize::MAX, "a refused charge must not wrap the counter");
    }

    #[test]
    fn given_more_contenders_than_the_limit_when_they_race_then_exactly_the_limit_is_admitted() {
        const LIMIT: usize = 8;
        const THREADS: usize = 32;

        let budget = WakeToolBudget::new(LIMIT);
        let barrier = Arc::new(Barrier::new(THREADS));
        let admitted = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::with_capacity(THREADS);
        for _ in 0..THREADS {
            let budget = Arc::clone(&budget);
            let barrier = Arc::clone(&barrier);
            let admitted = Arc::clone(&admitted);
            handles.push(std::thread::spawn(move || {
                // Charge only after every thread is ready, so the compare-exchange in `charge`
                // runs under real contention. No sleeps and no polling: the barrier is the signal.
                barrier.wait();
                if budget.charge() {
                    admitted.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }

        for handle in handles {
            handle.join().expect("a charging thread must not panic");
        }

        assert_eq!(admitted.load(Ordering::SeqCst), LIMIT, "exactly the limit is admitted");
        assert_eq!(budget.used(), LIMIT, "the counter settles exactly at the limit");
        assert!(!budget.charge(), "the budget is exhausted once the limit is reached");
    }
}
