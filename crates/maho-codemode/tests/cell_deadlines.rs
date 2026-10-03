use maho_codemode::{tool::cell_deadlines::*, timeouts::idle_timeout::TimeoutPauseHandle};

#[tokio::test(start_paused = true)]
async fn run_budget_wins_and_disarms_hard_limit() {
    let deadlines = CellDeadlines::new("cell".into(), 10.0, 2.0);
    let mut signal = deadlines.signal();
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    signal.changed().await.unwrap();
    assert_eq!(signal.borrow_and_update().as_ref().unwrap().kind, CellDeadlineKind::RunBudget);
    tokio::time::advance(std::time::Duration::from_secs(10)).await;
    assert!(!signal.has_changed().unwrap_or(false));
}

#[tokio::test(start_paused = true)]
async fn hard_limit_still_runs_while_budget_paused() {
    let deadlines = CellDeadlines::new("cell".into(), 2.0, 1.0);
    let mut signal = deadlines.signal();
    deadlines.pause();
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    signal.changed().await.unwrap();
    assert_eq!(signal.borrow_and_update().as_ref().unwrap().kind, CellDeadlineKind::HardLimit);
}

#[tokio::test(start_paused = true)]
async fn host_call_pause_preserves_remaining_budget() {
    let deadlines = CellDeadlines::new("cell".into(), 100.0, 2.0);
    let mut signal = deadlines.signal();
    deadlines.pause();
    tokio::time::advance(std::time::Duration::from_secs(10)).await;
    assert!(!signal.has_changed().unwrap());
    deadlines.resume();
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    signal.changed().await.unwrap();
    assert_eq!(signal.borrow().as_ref().unwrap().kind, CellDeadlineKind::RunBudget);
}

#[tokio::test(start_paused = true)]
async fn clear_disarms_both_deadlines() {
    let deadlines = CellDeadlines::new("cell".into(), 2.0, 1.0);
    let signal = deadlines.signal();
    deadlines.clear();
    deadlines.clear();
    tokio::time::advance(std::time::Duration::from_secs(20)).await;
    assert!(signal.borrow().is_none());
}
