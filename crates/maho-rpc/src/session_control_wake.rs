//! Edge-driven wakes for a session's inbox drain (senpi `modes/interactive/session-control-wake.ts`, `WakeScheduler`).
//! Nothing here runs on a timer: every pass is caused by an edge.
//!
//! One pass runs at a time. Edges that arrive while a pass runs are merged into exactly ONE more pass after it
//! returns, never dropped and never multiplied. Every requester is answered with the result of the pass that covered
//! its edge.
//!
//! The one port of the file: the scheduler both the terminal endpoint and the host's `HostSessionControl` run, the
//! inbox watch (`watchInbox`) and the SIGCONT edge (`onProcessContinue`).
use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
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

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
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

/// Whether an inbox event changed an entry (senpi `fs.watch` "rename" and "change": created, removed, renamed or
/// written). inotify also reports opens, reads and closes, which libuv never surfaces.
fn inbox_entry_changed(kind: &notify::EventKind) -> bool {
    !matches!(kind, notify::EventKind::Access(_))
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
            // An open, read or close changes no entry (senpi's `fs.watch` never surfaces one), and the drain opens
            // the entries it reads: waking on those would re-wake the drain forever.
            Ok(event) if !inbox_entry_changed(&event.kind) => {}
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

    /// senpi test/suite/tui-endpoint-inbox-arm.test.ts "stops arming quietly when the watch is stopped before it
    /// was confirmed". On the current-thread test runtime the arming task has not run when `stop` is called, so the
    /// watch is never confirmed and no sentinel is ever written.
    #[tokio::test]
    async fn stops_arming_quietly_when_the_watch_is_stopped_before_it_was_confirmed() {
        let inbox = tempfile::tempdir().unwrap();
        let errors = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = errors.clone();
        let watch = watch_inbox(
            inbox.path(),
            Arc::new(|| {}),
            Arc::new(move |error| sink.lock().unwrap().push(error)),
        )
        .await
        .unwrap();
        watch.stop();
        tokio::time::timeout(std::time::Duration::from_secs(1), watch.armed)
            .await
            .expect("arming settles after stop");
        assert!(errors.lock().unwrap().is_empty());
        assert_eq!(std::fs::read_dir(inbox.path()).unwrap().count(), 0);
    }

    #[test]
    fn an_access_changes_no_entry_and_every_other_kind_does() {
        use notify::EventKind;
        use notify::event::{AccessKind, AccessMode, CreateKind, RemoveKind};
        assert!(inbox_entry_changed(&EventKind::Create(CreateKind::File)));
        assert!(inbox_entry_changed(&EventKind::Remove(RemoveKind::File)));
        assert!(!inbox_entry_changed(&EventKind::Access(AccessKind::Open(
            AccessMode::Any
        ))));
    }
}
