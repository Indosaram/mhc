//! Per-tool-call deadline tracking (`#14130`).
//!
//! Upstream counterpart: `packages/omo-senpi/src/components/call-budget/call-deadline.ts`.

use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// One thread tool call may send several commands (state probe, admission, post-state read, or paged
/// reads). Each sizes its wait to what is left of this 50 s budget so the whole call settles,
/// typed, inside the 60 s bridge limit.
pub const TOOL_CALL_BUDGET_MS: i64 = 50_000;

/// What a wait inside a call leaves for the read after it.
pub const READ_RESERVE_MS: i64 = 15_000;

tokio::task_local! {
    static CALL_DEADLINE_MS: Option<i64>;
}

fn current_epoch_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

fn active_deadline() -> Option<i64> {
    CALL_DEADLINE_MS.try_with(|v| *v).ok().flatten()
}

/// Runs `call` with every remote/host command inside it bounded by `deadline_ms` (epoch ms).
pub async fn with_call_deadline<F, T>(deadline_ms: i64, call: F) -> T
where
    F: Future<Output = T>,
{
    CALL_DEADLINE_MS.scope(Some(deadline_ms), call).await
}

/// The wait a command may take: `bound`, cut to what is left of the enclosing call's deadline
/// (none: `bound`). Uses current epoch ms when `now` is `None`.
pub fn command_wait_ms(bound: i64, now: Option<i64>) -> i64 {
    match active_deadline() {
        None => bound,
        Some(deadline) => {
            let current = now.unwrap_or_else(current_epoch_ms);
            bound.min(deadline.saturating_sub(current))
        }
    }
}

/// A wait inside a call: `requested`, cut so that `reserve` of the call's budget is left for what
/// follows (none: `requested`).
pub fn wait_within_call_budget(requested: i64, reserve: Option<i64>, now: Option<i64>) -> i64 {
    match active_deadline() {
        None => requested,
        Some(deadline) => {
            let current = now.unwrap_or_else(current_epoch_ms);
            let res = reserve.unwrap_or(READ_RESERVE_MS);
            0_i64.max(requested.min(deadline.saturating_sub(current).saturating_sub(res)))
        }
    }
}

/// Runs `work` outside any enclosing call's budget (e.g. a detached background re-send).
pub async fn outside_call_deadline<F, T>(work: F) -> T
where
    F: Future<Output = T>,
{
    CALL_DEADLINE_MS.scope(None, work).await
}

/// Runs `work` synchronously outside any enclosing call's budget.
pub fn outside_call_deadline_sync<F, T>(work: F) -> T
where
    F: FnOnce() -> T,
{
    CALL_DEADLINE_MS.sync_scope(None, work)
}

/// Awaits `work`, returning `None` once the enclosing call's budget runs out first
/// (no enclosing call: awaits `work` directly and returns `Some(result)`).
pub async fn within_call_budget<F, T>(work: F, now: Option<i64>) -> Option<T>
where
    F: Future<Output = T>,
{
    let Some(deadline) = active_deadline() else {
        return Some(work.await);
    };
    let current = now.unwrap_or_else(current_epoch_ms);
    let remaining_ms = 0_i64.max(deadline.saturating_sub(current));
    let Ok(remaining_u64) = u64::try_from(remaining_ms) else {
        return None;
    };
    tokio::time::timeout(Duration::from_millis(remaining_u64), work)
        .await
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_wait_ms_without_scope_returns_bound() {
        assert_eq!(command_wait_ms(30_000, Some(1_000)), 30_000);
        assert_eq!(wait_within_call_budget(25_000, None, Some(1_000)), 25_000);
    }

    #[tokio::test]
    async fn scoped_deadline_bounds_command_and_budget_waits() {
        let now = 100_000;
        let deadline = now + TOOL_CALL_BUDGET_MS;
        with_call_deadline(deadline, async {
            // 50_000 remaining, bound 60_000 -> 50_000
            assert_eq!(command_wait_ms(60_000, Some(now)), 50_000);
            // 50_000 - 15_000 reserve = 35_000 -> requested 40_000 clamped to 35_000
            assert_eq!(wait_within_call_budget(40_000, None, Some(now)), 35_000);
            // Near end of call: 10_000 left, reserve 15_000 -> 0
            assert_eq!(
                wait_within_call_budget(20_000, None, Some(deadline - 10_000)),
                0
            );
            // Past deadline: command_wait_ms goes negative just like JS Math.min(bound, deadline - now)
            assert_eq!(command_wait_ms(10_000, Some(deadline + 500)), -500);
            // Exiting scope drops the active deadline
            outside_call_deadline(async {
                assert_eq!(command_wait_ms(30_000, Some(deadline)), 30_000);
            })
            .await;
            assert_eq!(
                outside_call_deadline_sync(|| command_wait_ms(30_000, Some(deadline))),
                30_000
            );
        })
        .await;
    }

    #[tokio::test]
    async fn within_call_budget_returns_none_when_expired() {
        let now = current_epoch_ms();
        let res = with_call_deadline(now, async {
            within_call_budget(std::future::pending::<i32>(), Some(now)).await
        })
        .await;
        assert_eq!(res, None);

        let ok = with_call_deadline(now + 5_000, async {
            within_call_budget(async { 42 }, Some(now)).await
        })
        .await;
        assert_eq!(ok, Some(42));
    }
}
