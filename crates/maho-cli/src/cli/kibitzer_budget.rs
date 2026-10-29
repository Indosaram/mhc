//! The CLI's concrete per-wake budget slot. It implements the PRODUCER-owned
//! `KibitzerToolBudget`/`KibitzerBudgetSlot` traits, so the memory crate never names a CLI type.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use maho_omo_memory::kibitzer_contract::{KibitzerBudgetSlot, KibitzerToolBudget};

/// One wake's budget. `charge` is a bounded atomic increment: no interleaving can admit more than
/// `limit` calls, a zero limit refuses every call, and the counter never advances past `limit` (so
/// it can only reach `limit`, never wrap).
pub struct CliWakeBudget {
    limit: usize,
    used: AtomicUsize,
}

impl CliWakeBudget {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self { limit, used: AtomicUsize::new(0) })
    }

    /// Seeded constructor for the boundary regression: the counter starts at `used`.
    #[cfg(test)]
    fn seeded(limit: usize, used: usize) -> Arc<Self> {
        Arc::new(Self { limit, used: AtomicUsize::new(used) })
    }
}

impl KibitzerToolBudget for CliWakeBudget {
    fn limit(&self) -> usize {
        self.limit
    }

    fn used(&self) -> usize {
        self.used.load(Ordering::SeqCst)
    }

    fn charge(&self) -> bool {
        // Bounded increment: retry only while `current < limit`. `then(|| current + 1)` is LAZY, so
        // the increment is never evaluated when the call is refused - no overflow at `usize::MAX`.
        self.used
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                (current < self.limit).then(|| current + 1)
            })
            .is_ok()
    }
}

/// The retained slot the tools read and the sidecar resets per wake.
#[derive(Clone)]
pub struct CliBudgetSlot {
    inner: Arc<Mutex<Arc<CliWakeBudget>>>,
}

impl CliBudgetSlot {
    pub fn new(limit: usize) -> Self {
        Self { inner: Arc::new(Mutex::new(CliWakeBudget::new(limit))) }
    }
}

impl KibitzerBudgetSlot for CliBudgetSlot {
    fn current(&self) -> Arc<dyn KibitzerToolBudget> {
        // Clone the INNER `Arc<CliWakeBudget>` (dereference the guard), then coerce to the trait
        // object; never clone the guard itself.
        let budget: Arc<CliWakeBudget> = Arc::clone(&*self.inner.lock().unwrap_or_else(PoisonError::into_inner));
        budget as Arc<dyn KibitzerToolBudget>
    }

    /// Swap in a fresh budget for the wake about to run; called by the memory sidecar per wake.
    fn reset(&self, limit: usize) {
        *self.inner.lock().unwrap_or_else(PoisonError::into_inner) = CliWakeBudget::new(limit);
    }
}

#[cfg(test)]
mod tests {
    use super::{CliBudgetSlot, CliWakeBudget};
    use maho_omo_memory::kibitzer_contract::{KibitzerBudgetSlot, KibitzerToolBudget};
    use std::sync::{Arc, Barrier};

    /// Exactly `limit` charges are admitted under simultaneous contenders; the rest are refused, and
    /// the counter never exceeds the limit. Barrier-synchronized: no sleeps, fully deterministic.
    #[test]
    fn simultaneous_charges_admit_exactly_the_limit() {
        const LIMIT: usize = 7;
        const CONTENDERS: usize = 64;
        let budget = CliWakeBudget::new(LIMIT);
        let barrier = Arc::new(Barrier::new(CONTENDERS));
        let mut handles = Vec::with_capacity(CONTENDERS);
        for _ in 0..CONTENDERS {
            let budget = Arc::clone(&budget);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                budget.charge()
            }));
        }
        let admitted = handles
            .into_iter()
            .map(|handle| handle.join().expect("charge thread"))
            .filter(|admitted| *admitted)
            .count();
        assert_eq!(admitted, LIMIT, "exactly the limit is admitted");
        assert_eq!(budget.used(), LIMIT, "the counter stops at the limit");
    }

    /// A zero limit refuses every charge (no admission, no increment).
    #[test]
    fn a_zero_limit_refuses_every_charge() {
        let budget = CliWakeBudget::new(0);
        assert!(!budget.charge());
        assert_eq!(budget.used(), 0);
    }

    /// Boundary: a counter seeded at `usize::MAX` with `limit = usize::MAX` refuses without wrapping.
    #[test]
    fn a_max_counter_at_the_limit_refuses_without_overflow() {
        let budget = CliWakeBudget::seeded(usize::MAX, usize::MAX);
        assert!(!budget.charge(), "a full counter refuses");
        assert_eq!(budget.used(), usize::MAX, "the counter never wraps");
    }

    /// `reset` swaps in a fresh budget the tools observe through `current()`.
    #[test]
    fn reset_swaps_the_current_budget() {
        let slot = CliBudgetSlot::new(2);
        assert!(slot.current().charge());
        assert!(slot.current().charge());
        assert!(!slot.current().charge(), "the seeded limit is enforced");
        slot.reset(5);
        assert_eq!(slot.current().limit(), 5);
        assert_eq!(slot.current().used(), 0, "the reset budget starts fresh");
    }
}
