//! Port of senpi packages/coding-agent/src/modes/interactive/session-control-wake.ts (v2026.10.27-2).
//!
//! Edge-driven wakes for a session's inbox drain. Nothing here runs on a timer: every pass is caused
//! by an edge (idle, submission, draft cleared, a `wake` command, an inbox entry created or deleted,
//! a delivery written, a continue after a stop).
//!
//! One pass runs at a time. Edges that arrive while a pass runs set the dirty flag - they are merged
//! into exactly ONE more pass after it returns, never dropped and never multiplied, so the drain's own
//! marker deletions cost at most one extra empty pass and then the watcher falls silent.
use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// The reason a coalesced pass reports: the edge a held delivery is waiting for wins.
const REASON_PRIORITY: [&str; 7] = [
    "submission",
    "draft_cleared",
    "idle",
    "continue",
    "command",
    "emitted",
    "inbox",
];

/// One pass: the `session_control_wake` event in, the `SessionControlDrainResult` out.
pub type WakePass = Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;
pub type WakeError = Arc<dyn Fn(String) + Send + Sync>;

fn empty_result() -> Value {
    json!({"admitted": []})
}

#[derive(Default)]
struct WakeBatch {
    reasons: Vec<String>,
    delivery_ids: Vec<String>,
    waiters: Vec<tokio::sync::oneshot::Sender<Value>>,
}

#[derive(Default)]
struct SchedulerState {
    running: bool,
    disposed: bool,
    next: Option<WakeBatch>,
}

