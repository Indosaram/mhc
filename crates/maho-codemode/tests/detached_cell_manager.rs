use std::sync::{Arc, Mutex};
use maho_ext_api::AgentToolResult;
use maho_codemode::tool::{detached_cell_manager::*, detached_cell_contract::EvalDetachedCellState, types::{EvalToolInput, EvalLanguage}};

fn input(language: EvalLanguage) -> EvalToolInput {
    EvalToolInput { language, code:"print(42)".into(), summary:"compute result".into(), action:None, timeout:None, on_timeout:None, reset:None }
}

#[tokio::test]
async fn timestamp_ties_and_detach_callbacks_preserve_distinct_insertion_orders() {
    let statuses=Arc::new(Mutex::new(Vec::new()));let target=statuses.clone();
    let mut manager=EvalDetachedCellManager::new(DetachedCellManagerOptions {now:Arc::new(||100.0),max_detached_cells:64,on_status_change:Some(Arc::new(move |entries|target.lock().unwrap().push(entries.into_iter().map(|entry|entry.cell_id).collect::<Vec<_>>()))),..Default::default()});
    let ids=(0..32).map(|index|format!("cell-{index:02}")).collect::<Vec<_>>();
    let mut cells=Vec::new();
    for id in &ids {
        let cell=manager.create(id.clone(),input(EvalLanguage::Py)).unwrap();
        manager.bind_kernel(&cell,Arc::new(||AgentToolResult::text("")),Arc::new(||(None,vec![])));
        cells.push(cell);
    }
    let live=manager.live_cells(None,None).into_iter().map(|cell|cell.cell_id).collect::<Vec<_>>();
    for cell in cells.iter().rev() {assert!(manager.detach(cell));}
    let status=statuses.lock().unwrap().last().unwrap().clone();
    for cell in &cells {manager.cancel_without_interrupt(cell);}
    manager.flush_notifications().await.unwrap();
    assert_eq!(live,ids);
    assert_eq!(status,ids.into_iter().rev().collect::<Vec<_>>());
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

#[tokio::test]
async fn disposal_dequeues_before_interrupt_and_flushes_notifications() {
    use maho_codemode::tool::types::*;
    struct Kernel(Arc<Mutex<Vec<String>>>);
    impl EvalKernel for Kernel {
        fn run(&self,_:EvalKernelRunInput)->EvalKernelFuture<'_,serde_json::Value> {Box::pin(async {panic!("disposal cannot run a cell")})}
        fn cancel_queued<'a>(&'a self,id:&'a str,_:&'a str)->EvalKernelFuture<'a,bool> {Box::pin(async move {self.0.lock().unwrap().push(format!("dequeue:{id}"));Ok(true)})}
        fn interrupt<'a>(&'a self,_:&'a str,id:Option<&'a str>)->EvalKernelFuture<'a,KernelInterruptHandle> {Box::pin(async move {self.0.lock().unwrap().push(format!("interrupt:{}",id.unwrap()));Err("retirement failure fixture".into())})}
        fn queue_snapshot(&self)->(Option<String>,Vec<String>) {(None,vec![])}
        fn deliver_tool_reply(&self,_:serde_json::Value)->Result<(),String> {panic!("disposal cannot reply")}
        fn reset(&self)->EvalKernelFuture<'_,()> {Box::pin(async {panic!("disposal cannot reset")})}
        fn close(&self)->EvalKernelFuture<'_,()> {Box::pin(async {panic!("manager does not own kernel close")})}
    }
    let operations=Arc::new(Mutex::new(Vec::new()));
    let notifications=Arc::new(Mutex::new(Vec::new()));let observed=notifications.clone();
    let manager=Arc::new(Mutex::new(EvalDetachedCellManager::new(DetachedCellManagerOptions {notifier:Some(Arc::new(move |batch| {observed.lock().unwrap().extend(batch);Ok(())})),..Default::default()})));
    let kernel=Arc::new(Kernel(operations.clone()));
    let mut terminals=Vec::new();
    for id in ["running","queued"] {
        let locked= &mut *manager.lock().unwrap();
        let cell=locked.create(id.into(),input(EvalLanguage::Js)).unwrap();
        locked.bind_kernel(&cell,Arc::new(||AgentToolResult::text("partial")),Arc::new(||(None,vec![])));
        cell.lock().unwrap().kernel=Some(kernel.clone());
        if id=="running" {locked.mark_running(&cell);}
        assert!(locked.detach(&cell));
        terminals.push(locked.terminal_signal(id).unwrap());
    }
    EvalDetachedCellManager::dispose(&manager).await.unwrap();
    assert_eq!(*operations.lock().unwrap(),vec!["dequeue:queued","interrupt:running"]);
    assert_eq!(notifications.lock().unwrap().len(),2);
    for terminal in terminals {assert_eq!(terminal.borrow().as_ref().unwrap().state,EvalDetachedCellState::Cancelled);}
    let locked=manager.lock().unwrap();assert!(locked.list().0.is_empty());assert!(locked.list().1.is_empty());
}
