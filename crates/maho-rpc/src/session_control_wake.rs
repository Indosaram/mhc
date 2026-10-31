//! Edge-driven wakes for a session's inbox drain (senpi `modes/interactive/session-control-wake.ts`, `WakeScheduler`).
//! Nothing here runs on a timer: every pass is caused by an edge.
//!
//! One pass runs at a time. Edges that arrive while a pass runs are merged into exactly ONE more pass after it
//! returns, never dropped and never multiplied. Every requester is answered with the result of the pass that covered
//! its edge.
//!
//! The inbox watch (`watchInbox`) and SIGCONT edge live with the interactive endpoint; this module is the scheduler
//! the host's `HostSessionControl` shares with it.
use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

/// The reason a coalesced pass reports: the edge a held delivery is waiting for wins.
pub const REASON_PRIORITY: [&str; 7] = [
    "submission",
    "draft_cleared",
    "idle",
    "continue",
    "command",
    "emitted",
    "inbox",
];

/// `SessionControlDrainResult`: `{ admitted: [...] }`.
pub type DrainResult = Value;

pub fn empty_result() -> DrainResult {
    json!({"admitted": []})
}

pub type WakePass =
    Arc<dyn Fn(Value) -> BoxFuture<'static, Result<DrainResult, String>> + Send + Sync>;
pub type WakeError = Arc<dyn Fn(String) + Send + Sync>;

#[derive(Default)]
struct WakeBatch {
    reasons: BTreeSet<&'static str>,
    delivery_ids: Vec<String>,
    waiters: Vec<oneshot::Sender<DrainResult>>,
}

#[derive(Default)]
struct State {
    running: bool,
    disposed: bool,
    next: Option<WakeBatch>,
}

#[derive(Clone)]
pub struct WakeScheduler {
    state: Arc<Mutex<State>>,
    /// Edges arrive on threads with no runtime (the inbox watch runs on `notify`'s own thread), so passes are
    /// spawned through the runtime the scheduler was created on.
    runtime: tokio::runtime::Handle,
    pass: WakePass,
    on_error: WakeError,
}

fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The `session_control_wake` event one batch covers.
fn wake_event(batch: &WakeBatch) -> Value {
    let reasons = REASON_PRIORITY
        .into_iter()
        .filter(|reason| batch.reasons.contains(reason))
        .collect::<Vec<_>>();
    let mut event = json!({
        "type": "session_control_wake",
        "reason": reasons.first().copied().unwrap_or("inbox"),
        "reasons": reasons,
    });
    if !batch.delivery_ids.is_empty() {
        event["delivery_ids"] = json!(batch.delivery_ids);
    }
    event
}

impl WakeScheduler {
    /// Must be called within a tokio runtime; every pass runs on it, whichever thread requests the wake.
    pub fn new(pass: WakePass, on_error: WakeError) -> Self {
        Self {
            state: Arc::default(),
            runtime: tokio::runtime::Handle::current(),
            pass,
            on_error,
        }
    }

    /// Resolves with the result of the pass that covers this edge; a disposed scheduler answers `admitted: []`.
    pub fn request(
        &self,
        reason: &str,
        delivery_ids: &[String],
    ) -> BoxFuture<'static, DrainResult> {
        let (sender, receiver) = oneshot::channel();
        let start = {
            let mut state = lock(&self.state);
            if state.disposed {
                return Box::pin(async { empty_result() });
            }
            let batch = state.next.get_or_insert_with(WakeBatch::default);
            if let Some(reason) = REASON_PRIORITY.into_iter().find(|known| *known == reason) {
                batch.reasons.insert(reason);
            }
            for id in delivery_ids {
                if !batch.delivery_ids.contains(id) {
                    batch.delivery_ids.push(id.clone());
                }
            }
            batch.waiters.push(sender);
            !std::mem::replace(&mut state.running, true)
        };
        if start {
            self.runtime.spawn(self.clone().run());
        }
        Box::pin(async move { receiver.await.unwrap_or_else(|_| empty_result()) })
    }

    /// Stops every later pass and answers the waiters of the batch not yet started with `admitted: []`.
    pub fn dispose(&self) {
        let batch = {
            let mut state = lock(&self.state);
            state.disposed = true;
            state.next.take()
        };
        for waiter in batch.map(|batch| batch.waiters).unwrap_or_default() {
            let _ = waiter.send(empty_result());
        }
    }

    async fn run(self) {
        loop {
            let batch = {
                let mut state = lock(&self.state);
                match state.next.take().filter(|_| !state.disposed) {
                    Some(batch) => batch,
                    None => {
                        state.running = false;
                        return;
                    }
                }
            };
            let result = match (self.pass)(wake_event(&batch)).await {
                Ok(result) => result,
                Err(error) => {
                    (self.on_error)(error);
                    empty_result()
                }
            };
            for waiter in batch.waiters {
                let _ = waiter.send(result.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn edges_during_a_pass_merge_into_exactly_one_more_pass() {
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let released = Arc::new(tokio::sync::Mutex::new(Some(released)));
        let events = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started) = tokio::sync::oneshot::channel::<()>();
        let started_tx = Arc::new(Mutex::new(Some(started_tx)));
        let seen = events.clone();
        let scheduler = WakeScheduler::new(
            Arc::new(move |event: Value| {
                let seen = seen.clone();
                let released = released.clone();
                let started_tx = started_tx.clone();
                Box::pin(async move {
                    seen.lock().unwrap().push(event);
                    if let Some(started) = started_tx.lock().unwrap().take() {
                        let _ = started.send(());
                    }
                    if let Some(gate) = released.lock().await.take() {
                        let _ = gate.await;
                    }
                    Ok(json!({"admitted": ["d"]}))
                })
            }),
            Arc::new(|_| {}),
        );
        let first = scheduler.request("inbox", &[]);
        started.await.unwrap();
        let second = scheduler.request("command", &["d1".into()]);
        let third = scheduler.request("idle", &[]);
        release.send(()).unwrap();
        assert_eq!(first.await, json!({"admitted": ["d"]}));
        assert_eq!(second.await, json!({"admitted": ["d"]}));
        third.await;
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1]["reason"], "idle");
        assert_eq!(events[1]["reasons"], json!(["idle", "command"]));
        assert_eq!(events[1]["delivery_ids"], json!(["d1"]));
    }

    #[tokio::test]
    async fn a_failed_pass_reports_and_answers_empty_and_dispose_answers_at_once() {
        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = errors.clone();
        let scheduler = WakeScheduler::new(
            Arc::new(|_| Box::pin(async { Err("boom".to_owned()) })),
            Arc::new(move |error| sink.lock().unwrap().push(error)),
        );
        assert_eq!(scheduler.request("inbox", &[]).await, empty_result());
        assert_eq!(*errors.lock().unwrap(), ["boom"]);
        scheduler.dispose();
        assert_eq!(scheduler.request("command", &[]).await, empty_result());
    }

    /// The inbox watch requests wakes from `notify`'s thread, which has no runtime context.
    #[tokio::test]
    async fn a_wake_requested_from_a_thread_without_a_runtime_runs_its_pass() {
        let scheduler = WakeScheduler::new(
            Arc::new(|event: Value| {
                Box::pin(async move { Ok(json!({"admitted": [event["reason"]]})) })
            }),
            Arc::new(|_| {}),
        );
        let requester = scheduler.clone();
        let covered = std::thread::spawn(move || {
            assert!(tokio::runtime::Handle::try_current().is_err());
            requester.request("inbox", &[])
        })
        .join()
        .unwrap();
        assert_eq!(covered.await, json!({"admitted": ["inbox"]}));
    }
}
