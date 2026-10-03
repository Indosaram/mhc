use maho_codemode::tool::{managed_cell::*, types::*, detached_cell_contract::EvalDetachedCellState,cell_deadlines::CellDeadlineKind};

fn input(timeout: Option<f64>) -> EvalToolInput { EvalToolInput { language:EvalLanguage::Js,code:"1".into(),summary:"test".into(),action:None,timeout,on_timeout:None,reset:None } }
#[tokio::test(start_paused = true)] async fn explicit_longer_budget_raises_wall_deadline() {
    let cell=create_managed_cell("cell".into(),input(Some(2000.0)),None,10.0,1800.0,300.0);
    assert_eq!(cell.source.hard_limit_seconds,Some(2000.0));assert_eq!(cell.source.run_budget_seconds,Some(2000.0));assert_eq!(cell.source.state,EvalDetachedCellState::Queued);
    assert!(!cell.can_detach);assert!(cell.source.run_started_at_ms.is_none());
}
#[tokio::test(start_paused = true)] async fn queued_cell_budget_paused_but_hard_deadline_remains_armed() {
    let cell=create_managed_cell("cell".into(),input(Some(1.0)),None,10.0,2.0,300.0);
    let mut signal=cell.deadlines.signal();
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    signal.changed().await.unwrap();
    assert_eq!(signal.borrow().as_ref().unwrap().kind,CellDeadlineKind::HardLimit);
}
