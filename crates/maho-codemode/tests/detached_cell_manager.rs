use std::sync::{Arc, Mutex};
use maho_ext_api::AgentToolResult;
use maho_codemode::tool::{detached_cell_manager::*, detached_cell_contract::EvalDetachedCellState, types::{EvalToolInput, EvalLanguage}};

fn input(language: EvalLanguage) -> EvalToolInput {
    EvalToolInput { language, code:"print(42)".into(), summary:"compute result".into(), action:None, timeout:None, on_timeout:None, reset:None }
}

#[tokio::test]
async fn settlement_removes_live_cell_and_notifies_once() {
    let notifications = Arc::new(Mutex::new(Vec::new()));
    let target = notifications.clone();
    let mut manager = EvalDetachedCellManager::new(DetachedCellManagerOptions { notifier:Some(Arc::new(move |batch| { target.lock().expect("notifications").extend(batch); Ok(()) })), ..Default::default() });
    let cell = manager.create("cell".into(), input(EvalLanguage::Py)).unwrap();
    manager.bind_kernel(&cell, Arc::new(||AgentToolResult::text("42")), Arc::new(||(None, vec![])));
    assert!(manager.detach(&cell));
    assert!(!manager.detach(&cell));
    assert!(manager.create("cell".into(), input(EvalLanguage::Py)).is_err());
    let mut terminal = manager.terminal_signal("cell").unwrap();
    manager.mark_running(&cell);
    assert!(manager.complete(&cell, AgentToolResult::text("42")));
    assert!(!manager.complete(&cell, AgentToolResult::text("duplicate")));
    terminal.wait_for(|value|value.is_some()).await.unwrap();
    assert!(manager.list().0.is_empty());
    assert_eq!(manager.peek("cell").unwrap().state, EvalDetachedCellState::Completed);
    manager.flush_notifications().await.unwrap();
    assert_eq!(notifications.lock().unwrap().len(), 1);
    assert!(manager.create("cell".into(), input(EvalLanguage::Py)).is_ok());
}

#[tokio::test]
async fn capacity_and_live_language_filters_are_shared() {
    let mut manager = EvalDetachedCellManager::new(DetachedCellManagerOptions {max_detached_cells:1,..Default::default()});
    let python = manager.create("python".into(), input(EvalLanguage::Py)).unwrap();
    let javascript = manager.create("javascript".into(), input(EvalLanguage::Js)).unwrap();
    for cell in [&python, &javascript] { manager.bind_kernel(cell, Arc::new(||AgentToolResult::text("")), Arc::new(||(None, vec![]))); }
    assert!(manager.detach(&python));
    assert!(!manager.detach(&javascript));
    assert_eq!(manager.live_cells(Some(EvalLanguage::Js), None).len(), 1);
    assert_eq!(manager.live_cells(None, Some("python")).len(), 1);
    assert!(manager.fail(&python, "failure"));
    assert!(manager.detach(&javascript));
    assert!(manager.cancel_without_interrupt(&javascript));
    manager.flush_notifications().await.unwrap();
    assert_eq!(manager.peek("javascript").unwrap().state, EvalDetachedCellState::Cancelled);
}
