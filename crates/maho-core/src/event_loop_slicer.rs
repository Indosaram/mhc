//! Port of senpi `packages/coding-agent/src/core/event-loop-slicer.ts`.
//!
//! Cooperative time slicing for work that walks a whole transcript on a shared runtime. Work checks
//! `due` between items and awaits `yield_now()` once its slice is spent, so sockets, timers and other
//! sessions run between slices (senpi#3518).

use std::sync::Arc;
use std::time::Instant;

pub const DEFAULT_SLICE_BUDGET_MS: f64 = 8.0;

pub type SliceClock = Arc<dyn Fn() -> f64 + Send + Sync>;

#[derive(Default, Clone)]
pub struct EventLoopSlicerOptions {
    pub budget_ms: Option<f64>,
    /// Milliseconds; a clock that advances per call makes slicing deterministic in tests.
    pub clock: Option<SliceClock>,
}

pub struct EventLoopSlicer {
    budget_ms: f64,
    clock: SliceClock,
    slice_start: f64,
}

impl Default for EventLoopSlicer {
    fn default() -> Self {
        Self::new(EventLoopSlicerOptions::default())
    }
}

impl EventLoopSlicer {
    pub fn new(options: EventLoopSlicerOptions) -> Self {
        let clock = options.clock.unwrap_or_else(|| {
            let origin = Instant::now();
            Arc::new(move || origin.elapsed().as_secs_f64() * 1000.0)
        });
        let slice_start = clock();
        Self {
            budget_ms: options.budget_ms.unwrap_or(DEFAULT_SLICE_BUDGET_MS),
            clock,
            slice_start,
        }
    }

    pub fn due(&self) -> bool {
        (self.clock)() - self.slice_start >= self.budget_ms
    }

    /// A real scheduler yield rather than a no-op: only giving the runtime a turn lets pending I/O run.
    pub async fn yield_now(&mut self) {
        tokio::task::yield_now().await;
        self.slice_start = (self.clock)();
    }
}

pub async fn map_in_slices<T, R>(
    items: &[T],
    mut transform: impl FnMut(&T) -> R,
    slicer: &mut EventLoopSlicer,
) -> Vec<R> {
    let mut mapped = Vec::with_capacity(items.len());
    for item in items {
        mapped.push(transform(item));
        if slicer.due() {
            slicer.yield_now().await;
        }
    }
    mapped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn stepping_clock(step: u64) -> (SliceClock, Arc<AtomicU64>) {
        let ticks = Arc::new(AtomicU64::new(0));
        let shared = ticks.clone();
        let clock: SliceClock = Arc::new(move || shared.fetch_add(step, Ordering::SeqCst) as f64);
        (clock, ticks)
    }

    #[tokio::test]
    async fn yields_once_each_slice_is_spent_and_keeps_order() {
        let (clock, ticks) = stepping_clock(5);
        let mut slicer = EventLoopSlicer::new(EventLoopSlicerOptions {
            budget_ms: Some(8.0),
            clock: Some(clock),
        });
        let mapped = map_in_slices(&[1, 2, 3, 4], |value| value * 10, &mut slicer).await;
        assert_eq!(mapped, vec![10, 20, 30, 40]);
        // one read at start, one `due` per item, one restart per yield (items 2 and 4): 7 reads of 5.
        assert_eq!(ticks.load(Ordering::SeqCst), 35);
    }

    #[test]
    fn a_fresh_slice_is_not_due() {
        let (clock, _) = stepping_clock(1);
        let slicer = EventLoopSlicer::new(EventLoopSlicerOptions {
            budget_ms: Some(8.0),
            clock: Some(clock),
        });
        assert!(!slicer.due());
    }
}
