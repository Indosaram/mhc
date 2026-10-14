//! UI-thread `setInterval`/`clearInterval`.
//!
//! senpi components arm Node intervals that call back on the event loop thread; component
//! callbacks here hold `Rc` state, so intervals live in a thread-local registry that the
//! interactive host fires from its frame tick with [`run_due_ui_intervals`].
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiIntervalId(u64);

struct UiInterval {
    id: UiIntervalId,
    period: Duration,
    next_due: Instant,
    callback: Rc<dyn Fn()>,
}

thread_local! {
    static INTERVALS: RefCell<Vec<UiInterval>> = const { RefCell::new(Vec::new()) };
    static NEXT_ID: Cell<u64> = const { Cell::new(0) };
}

/// senpi `setInterval(callback, period)`: the first call is due one period from now.
pub fn set_ui_interval(period: Duration, callback: Rc<dyn Fn()>) -> UiIntervalId {
    let id = UiIntervalId(NEXT_ID.with(|next| {
        let id = next.get();
        next.set(id.wrapping_add(1));
        id
    }));
    INTERVALS.with(|intervals| intervals.borrow_mut().push(UiInterval { id, period, next_due: Instant::now() + period, callback }));
    id
}

/// senpi `clearInterval(id)`.
pub fn clear_ui_interval(id: UiIntervalId) {
    INTERVALS.with(|intervals| intervals.borrow_mut().retain(|interval| interval.id != id));
}

/// Fires every interval due at `now` once (a late host frame does not replay missed periods) and
/// reports whether any fired. Callbacks run with the registry unborrowed, so they may set or clear
/// intervals.
pub fn run_due_ui_intervals(now: Instant) -> bool {
    let due: Vec<(UiIntervalId, Rc<dyn Fn()>)> = INTERVALS.with(|intervals| {
        intervals
            .borrow_mut()
            .iter_mut()
            .filter(|interval| interval.next_due <= now)
            .map(|interval| {
                while interval.next_due <= now {
                    interval.next_due += interval.period.max(Duration::from_millis(1));
                }
                (interval.id, Rc::clone(&interval.callback))
            })
            .collect()
    });
    let fired = !due.is_empty();
    for (id, callback) in due {
        if INTERVALS.with(|intervals| intervals.borrow().iter().any(|interval| interval.id == id)) {
            callback();
        }
    }
    fired
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_fires_once_per_due_frame_until_cleared() {
        let count = Rc::new(Cell::new(0));
        let counter = Rc::clone(&count);
        let id = set_ui_interval(Duration::from_secs(1), Rc::new(move || counter.set(counter.get() + 1)));
        let start = Instant::now();
        assert!(!run_due_ui_intervals(start));
        assert!(run_due_ui_intervals(start + Duration::from_secs(5)));
        assert_eq!(count.get(), 1);
        clear_ui_interval(id);
        assert!(!run_due_ui_intervals(start + Duration::from_secs(10)));
        assert_eq!(count.get(), 1);
    }
}