#[derive(Clone)]
pub struct WakeScheduler {
    state: Arc<Mutex<SchedulerState>>,
    pass: WakePass,
    on_error: WakeError,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl WakeScheduler {
    pub fn new(pass: WakePass, on_error: WakeError) -> Self {
        Self {
            state: Arc::default(),
            pass,
            on_error,
        }
    }

    /// Resolves with the result of the pass that covers this edge.
    pub fn request(&self, reason: &str, delivery_ids: &[String]) -> BoxFuture<'static, Value> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let start = {
            let mut state = lock(&self.state);
            if state.disposed {
                return Box::pin(async { empty_result() });
            }
            let batch = state.next.get_or_insert_with(WakeBatch::default);
            if !batch.reasons.iter().any(|existing| existing == reason) {
                batch.reasons.push(reason.to_owned());
            }
            for id in delivery_ids {
                if !batch.delivery_ids.contains(id) {
                    batch.delivery_ids.push(id.clone());
                }
            }
            batch.waiters.push(sender);
            let start = !state.running;
            state.running = true;
            start
        };
        if start {
            let scheduler = self.clone();
            tokio::spawn(async move { scheduler.run().await });
        }
        Box::pin(async move { receiver.await.unwrap_or_else(|_| empty_result()) })
    }

    pub fn dispose(&self) {
        let batch = {
            let mut state = lock(&self.state);
            state.disposed = true;
            state.next.take()
        };
        for waiter in batch.map(|batch| batch.waiters).unwrap_or_default() {
            drop(waiter.send(empty_result()));
        }
    }

    async fn run(&self) {
        loop {
            let batch = {
                let mut state = lock(&self.state);
                match state.next.take() {
                    Some(batch) if !state.disposed => batch,
                    other => {
                        state.next = other;
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
                drop(waiter.send(result.clone()));
            }
        }
    }
}

fn wake_event(batch: &WakeBatch) -> Value {
    let reasons = REASON_PRIORITY
        .iter()
        .filter(|reason| batch.reasons.iter().any(|seen| seen == *reason))
        .copied()
        .collect::<Vec<_>>();
    let reason = reasons.first().copied().unwrap_or("inbox");
    let mut event = json!({"type": "session_control_wake", "reason": reason, "reasons": reasons});
    if !batch.delivery_ids.is_empty() {
        event["delivery_ids"] = json!(batch.delivery_ids);
    }
    event
}

/// Prefix of the engine's own entries in an inbox; a registrant never treats them as deliveries.
pub const INBOX_ENGINE_ENTRY_PREFIX: &str = ".senpi-";
const INBOX_ARM_ATTEMPTS: u32 = 25;
const INBOX_ARM_ATTEMPT_MS: u64 = 200;

/// A live inbox watch. `armed` settles once arming did: the watch confirmed, the bounded retry ran
/// out, or it was stopped.
pub struct InboxWatch {
    pub armed: BoxFuture<'static, ()>,
    stop: Arc<dyn Fn() + Send + Sync>,
}

impl InboxWatch {
    pub fn stop(&self) {
        (self.stop)();
    }

    pub fn stopper(&self) -> Arc<dyn Fn() + Send + Sync> {
        self.stop.clone()
    }
}

/// One watch on the inbox directory.
///
/// The watcher arms asynchronously, and an entry written before it is armed produces no event.
/// Arming is confirmed by a sentinel entry's own event coming back - the sentinel is re-touched until
/// then, bounded, which is the only timer here and runs only while arming (an unconfirmed arm is
/// reported through `on_error`). Nothing waits on that confirmation: the watch is returned at once and
/// `armed` settles when arming did, so the caller can request the pass that picks up anything created
/// before the watch could see it.
pub async fn watch_inbox(
    inbox_dir: &Path,
    on_change: Arc<dyn Fn() + Send + Sync>,
    on_error: Arc<dyn Fn(String) + Send + Sync>,
) -> Result<InboxWatch, String> {
    create_private_dir_all(inbox_dir).map_err(|error| error.to_string())?;
    let sentinel = format!("{INBOX_ENGINE_ENTRY_PREFIX}armed-{}", uuid::Uuid::new_v4());
    let (confirm_tx, confirm_rx) = tokio::sync::watch::channel(None::<bool>);
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sentinel_name = sentinel.clone();
    let confirm = confirm_tx.clone();
    let watch_error = on_error.clone();
    let watcher =
        notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
            Ok(event) => {
                for path in &event.paths {
                    let name = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned());
                    if name.as_deref() == Some(sentinel_name.as_str()) {
                        confirm.send_replace(Some(true));
                    } else if !name
                        .as_deref()
                        .is_some_and(|name| name.starts_with(INBOX_ENGINE_ENTRY_PREFIX))
                    {
                        on_change();
                    }
                }
            }
            Err(error) => watch_error(error.to_string()),
        });
    let mut watcher = watcher.map_err(|error| error.to_string())?;
    notify::Watcher::watch(&mut watcher, inbox_dir, notify::RecursiveMode::NonRecursive)
        .map_err(|error| error.to_string())?;
    let watcher = Arc::new(Mutex::new(Some(watcher)));
    let sentinel_path = inbox_dir.join(&sentinel);
    let arm_stopped = stopped.clone();
    let arm_error = on_error.clone();
    let directory = inbox_dir.display().to_string();
    let mut confirmed = confirm_rx.clone();
    let arming = tokio::spawn(async move {
        let seen = confirm_arm(&sentinel_path, &mut confirmed, &arm_stopped).await;
        match seen {
            Ok(false) if !arm_stopped.load(std::sync::atomic::Ordering::SeqCst) => {
                arm_error(format!("inbox watch on {directory} was not confirmed"));
            }
            Err(error) if !arm_stopped.load(std::sync::atomic::Ordering::SeqCst) => {
                arm_error(error);
            }
            _ => {}
        }
    });
    let mut armed_rx = confirm_rx;
    let armed: BoxFuture<'static, ()> = Box::pin(async move {
        // Settles on the sentinel's own event, before any later event of the same watch is
        // delivered; otherwise when arming ended.
        tokio::select! {
            _ = armed_rx.wait_for(|value| *value == Some(true)) => {}
            _ = arming => {}
        }
    });
    let stop_watcher = watcher.clone();
    let stop: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        stopped.store(true, std::sync::atomic::Ordering::SeqCst);
        confirm_tx.send_replace(Some(false));
        drop(lock(&stop_watcher).take());
    });
    Ok(InboxWatch { armed, stop })
}

