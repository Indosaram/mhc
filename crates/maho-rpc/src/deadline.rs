//! One bounded wait for a condition: poll it at `interval` until it holds or the deadline passes, and answer
//! its final state. Callers that hand-rolled this loop differed only in the condition and the poll interval.

/// The poll interval every bounded host-lifecycle wait uses. Kept beside the wait itself so platform
/// tuning (Windows named pipes are slower to answer) has one place to change.
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// The grace a supervisor gets between SIGKILL and the moment its generation must be gone. Shared by
/// `host-ensure-stop` and `host-session` so the two never drift apart.
pub const SIGKILL_GRACE_MS: u64 = 2_000;

/// Polls `condition` once, then every `interval` while the deadline is still ahead, and answers the condition's
/// final state. No time passes when the condition already holds, no sleep starts at or after the deadline, and the
/// condition is read once more once the wait is over - the "look again after the loop" every caller already had.
pub async fn wait_until<F>(
    deadline: tokio::time::Instant,
    interval: std::time::Duration,
    mut condition: F,
) -> bool
where
    F: FnMut() -> bool,
{
    if condition() {
        return true;
    }
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(interval).await;
        if condition() {
            return true;
        }
    }
    // The wait is over: this read is the condition's final state.
    condition()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn a_condition_that_already_holds_returns_without_any_wait() {
        let start = tokio::time::Instant::now();
        let calls = Cell::new(0);
        let held = wait_until(
            start + Duration::from_secs(10),
            Duration::from_millis(50),
            || {
                calls.set(calls.get() + 1);
                true
            },
        )
        .await;
        assert!(held);
        assert_eq!(calls.get(), 1);
        assert_eq!(start.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn a_condition_that_holds_before_the_deadline_ends_the_wait_there() {
        let start = tokio::time::Instant::now();
        let calls = Cell::new(0);
        let held = wait_until(
            start + Duration::from_secs(10),
            Duration::from_millis(50),
            || {
                let call = calls.get() + 1;
                calls.set(call);
                call > 2
            },
        )
        .await;
        assert!(held);
        assert_eq!(calls.get(), 3);
        assert_eq!(start.elapsed(), Duration::from_millis(100));
    }

    #[tokio::test(start_paused = true)]
    async fn a_condition_that_never_holds_returns_its_state_at_the_deadline() {
        let start = tokio::time::Instant::now();
        let calls = Cell::new(0);
        let held = wait_until(
            start + Duration::from_millis(300),
            Duration::from_millis(50),
            || {
                calls.set(calls.get() + 1);
                false
            },
        )
        .await;
        assert!(!held);
        // Polls at 0, 50, ..., 250ms, one after the 300ms sleep, then the closing read.
        assert_eq!(calls.get(), 8);
        assert_eq!(start.elapsed(), Duration::from_millis(300));
    }

    #[tokio::test(start_paused = true)]
    async fn a_condition_that_holds_only_at_the_deadline_is_still_observed() {
        let start = tokio::time::Instant::now();
        let calls = Cell::new(0);
        let held = wait_until(
            start + Duration::from_millis(300),
            Duration::from_millis(50),
            || {
                let call = calls.get() + 1;
                calls.set(call);
                call > 7
            },
        )
        .await;
        assert!(held);
        assert_eq!(calls.get(), 8);
        assert_eq!(start.elapsed(), Duration::from_millis(300));
    }
}
