use maho_codemode::kernels::js::interrupt_bounds::*;
use std::future::{pending, ready};

#[tokio::test(start_paused = true)]
async fn already_settled_skips_ack_and_grace() {
    assert_eq!(await_cooperative_settlement(true, pending(), Some(pending::<()>()), 500, 2000).await, CooperativeSettlement::Settled);
}
#[tokio::test(start_paused = true)]
async fn settlement_wins_before_acknowledgement() {
    assert_eq!(await_cooperative_settlement(false, ready(()), Some(pending::<()>()), 500, 2000).await, CooperativeSettlement::Settled);
}
#[tokio::test(start_paused = true)]
async fn silence_uses_ack_deadline() {
    let started = tokio::time::Instant::now();
    assert_eq!(await_cooperative_settlement(false, pending(), Some(pending::<()>()), 500, 2000).await, CooperativeSettlement::Unresponsive);
    assert_eq!(started.elapsed(), std::time::Duration::from_millis(500));
}
#[tokio::test(start_paused = true)]
async fn acknowledged_run_receives_grace() {
    let started = tokio::time::Instant::now();
    assert_eq!(await_cooperative_settlement(false, pending(), Some(ready(())), 500, 2000).await, CooperativeSettlement::Unresponsive);
    assert_eq!(started.elapsed(), std::time::Duration::from_millis(2000));
}
#[tokio::test(start_paused = true)]
async fn termination_has_bounded_wait() {
    assert_eq!(retire_worker(ready(()), 3000).await, WorkerRetirement::Terminated);
    let started = tokio::time::Instant::now();
    assert_eq!(retire_worker(pending(), 3000).await, WorkerRetirement::Abandoned);
    assert_eq!(started.elapsed(), std::time::Duration::from_millis(3000));
}
