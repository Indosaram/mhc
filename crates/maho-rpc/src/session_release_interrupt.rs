//! What `release_session { interrupt: true }` does to a busy session before its final check (senpi
//! `modes/rpc/session-release-interrupt.ts`): it takes the queued input out - reported back to the caller, never
//! run here - then aborts the work and waits, bounded, for it to settle. Taking the input is synchronous, so
//! what it took is known before anything can fail.
use async_trait::async_trait;
use maho_core::agent_session::ClearedQueue;
use maho_core::session_activity::SessionActivitySnapshot;
use maho_core::session_move::ownership::SessionMovedAwayError;
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc, time::Duration};

/// How long an `interrupt` release waits for the aborted work to settle before it re-checks.
pub const RELEASE_SETTLE_MS: u64 = 10_000;

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// What an `interrupt` release took out of the queues: delivery ids to redeliver, user text to restore.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReleaseDropped {
    pub deliveries: Vec<String>,
    pub user_messages: Vec<String>,
}

impl ReleaseDropped {
    /// `NOTHING_DROPPED`.
    pub fn nothing() -> Self {
        Self::default()
    }

    /// The wire form: `{ deliveries, user_messages }`.
    pub fn to_value(&self) -> Value {
        json!({"deliveries": self.deliveries, "user_messages": self.user_messages})
    }
}

/// A failure appending to or writing the session file during a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseWriteError {
    /// The file's ownership already moved elsewhere (`session_moved_away`).
    MovedAway(SessionMovedAwayError),
    Other(String),
}

impl std::fmt::Display for ReleaseWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MovedAway(error) => error.fmt(f),
            Self::Other(message) => f.write_str(message),
        }
    }
}

/// Keeps an event subscription alive; dropping it unsubscribes.
pub type SubscriptionGuard = Box<dyn std::any::Any + Send>;

/// The slice of an `AgentSession` a release reads and drives (senpi `AgentSession`'s activity snapshot, queue,
/// abort, `externalAdmission`, `whenQueueClearRecorded` and the manager's `persistHeaderNow`).
#[async_trait]
pub trait ReleasableSession: Send + Sync {
    fn activity_snapshot(&self) -> SessionActivitySnapshot;
    fn pending_message_count(&self) -> usize;
    /// `externalAdmission.list().pending`: admitted deliveries still waiting to be written.
    fn admission_pending(&self) -> Vec<String>;
    /// `externalAdmission.close(reason)`.
    fn admission_close(&self, reason: &str);
    /// `externalAdmission.reopen()`.
    fn admission_reopen(&self);
    fn clear_queue(&self, abort_will_follow: bool) -> ClearedQueue;
    fn abort_bash(&self);
    /// Calls `on_start` on every `agent_start` event until the guard is dropped.
    fn on_agent_start(&self, on_start: Arc<dyn Fn() + Send + Sync>) -> SubscriptionGuard;
    async fn abort(&self);
    async fn wait_for_idle(&self);
    /// `whenQueueClearRecorded()`.
    async fn when_queue_clear_recorded(&self);
    fn session_id(&self) -> String;
    /// `sessionManager.isTranscriptFlushed()`.
    fn is_transcript_flushed(&self) -> bool;
    /// `sessionManager.persistHeaderNow()`.
    async fn persist_header_now(&self) -> Result<(), String>;
    async fn prepare_move_state(&self, _move_id: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
    /// `sessionManager.appendCustomEntry(customType, data)`: on disk or not at all.
    fn append_custom_entry(&self, custom_type: &str, data: Value) -> Result<(), ReleaseWriteError>;
    /// `extensionRunner.emitSessionMove({ type: "session_move_export", moveId, dir })`: the session's extensions write
    /// the state that must follow it into `dir`, while their runtime is still live. The first failing handler's report
    /// is the release's failure.
    async fn emit_session_move(&self, _move_id: &str, _dir: &str) -> Result<(), String> {
        Ok(())
    }
}

#[async_trait]
pub trait ReleaseSettlePort: Send + Sync {
    async fn other_requests_settled(&self, session_id: &str);
    /// `prompt` calls the session's binding started that have not settled, preflight included.
    fn pending_prompts(&self, session_id: &str) -> Vec<BoxFuture<'static, ()>>;
}

/// Empties both queues. Queued deliveries leave the ledger unwritten, so their sender redelivers them to the
/// next owner; the user's queued text is returned in enqueue order for the caller to restore.
pub fn take_queued_input(session: &dyn ReleasableSession) -> ReleaseDropped {
    let admitted_before = session.admission_pending();
    let cleared = session.clear_queue(true);
    let still_admitted: HashSet<String> = session.admission_pending().into_iter().collect();
    ReleaseDropped {
        deliveries: admitted_before
            .into_iter()
            .filter(|delivery_id| !still_admitted.contains(delivery_id))
            .collect(),
        user_messages: cleared
            .ordered
            .into_iter()
            .map(|queued| queued.text)
            .collect(),
    }
}

pub async fn abort_and_settle(
    port: &dyn ReleaseSettlePort,
    session_id: &str,
    session: &Arc<dyn ReleasableSession>,
) {
    abort_and_settle_within(
        port,
        session_id,
        session,
        Duration::from_millis(RELEASE_SETTLE_MS),
    )
    .await;
}

pub async fn abort_and_settle_within(
    port: &dyn ReleaseSettlePort,
    session_id: &str,
    session: &Arc<dyn ReleasableSession>,
    deadline: Duration,
) {
    session.abort_bash();
    // A prompt still in preflight, or an admitted delivery, may start its run after the abort below.
    let stop_starts = session.on_agent_start({
        let session = Arc::clone(session);
        Arc::new(move || {
            let session = Arc::clone(&session);
            tokio::spawn(async move { session.abort().await });
        })
    });
    let mut work: Vec<BoxFuture<'_, ()>> = vec![
        Box::pin(async {
            session.abort().await;
            session.wait_for_idle().await;
        }),
        Box::pin(port.other_requests_settled(session_id)),
    ];
    work.extend(port.pending_prompts(session_id));
    // `Promise.race([allSettled, expired])`: an expired wait is not an error, the caller re-checks.
    let _ = tokio::time::timeout(deadline, futures::future::join_all(work)).await;
    drop(stop_starts);
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_core::agent_session::QueuedInput;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    pub(crate) struct FakeSession {
        pub pending: Mutex<Vec<String>>,
        pub queued: Mutex<Vec<String>>,
        pub aborts: AtomicUsize,
        pub bash_aborts: AtomicUsize,
        pub idle_waits: AtomicUsize,
        pub hang_idle: AtomicBool,
        pub start_listener: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    }