async fn confirm_arm(
    sentinel_path: &Path,
    confirmed: &mut tokio::sync::watch::Receiver<Option<bool>>,
    stopped: &std::sync::atomic::AtomicBool,
) -> Result<bool, String> {
    let mut seen = false;
    let mut attempt = 0;
    let mut failure = None;
    while attempt < INBOX_ARM_ATTEMPTS
        && !seen
        && !stopped.load(std::sync::atomic::Ordering::SeqCst)
    {
        if let Err(error) = write_private_file(sentinel_path, &attempt.to_string()) {
            failure = Some(error.to_string());
            break;
        }
        seen = tokio::select! {
            result = confirmed.wait_for(Option::is_some) => result.is_ok_and(|value| *value == Some(true)),
            () = tokio::time::sleep(std::time::Duration::from_millis(INBOX_ARM_ATTEMPT_MS)) => false,
        };
        attempt += 1;
    }
    match std::fs::remove_file(sentinel_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => failure = failure.or(Some(error.to_string())),
    }
    failure.map_or(Ok(seen), Err)
}

fn create_private_dir_all(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

fn write_private_file(path: &Path, content: &str) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    std::io::Write::write_all(&mut options.open(path)?, content.as_bytes())
}

/// A continue after any stop (`^Z` or an external SIGSTOP) wakes the drain once the terminal has been
/// restored: the listener is persistent, so every stop/continue cycle is covered, and it defers past
/// the handlers that restore the terminal on the same signal.
pub fn on_process_continue(listener: Arc<dyn Fn() + Send + Sync>) -> Box<dyn FnOnce() + Send> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut continued) = signal(SignalKind::from_raw(libc::SIGCONT)) else {
            return Box::new(|| {});
        };
        let task = tokio::spawn(async move {
            while continued.recv().await.is_some() {
                tokio::task::yield_now().await;
                listener();
            }
        });
        Box::new(move || task.abort())
    }
    #[cfg(not(unix))]
    {
        drop(listener);
        Box::new(|| {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn edges_during_a_pass_merge_into_one_more_pass_with_the_priority_reason() {
        let passes = Arc::new(Mutex::new(Vec::<Value>::new()));
        let (gate_tx, gate_rx) = tokio::sync::watch::channel(false);
        // Sent from inside the first pass: the edges below are requested while it is running.
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let started_tx = Arc::new(Mutex::new(Some(started_tx)));
        let seen = passes.clone();
        let pass: WakePass = Arc::new(
            move |event: Value| -> BoxFuture<'static, Result<Value, String>> {
                let seen = seen.clone();
                let mut gate = gate_rx.clone();
                let started = lock(&started_tx).take();
                Box::pin(async move {
                    let first = lock(&seen).is_empty();
                    lock(&seen).push(event);
                    if first {
                        if let Some(started) = started {
                            drop(started.send(()));
                        }
                        drop(gate.wait_for(|open| *open).await);
                    }
                    Ok(json!({"admitted": ["d"]}))
                })
            },
        );
        let scheduler = WakeScheduler::new(pass, Arc::new(|_| {}));
        let first = scheduler.request("inbox", &[]);
        tokio::time::timeout(std::time::Duration::from_secs(10), started_rx)
            .await
            .expect("the first pass starts")
            .expect("the first pass signals its start");
        let second = scheduler.request("emitted", &["a".into()]);
        let third = scheduler.request("submission", &["b".into()]);
        gate_tx.send_replace(true);
        assert_eq!(first.await["admitted"], json!(["d"]));
        assert_eq!(second.await["admitted"], json!(["d"]));
        assert_eq!(third.await["admitted"], json!(["d"]));
        let passes = lock(&passes).clone();
        assert_eq!(
            passes.len(),
            2,
            "two edges during a pass cost one more pass"
        );
        assert_eq!(passes[1]["reason"], "submission");
        assert_eq!(passes[1]["reasons"], json!(["submission", "emitted"]));
        assert_eq!(passes[1]["delivery_ids"], json!(["a", "b"]));
    }

    #[tokio::test]
    async fn a_failed_pass_reports_and_answers_empty_and_a_disposed_scheduler_answers_empty() {
        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = errors.clone();
        let scheduler = WakeScheduler::new(
            Arc::new(|_: Value| -> BoxFuture<'static, Result<Value, String>> {
                Box::pin(async { Err("boom".to_owned()) })
            }),
            Arc::new(move |error| lock(&sink).push(error)),
        );
        assert_eq!(scheduler.request("idle", &[]).await, empty_result());
        assert_eq!(lock(&errors).clone(), vec!["boom".to_owned()]);
        scheduler.dispose();
        assert_eq!(scheduler.request("idle", &[]).await, empty_result());
    }
}
