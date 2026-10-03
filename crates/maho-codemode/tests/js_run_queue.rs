use maho_codemode::kernels::{js::run_queue::*, shared::subprocess_contract::KernelRunInput};
use std::sync::{Arc, Mutex};

fn input(id: &str) -> KernelRunInput { KernelRunInput { cell_id: id.into(), code: String::new(), timeout_ms: None } }

#[tokio::test]
async fn removed_cell_settles_while_neighbors_preserve_order() {
    let mut queue = JavaScriptRunQueue::default();
    let _a = queue.enqueue(input("A"), None, None);
    let mut b = queue.enqueue(input("B"), None, None);
    let _c = queue.enqueue(input("C"), None, None);
    queue.start_next(10.0);
    assert!(queue.remove("B", "cancel B"));
    b.changed().await.unwrap();
    assert_eq!(b.borrow().as_ref().unwrap().as_ref().unwrap(), &stopped_result("B", "cancel B"));
    assert_eq!(queue.snapshot(), (Some("A".into()), vec!["C".into()]));
    queue.settle_all("cleanup");
}

#[tokio::test]
async fn activation_fires_once_and_never_for_removed_cell() {
    let mut queue = JavaScriptRunQueue::default();
    let started = Arc::new(Mutex::new(Vec::new()));
    let mut receivers = Vec::new();
    for id in ["A", "B", "C"] {
        let seen = Arc::clone(&started);
        receivers.push(queue.enqueue(input(id), Some(Arc::new(move || seen.lock().unwrap().push(id))), None));
    }
    queue.start_next(10.0);
    assert!(queue.start_next(20.0).is_none());
    assert!(queue.remove("B", "cancel B"));
    let mut a = queue.release_active().unwrap();
    JavaScriptRunQueue::settle(&mut a, serde_json::json!({"ok":true}));
    queue.start_next(30.0);
    assert_eq!(*started.lock().unwrap(), ["A", "C"]);
    queue.settle_all("cleanup");
    for mut receiver in receivers { receiver.changed().await.unwrap(); }
}

#[test]
fn active_and_missing_cells_cannot_be_removed() {
    let mut queue = JavaScriptRunQueue::default();
    let _a = queue.enqueue(input("A"), None, None);
    let _b = queue.enqueue(input("B"), None, None);
    queue.start_next(10.0);
    assert!(!queue.remove("A", "active"));
    assert!(!queue.remove("missing", "unknown"));
    assert!(queue.remove("B", "queued"));
    assert!(!queue.remove("B", "again"));
}

#[tokio::test]
async fn interrupt_result_wins_and_waiting_rejections_are_distinct() {
    let mut queue = JavaScriptRunQueue::default();
    let mut a = queue.enqueue(input("A"), None, None);
    let mut b = queue.enqueue(input("B"), None, None);
    queue.start_next(10.0);
    queue.active_mut().unwrap().interrupt_result = Some(stopped_result("A", "specific interruption"));
    queue.reject_waiting("startup failed");
    b.changed().await.unwrap();
    assert_eq!(b.borrow().as_ref().unwrap().as_ref().unwrap_err(), "startup failed");
    queue.settle_all("generic cleanup");
    a.changed().await.unwrap();
    assert_eq!(a.borrow().as_ref().unwrap().as_ref().unwrap()["error"]["message"], "specific interruption");
}