    struct Listening(Arc<FakeSession>);
    impl Drop for Listening {
        fn drop(&mut self) {
            *self.0.start_listener.lock().unwrap() = None;
        }
    }

    struct FakeHandle(Arc<FakeSession>);

    #[async_trait]
    impl ReleasableSession for FakeHandle {
        fn activity_snapshot(&self) -> SessionActivitySnapshot {
            SessionActivitySnapshot::default()
        }
        fn pending_message_count(&self) -> usize {
            self.0.queued.lock().unwrap().len()
        }
        fn admission_pending(&self) -> Vec<String> {
            self.0.pending.lock().unwrap().clone()
        }
        fn admission_close(&self, _: &str) {}
        fn admission_reopen(&self) {}
        fn clear_queue(&self, abort_will_follow: bool) -> ClearedQueue {
            assert!(abort_will_follow);
            let queued: Vec<String> = std::mem::take(&mut *self.0.queued.lock().unwrap());
            // Queued deliveries leave the admission ledger with the queue.
            self.0.pending.lock().unwrap().retain(|id| id == "kept");
            ClearedQueue {
                steering: queued.clone(),
                follow_up: Vec::new(),
                ordered: queued
                    .into_iter()
                    .enumerate()
                    .map(|(order, text)| QueuedInput {
                        text,
                        mode: maho_ext_api::StreamingBehavior::Steer,
                        enqueue_order: order as u64,
                    })
                    .collect(),
            }
        }
        fn abort_bash(&self) {
            self.0.bash_aborts.fetch_add(1, Ordering::SeqCst);
        }
        fn on_agent_start(&self, on_start: Arc<dyn Fn() + Send + Sync>) -> SubscriptionGuard {
            *self.0.start_listener.lock().unwrap() = Some(on_start);
            Box::new(Listening(Arc::clone(&self.0)))
        }
        async fn abort(&self) {
            self.0.aborts.fetch_add(1, Ordering::SeqCst);
        }
        async fn wait_for_idle(&self) {
            self.0.idle_waits.fetch_add(1, Ordering::SeqCst);
            if self.0.hang_idle.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
        }
        async fn when_queue_clear_recorded(&self) {}
        fn session_id(&self) -> String {
            "s".into()
        }
        fn is_transcript_flushed(&self) -> bool {
            true
        }
        async fn persist_header_now(&self) -> Result<(), String> {
            Ok(())
        }
        fn append_custom_entry(&self, _: &str, _: Value) -> Result<(), ReleaseWriteError> {
            Ok(())
        }
    }

