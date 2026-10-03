use maho_codemode::timeouts::bridge_timeout::{TIMEOUT_PAUSE_OP, TIMEOUT_RESUME_OP, with_bridge_timeout_pause};
use maho_codemode::timeouts::idle_timeout::{DEFAULT_MAX_PAUSE_GRACE_MS, IdleTimeout, IdleTimeoutOptions, TimeoutPauseHandle};
use maho_codemode::timeouts::run_budget::RunBudget;
use std::time::Duration;
use tokio::time::{Instant, advance};

fn idle(timeout_ms: u64, grace: Option<u64>, deadline: Option<Instant>) -> IdleTimeout {
    IdleTimeout::new(IdleTimeoutOptions { cell_id: "cell-1".into(), timeout_ms, max_pause_grace_ms: grace, deadline })
}

async fn tick(ms: u64) {
    advance(Duration::from_millis(ms)).await;
    tokio::task::yield_now().await;
}

#[test]
fn canonical_operations() {
    assert_eq!(TIMEOUT_PAUSE_OP, "timeout-pause");
    assert_eq!(TIMEOUT_RESUME_OP, "timeout-resume");
}

#[tokio::test(start_paused = true)]
async fn idle_expires_once() {
    let watchdog = idle(1_000, None, None);
    let mut signal = watchdog.signal();
    tick(999).await;
    assert!(signal.borrow().is_none());
    tick(1).await;
    signal.changed().await.unwrap();
    assert_eq!(signal.borrow().as_ref().unwrap().cell_id, "cell-1");
    assert_eq!(signal.borrow().as_ref().unwrap().error, "Cell timed out after 1000ms");
    tick(5_000).await;
    assert!(!signal.has_changed().unwrap_or(false));
}

#[tokio::test(start_paused = true)]
async fn bridge_pause_survives_idle_window() {
    let watchdog = idle(1_000, None, None);
    let result = with_bridge_timeout_pause(Some(&watchdog), async { tick(5_000).await; "result" }).await;
    assert_eq!(result, "result");
    assert!(watchdog.signal().borrow().is_none());
}

#[tokio::test(start_paused = true)]
async fn pause_grace_expires() {
    let watchdog = idle(1_000, Some(10_000), None);
    let mut signal = watchdog.signal();
    watchdog.pause();
    tick(9_999).await;
    assert!(signal.borrow().is_none());
    tick(1).await;
    signal.changed().await.unwrap();
    assert_eq!(signal.borrow().as_ref().unwrap().error, "Cell timed out after 10000ms waiting on a host tool call");
}

#[tokio::test(start_paused = true)]
async fn grace_fires_once_and_stays_settled() {
    let watchdog = idle(1_000, Some(5_000), None);
    let mut signal = watchdog.signal();
    watchdog.pause();
    tick(60_000).await;
    signal.changed().await.unwrap();
    watchdog.resume();
    tick(60_000).await;
    assert!(!signal.has_changed().unwrap_or(false));
}

#[tokio::test(start_paused = true)]
async fn five_minute_bridge_fits_grace() {
    let watchdog = idle(1_000, Some(600_000), None);
    let value = with_bridge_timeout_pause(Some(&watchdog), async { tick(300_000).await; "built" }).await;
    assert_eq!(value, "built");
    assert!(watchdog.signal().borrow().is_none());
}

