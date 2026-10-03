use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};
use maho_ext_api::EventBus;
use maho_omo_task::resumption_channel_emitter::{QueuedResumptionChannelEmitter, ResumptionChannelManager, RESUMPTION_CHANNEL_STATE_EVENT};
use senpi_task::state::{TaskRecord, TaskRecordInput, create_task_record};
struct Manager {
    record: TaskRecord,
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<Option<tokio::sync::oneshot::Receiver<bool>>>,
    calls: AtomicUsize,
}
impl ResumptionChannelManager for Manager {
    fn list(&self, _: &str) -> Vec<TaskRecord> { vec![self.record.clone()] }
    fn was_background(&self, _: &str) -> bool { false }
    fn is_owned_team_member(&self, _: &TaskRecord, _: &str) -> bool { panic!("async ownership must be awaited") }
    fn resolve_owned_team_member<'a>(&'a self, _: &'a TaskRecord, _: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let entered = self.entered.lock().expect("entered").take();
        let release = self.release.lock().expect("release").take();
        Box::pin(async move {
            if let Some(entered) = entered { entered.send(()).expect("observer"); }
            match release { Some(release) => release.await.expect("ownership release"), None => true }
        })
    }
}
#[tokio::test]
async fn shutdown_queues_after_pending_ownership_and_suppresses_changed_snapshot() {
    let (entered, mut observing) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let manager = Arc::new(Manager { record: create_task_record(TaskRecordInput::default(), Some(1)).expect("record"), entered: Mutex::new(Some(entered)), release: Mutex::new(Some(released)), calls: AtomicUsize::new(0) });
    let events = EventBus::default(); let values = Arc::new(Mutex::new(Vec::new())); let captured = values.clone();
    let subscription = events.on(RESUMPTION_CHANNEL_STATE_EVENT, Arc::new(move |event| captured.lock().expect("events").push(event.clone())));
    let emitter = QueuedResumptionChannelEmitter::new(events, manager.clone(), Arc::new(|| Some("session".into())));
    let mut start = emitter.emit_session_start();
    std::future::poll_fn(|context| { assert!(start.as_mut().poll(context).is_pending()); std::task::Poll::Ready(()) }).await;
    observing.try_recv().expect("ownership entered before shutdown");
    let mut changed = emitter.emit_if_changed(); let mut shutdown = emitter.emit_shutdown();
    release.send(true).expect("release");
    std::future::poll_fn(|context| {
        assert!(start.as_mut().poll(context).is_ready());
        assert!(changed.as_mut().poll(context).is_ready());
        assert!(shutdown.as_mut().poll(context).is_ready());
        std::task::Poll::Ready(())
    }).await;
    let values = values.lock().expect("events");
    assert_eq!(values.len(), 2); assert_eq!(values[0]["activeCount"], 1); assert_eq!(values[1]["activeCount"], 0);
    assert_eq!(manager.calls.load(Ordering::SeqCst), 1);
    drop(subscription);
}
#[tokio::test]
async fn dropped_queued_operation_does_not_block_following_shutdown() {
    let (entered, _) = tokio::sync::oneshot::channel(); let (_, released) = tokio::sync::oneshot::channel();
    let manager = Arc::new(Manager { record: create_task_record(TaskRecordInput::default(), Some(1)).expect("record"), entered: Mutex::new(Some(entered)), release: Mutex::new(Some(released)), calls: AtomicUsize::new(0) });
    let emitter = QueuedResumptionChannelEmitter::new(EventBus::default(), manager.clone(), Arc::new(|| Some("session".into())));
    let start = emitter.emit_session_start(); let mut shutdown = emitter.emit_shutdown();
    drop(start);
    std::future::poll_fn(|context| { assert!(shutdown.as_mut().poll(context).is_ready()); std::task::Poll::Ready(()) }).await;
    assert_eq!(manager.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn owned_worker_disposal_cancels_pending_resolution_and_joins_before_clear_returns() {
    use maho_omo_task::resumption_channel_emitter::OwnedResumptionChannels;
    struct Held { record: TaskRecord, entered: std::sync::mpsc::Sender<()> }
    impl ResumptionChannelManager for Held {
        fn list(&self, _: &str) -> Vec<TaskRecord> { vec![self.record.clone()] }
        fn was_background(&self, _: &str) -> bool { false }
        fn is_owned_team_member(&self, _: &TaskRecord, _: &str) -> bool { panic!("await ownership") }
        fn resolve_owned_team_member<'a>(&'a self, _: &'a TaskRecord, _: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
            Box::pin(async move { self.entered.send(()).expect("entered"); std::future::pending().await })
        }
    }
    let (entered, observing) = std::sync::mpsc::channel();
    let values = Arc::new(Mutex::new(Vec::new())); let captured = values.clone(); let events = EventBus::default();
    let subscription = events.on(RESUMPTION_CHANNEL_STATE_EVENT, Arc::new(move |event| captured.lock().expect("events").push(event.clone())));
    let owner = OwnedResumptionChannels::new(events, Arc::new(Held { record: create_task_record(TaskRecordInput::default(), Some(1)).expect("record"), entered }), Arc::new(|| Some("session".into()))).expect("owner");
    let mut start = Box::pin(owner.emit_session_start());
    std::future::poll_fn(|context| { assert!(start.as_mut().poll(context).is_pending()); std::task::Poll::Ready(()) }).await;
    observing.recv_timeout(std::time::Duration::from_secs(5)).expect("worker ownership resolution");
    owner.dispose();
    assert_eq!(values.lock().expect("events").as_slice(), &[serde_json::json!({"source":"senpi-task","activeCount":0,"channels":[]})]);
    std::future::poll_fn(|context| { assert!(matches!(start.as_mut().poll(context), std::task::Poll::Ready(Err(_)))); std::task::Poll::Ready(()) }).await;
    drop(start); drop(owner); drop(subscription);
}