    struct Port {
        settled: AtomicUsize,
        prompt_done: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    #[async_trait]
    impl ReleaseSettlePort for Port {
        async fn other_requests_settled(&self, _: &str) {
            self.settled.fetch_add(1, Ordering::SeqCst);
        }
        fn pending_prompts(&self, _: &str) -> Vec<BoxFuture<'static, ()>> {
            match self.prompt_done.lock().unwrap().take() {
                Some(receiver) => vec![Box::pin(async move {
                    let _ = receiver.await;
                })],
                None => Vec::new(),
            }
        }
    }

    fn port() -> Port {
        Port {
            settled: AtomicUsize::new(0),
            prompt_done: Mutex::new(None),
        }
    }

    #[test]
    fn takes_queued_input_and_reports_only_the_deliveries_the_clear_removed() {
        let fake = Arc::new(FakeSession::default());
        *fake.queued.lock().unwrap() = vec!["first".into(), "second".into()];
        *fake.pending.lock().unwrap() = vec!["d1".into(), "kept".into()];
        let dropped = take_queued_input(&FakeHandle(Arc::clone(&fake)));
        assert_eq!(dropped.user_messages, vec!["first", "second"]);
        assert_eq!(dropped.deliveries, vec!["d1"]);
        assert_eq!(
            dropped.to_value(),
            json!({"deliveries":["d1"],"user_messages":["first","second"]})
        );
        assert_eq!(ReleaseDropped::nothing(), ReleaseDropped::default());
    }

    #[tokio::test]
    async fn aborts_waits_for_idle_and_unsubscribes() {
        let fake = Arc::new(FakeSession::default());
        let session: Arc<dyn ReleasableSession> = Arc::new(FakeHandle(Arc::clone(&fake)));
        let port = port();
        abort_and_settle(&port, "s", &session).await;
        assert_eq!(fake.bash_aborts.load(Ordering::SeqCst), 1);
        assert_eq!(fake.aborts.load(Ordering::SeqCst), 1);
        assert_eq!(fake.idle_waits.load(Ordering::SeqCst), 1);
        assert_eq!(port.settled.load(Ordering::SeqCst), 1);
        assert!(fake.start_listener.lock().unwrap().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_after_the_settle_budget_instead_of_hanging() {
        let fake = Arc::new(FakeSession::default());
        fake.hang_idle.store(true, Ordering::SeqCst);
        let session: Arc<dyn ReleasableSession> = Arc::new(FakeHandle(Arc::clone(&fake)));
        let port = port();
        let (_keep, receiver) = tokio::sync::oneshot::channel();
        *port.prompt_done.lock().unwrap() = Some(receiver);
        let started = tokio::time::Instant::now();
        abort_and_settle(&port, "s", &session).await;
        assert_eq!(started.elapsed(), Duration::from_millis(RELEASE_SETTLE_MS));
        assert!(fake.start_listener.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn a_run_that_starts_while_settling_is_aborted_again() {
        let fake = Arc::new(FakeSession::default());
        let session: Arc<dyn ReleasableSession> = Arc::new(FakeHandle(Arc::clone(&fake)));
        let port = port();
        let (finish, receiver) = tokio::sync::oneshot::channel();
        *port.prompt_done.lock().unwrap() = Some(receiver);
        let settling = abort_and_settle(&port, "s", &session);
        tokio::pin!(settling);
        // Drive the settle until it has subscribed and is waiting on the pending prompt.
        let fired = Arc::clone(&fake);
        let trigger = async move {
            loop {
                let listener = fired.start_listener.lock().unwrap().clone();
                if let Some(listener) = listener {
                    listener();
                    break;
                }
                tokio::task::yield_now().await;
            }
            // The re-abort the listener spawned must land before the prompt settles.
            while fired.aborts.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
            let _ = finish.send(());
        };
        let (_, ()) = tokio::join!(trigger, &mut settling);
        assert!(fake.aborts.load(Ordering::SeqCst) >= 2);
    }
}