#[tokio::test(start_paused = true)]
async fn resume_before_grace_restores_full_window() {
    let watchdog = idle(1_000, Some(10_000), None);
    let mut signal = watchdog.signal();
    with_bridge_timeout_pause(Some(&watchdog), tick(9_000)).await;
    tick(999).await;
    assert!(signal.borrow().is_none());
    tick(1).await;
    signal.changed().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn sequential_calls_get_fresh_grace() {
    let watchdog = idle(1_000, Some(10_000), None);
    for _ in 0..3 { with_bridge_timeout_pause(Some(&watchdog), tick(9_000)).await; }
    assert!(watchdog.signal().borrow().is_none());
}

#[tokio::test(start_paused = true)]
async fn nested_pause_does_not_extend_grace() {
    let watchdog = idle(1_000, Some(10_000), None);
    let mut signal = watchdog.signal();
    watchdog.pause();
    tick(5_000).await;
    watchdog.pause();
    tick(4_999).await;
    assert!(signal.borrow().is_none());
    tick(1).await;
    signal.changed().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn grace_cannot_shorten_explicit_timeout() {
    let watchdog = idle(120_000, Some(10_000), None);
    let mut signal = watchdog.signal();
    watchdog.pause();
    tick(119_999).await;
    assert!(signal.borrow().is_none());
    tick(1).await;
    signal.changed().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn default_pause_grace() {
    let watchdog = idle(1_000, None, None);
    let mut signal = watchdog.signal();
    watchdog.pause();
    tick(DEFAULT_MAX_PAUSE_GRACE_MS - 1).await;
    assert!(signal.borrow().is_none());
    tick(1).await;
    signal.changed().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn disposal_during_pause() {
    let watchdog = idle(1_000, Some(10_000), None);
    watchdog.pause();
    watchdog.dispose();
    tick(60_000).await;
    assert!(watchdog.signal().borrow().is_none());
}

#[tokio::test(start_paused = true)]
async fn bridge_release_restarts_window() {
    let watchdog = idle(1_000, None, None);
    let mut signal = watchdog.signal();
    tick(400).await;
    with_bridge_timeout_pause(Some(&watchdog), tick(10_000)).await;
    tick(999).await;
    assert!(signal.borrow().is_none());
    tick(1).await;
    signal.changed().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn sequential_pauses_restart_window() {
    let watchdog = idle(1_000, None, None);
    let mut signal = watchdog.signal();
    for _ in 0..2 { tick(250).await; with_bridge_timeout_pause(Some(&watchdog), tick(5_000)).await; }
    tick(999).await;
    assert!(signal.borrow().is_none());
    tick(1).await;
    signal.changed().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn failed_bridge_resumes_window() {
    let watchdog = idle(1_000, None, None);
    let mut signal = watchdog.signal();
    let result: Result<(), &str> = with_bridge_timeout_pause(Some(&watchdog), async { tick(5_000).await; Err("denied") }).await;
    assert_eq!(result, Err("denied"));
    tick(999).await;
    assert!(signal.borrow().is_none());
    tick(1).await;
    signal.changed().await.unwrap();
    tick(1_000).await;
    assert!(!signal.has_changed().unwrap_or(false));
}

#[tokio::test]
async fn bridge_without_watchdog_runs_once() {
    let mut calls = 0;
    let result = with_bridge_timeout_pause(None, async { calls += 1; 42 }).await;
    assert_eq!(result, 42);
    assert_eq!(calls, 1);
}

#[tokio::test(start_paused = true)]
async fn overlapping_pauses_reference_counted() {
    let watchdog = idle(1_000, None, None);
    let mut signal = watchdog.signal();
    watchdog.pause(); watchdog.pause();
    tick(5_000).await;
    watchdog.resume();
    tick(5_000).await;
    assert!(signal.borrow().is_none());
    watchdog.resume();
    tick(999).await;
    assert!(signal.borrow().is_none());
    tick(1).await;
    signal.changed().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn idle_disposal_never_fires() {
    let watchdog = idle(1_000, None, None);
    watchdog.dispose();
    tick(5_000).await;
    assert!(watchdog.signal().borrow().is_none());
}

#[tokio::test(start_paused = true)]
async fn settled_idle_ignores_pause_resume() {
    let watchdog = idle(1_000, None, None);
    let mut signal = watchdog.signal();
    tick(1_000).await;
    signal.changed().await.unwrap();
    watchdog.pause(); watchdog.resume();
    tick(5_000).await;
    assert!(!signal.has_changed().unwrap_or(false));
}

#[tokio::test(start_paused = true)]
async fn absolute_deadline_bounds_paused_idle() {
    let watchdog = idle(1_000, None, Some(Instant::now() + Duration::from_millis(500)));
    let mut signal = watchdog.signal();
    watchdog.pause();
    tick(500).await;
    signal.changed().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn run_budget_exhausts_exactly() {
    let budget = RunBudget::new("cell-1".into(), 2_000);
    let mut signal = budget.signal();
    tick(1_999).await;
    assert!(signal.borrow().is_none());
    tick(1).await;
    signal.changed().await.unwrap();
    assert_eq!(signal.borrow().as_ref().unwrap().budget_ms, 2_000);
}

#[tokio::test(start_paused = true)]
async fn run_budget_excludes_host_time() {
    let budget = RunBudget::new("cell-1".into(), 2_000);
    let mut signal = budget.signal();
    tick(1_000).await;
    budget.pause();
    tick(10_000).await;
    assert!(signal.borrow().is_none());
    budget.resume();
    tick(999).await;
    assert!(signal.borrow().is_none());
    tick(1).await;
    signal.changed().await.unwrap();
    assert_eq!(budget.consumed(), Duration::from_millis(2_000));
}

#[tokio::test(start_paused = true)]
async fn run_budget_nested_pause() {
    let budget = RunBudget::new("cell-1".into(), 2_000);
    let mut signal = budget.signal();
    budget.pause(); budget.pause(); budget.resume();
    tick(10_000).await;
    assert!(signal.borrow().is_none());
    budget.resume();
    tick(2_000).await;
    signal.changed().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn run_budget_unmatched_resume() {
    let budget = RunBudget::new("cell-1".into(), 2_000);
    let mut signal = budget.signal();
    budget.resume();
    tick(1_999).await;
    assert!(signal.borrow().is_none());
    tick(1).await;
    signal.changed().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn run_budget_disposed() {
    let budget = RunBudget::new("cell-1".into(), 1_000);
    budget.dispose();
    tick(5_000).await;
    assert!(budget.signal().borrow().is_none());
}

#[tokio::test(start_paused = true)]
async fn run_budget_settles_once() {
    let budget = RunBudget::new("cell-1".into(), 1_000);
    let mut signal = budget.signal();
    tick(1_000).await;
    signal.changed().await.unwrap();
    budget.pause(); budget.resume();
    tick(1_000).await;
    assert!(!signal.has_changed().unwrap_or(false));
}
