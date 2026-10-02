use maho_codemode::kernels::shared::{subprocess_contract::KernelRunInput, subprocess_queue::SubprocessRunQueue, subprocess_run::*};
use serde_json::json;

fn input(id: &str) -> KernelRunInput { KernelRunInput { cell_id: id.into(), code: "1".into(), timeout_ms: None } }

#[tokio::test]
async fn fifo_results_release_active_before_next_start() {
    let mut queue = SubprocessRunQueue::default();
    let first = queue.enqueue(input("first"), None, None);
    let second = queue.enqueue(input("second"), None, None);
    assert_eq!(queue.start_next(10.0).unwrap().input.cell_id, "first");
    assert!(queue.start_next(20.0).is_none());
    assert!(!queue.handle_message(json!({"type":"result","cellId":"wrong","ok":true,"durationMs":0}), None));
    assert!(queue.handle_message(json!({"type":"result","cellId":"first","ok":true,"durationMs":1}), None));
    assert_eq!(first.await.unwrap()["ok"], true);
    assert_eq!(queue.start_next(20.0).unwrap().input.cell_id, "second");
    queue.settle_all("closed", 25.0);
    assert_eq!(second.await.unwrap()["durationMs"], 5.0);
}

#[tokio::test]
async fn queued_remove_preserves_active_and_charges_zero() {
    let mut queue = SubprocessRunQueue::default();
    let first = queue.enqueue(input("first"), None, None);
    let second = queue.enqueue(input("second"), None, None);
    queue.start_next(10.0);
    assert!(queue.remove("second", "interrupted", 100.0));
    assert_eq!(second.await.unwrap()["durationMs"], 0.0);
    assert_eq!(queue.snapshot(), (Some("first".into()), vec![]));
    queue.settle_all("closed", 100.0);
    assert_eq!(first.await.unwrap()["ok"], false);
}

#[tokio::test]
async fn tool_call_waiter_and_bounded_backlog() {
    let mut queue = SubprocessRunQueue::default();
    let receiver = queue.next_tool_call();
    queue.push_tool_call(json!({"callId":"direct"}));
    assert_eq!(receiver.await.unwrap()["callId"], "direct");
    for index in 0..257 { queue.push_tool_call(json!({"index":index})); }
    assert_eq!(queue.next_tool_call().await.unwrap()["index"], 1);
    queue.clear_tool_calls();
}

#[tokio::test]
async fn pending_run_settles_once() {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut run = create_pending_run(input("cell"), sender);
    let result = timeout_result(&run, 12);
    assert!(settle_pending_run(&mut run, result));
    assert!(!settle_pending_run(&mut run, json!({})));
    assert_eq!(receiver.await.unwrap()["durationMs"], 12);
}

#[tokio::test]
async fn cancelled_pull_does_not_consume_the_next_tool_frame() {
    let mut queue=SubprocessRunQueue::default();
    drop(queue.next_tool_call());
    let receiver=queue.next_tool_call();
    queue.push_tool_call(json!({"callId":"retained"}));
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(1),receiver).await.expect("cancelled consumer must not discard a frame").unwrap()["callId"],"retained");
    drop(queue.next_tool_call());
    queue.push_tool_call(json!({"callId":"buffered"}));
    assert_eq!(queue.next_tool_call().await.unwrap()["callId"],"buffered");
}
