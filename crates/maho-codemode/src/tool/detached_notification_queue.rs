use std::{collections::VecDeque, future::{Future, poll_fn}, path::PathBuf, pin::Pin, sync::{Arc, Mutex}, task::Poll};
use tokio::{sync::watch, task::JoinHandle};
use super::{detached_cell_contract::EvalDetachedCellSnapshot, detached_cell_notification::{EvalDetachedCellNotification, build_detached_cell_notification}};

pub type NotificationSnapshot = Pin<Box<dyn Future<Output = EvalDetachedCellSnapshot> + Send>>;
pub type SnapshotProvider = Box<dyn FnOnce() -> NotificationSnapshot + Send>;
pub type DetachedNotifier = Arc<dyn Fn(Vec<EvalDetachedCellNotification>) -> Result<(), String> + Send + Sync>;

pub struct PendingDetachedNotification {
    pub snapshot: SnapshotProvider,
    pub spill_path: Option<PathBuf>,
}

#[derive(Default)]
struct QueueState {
    pending: VecDeque<PendingDetachedNotification>,
    flush: Option<watch::Receiver<Option<Result<(), String>>>>,
}

pub struct DetachedNotificationQueue {
    state: Arc<Mutex<QueueState>>,
    notifier: Option<DetachedNotifier>,
    tasks: Vec<JoinHandle<()>>,
}

impl DetachedNotificationQueue {
    pub fn new(notifier: Option<DetachedNotifier>) -> Self {
        Self { state: Arc::new(Mutex::new(QueueState::default())), notifier, tasks: Vec::new() }
    }

    pub fn enqueue(&mut self, notification: PendingDetachedNotification) {
        let mut state = self.state.lock().expect("notification queue poisoned");
        state.pending.push_back(notification);
        if state.flush.is_some() { return; }
        let (sender, receiver) = watch::channel(None);
        state.flush = Some(receiver);
        drop(state);
        let state = Arc::clone(&self.state);
        let notifier = self.notifier.clone();
        self.tasks.retain(|task| !task.is_finished());
        self.tasks.push(tokio::spawn(async move {
            let mut sender = sender;
            loop {
                let pending: Vec<_> = state.lock().expect("notification queue poisoned").pending.drain(..).collect();
                let mut futures: Vec<_> = pending.into_iter().map(|item| {
                    Box::pin(async move {
                        let snapshot = (item.snapshot)().await;
                        build_detached_cell_notification(&snapshot, item.spill_path.as_deref()).await
                    })
                }).collect();
                let mut results = vec![None; futures.len()];
                let notifications = poll_fn(|cx| {
                    for (future, result) in futures.iter_mut().zip(&mut results) {
                        if result.is_none() && let Poll::Ready(value) = future.as_mut().poll(cx) { *result = Some(value); }
                    }
                    if results.iter().all(Option::is_some) {
                        Poll::Ready(results.iter_mut().map(|result| result.take().expect("batch completed")).collect::<Vec<_>>())
                    } else { Poll::Pending }
                }).await;
                let result = notifier.as_ref().map_or(Ok(()), |notify| notify(notifications));
                sender.send_replace(Some(result));
                let mut queue = state.lock().expect("notification queue poisoned");
                queue.flush = None;
                if queue.pending.is_empty() { return; }
                let (next_sender, receiver) = watch::channel(None);
                queue.flush = Some(receiver);
                sender = next_sender;
            }
        }));
    }

    pub async fn flush(&self) -> Result<(), String> {
        let receiver = self.state.lock().expect("notification queue poisoned").flush.clone();
        let Some(mut receiver) = receiver else { return Ok(()); };
        loop {
            if let Some(result) = receiver.borrow().clone() { return result; }
            receiver.changed().await.map_err(|_| "notification queue retired".to_string())?;
        }
    }
}

impl Drop for DetachedNotificationQueue {
    fn drop(&mut self) { for task in &self.tasks { task.abort(); } }
}
