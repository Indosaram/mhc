//! Successor-side reopen of handover-parked sessions (senpi `host-handover-reopen.ts`, senpi#3282).
//!
//! A successor host reopens exactly the set its predecessor held resident and parked, restoring each with
//! the same durable id. An entry leaves the record only after a successful reopen (crash-safe, at most once).
//! The loop is written against [`HandoverParkedStore`]; the file-backed store over
//! `<daemonDir>/handover-parked/*.json` is `host_handover_parked::FileHandoverParkedStore`.
use serde_json::Value;
use std::{collections::HashMap, future::Future, pin::Pin, time::Duration};
/// The watch budget must outlast a heavily loaded drain.
pub const HANDOVER_PARKED_REOPEN_BUDGET_MS: u64 = 120_000;
pub const HANDOVER_PARKED_REOPEN_POLL_MS: u64 = 250;
pub const HANDOVER_PARKED_REOPEN_MAX_ATTEMPTS: u32 = 8;
pub const HANDOVER_PARKED_REOPEN_QUIET_STREAK: u32 = 8;
/// `task_status` values after which a parked session is never reopened.
pub const TERMINAL_TASK_STATUSES: [&str; 6] = [
    "completed",
    "cancelled",
    "error",
    "failed",
    "lost",
    "interrupted",
];
pub use crate::host_handover_parked::HandoverParkedEntry;
/// A refusal from the opener; `session_path_in_use` means the predecessor still holds the path's claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReopenError {
    pub code: Option<String>,
    pub message: String,
}
impl std::fmt::Display for ReopenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// The per-entry parked-session record.
pub trait HandoverParkedStore: Send + Sync {
    fn read(&self) -> BoxFuture<'_, Vec<HandoverParkedEntry>>;
    fn clear(&self, session_path: &str) -> BoxFuture<'_, ()>;
    fn drop_if_file_gone(&self, session_path: &str) -> BoxFuture<'_, ()>;
}
/// Opens one parked session on this host, refusing with a [`ReopenError`].
pub type ParkedSessionOpener<'a> =
    &'a (dyn Fn(HandoverParkedEntry) -> BoxFuture<'static, Result<(), ReopenError>> + Send + Sync);
/// The host the watch reopens into. `should_reopen` returns false once that host is draining for a
/// handoff: a predecessor's own watch must never consume a park it wrote for its successor.
pub struct ReopenHolder<'a> {
    pub host_instance: String,
    pub host_socket: Option<String>,
    pub should_reopen: Option<&'a (dyn Fn() -> bool + Send + Sync)>,
}
impl ReopenHolder<'_> {
    fn reopening_stopped(&self) -> bool {
        self.should_reopen.is_some_and(|should| !should())
    }
}
/// Parks a PREVIOUS generation left for this one: a park this host wrote itself is for its successor.
async fn inherited_parks(
    store: &dyn HandoverParkedStore,
    holder: Option<&ReopenHolder<'_>>,
) -> Vec<HandoverParkedEntry> {
    store
        .read()
        .await
        .into_iter()
        .filter(|entry| {
            entry.parked_by.is_none()
                || entry.parked_by.as_deref() != holder.map(|holder| holder.host_instance.as_str())
        })
        .collect()
}
/// The terminal `task_status` of an entry: its own overlay key wins, else the session context's.
pub fn terminal_task_status(entry: &HandoverParkedEntry) -> Option<&str> {
    let status = match entry
        .context_overlay
        .as_ref()
        .and_then(Value::as_object)
        .filter(|overlay| overlay.contains_key("task_status"))
    {
        Some(overlay) => overlay.get("task_status"),
        None => entry
            .session_context
            .as_ref()
            .and_then(|context| context.get("task_status")),
    }
    .and_then(Value::as_str)?;
    TERMINAL_TASK_STATUSES.contains(&status).then_some(status)
}
pub async fn reopen_handover_parked_sessions(
    store: &dyn HandoverParkedStore,
    open: ParkedSessionOpener<'_>,
    holder: Option<&ReopenHolder<'_>>,
) -> () {
    let deadline =
        tokio::time::Instant::now() + Duration::from_millis(HANDOVER_PARKED_REOPEN_BUDGET_MS);
    let mut attempts = HashMap::new();
    let mut held_polls = HashMap::new();
    let mut empty_streak = 0u32;
    let mut saw_any_park = false;
    loop {
        // A draining predecessor must not reopen into its own registry a session it just parked for its successor.
        if holder.is_some_and(ReopenHolder::reopening_stopped) {
            return;
        }
        for entry in inherited_parks(store, holder).await {
            saw_any_park = true;
            reopen_one(store, open, entry, &mut attempts, &mut held_polls, holder).await;
        }
        let remaining = inherited_parks(store, holder).await.len();
        if holder.is_some_and(ReopenHolder::reopening_stopped) {
            return;
        }
        empty_streak = if remaining == 0 { empty_streak + 1 } else { 0 };
        if saw_any_park && empty_streak >= HANDOVER_PARKED_REOPEN_QUIET_STREAK {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(HANDOVER_PARKED_REOPEN_POLL_MS)).await;
    }
}
async fn reopen_one(
    store: &dyn HandoverParkedStore,
    open: ParkedSessionOpener<'_>,
    entry: HandoverParkedEntry,
    attempts: &mut HashMap<String, u32>,
    held_polls: &mut HashMap<String, u32>,
    holder: Option<&ReopenHolder<'_>>,
) {
    if let Some(status) = terminal_task_status(&entry) {
        store.clear(&entry.session_path).await;
        eprintln!(
            "senpi rpc successor dropped a terminal parked session ({status}): {}",
            entry.session_path
        );
        return;
    }
    store.drop_if_file_gone(&entry.session_path).await;
    if !std::path::Path::new(&entry.session_path).exists() {
        eprintln!(
            "senpi rpc successor dropped a parked session whose file is gone: {}",
            entry.session_path
        );
        return;
    }
    let path = entry.session_path.clone();
    match open(entry).await {
        Ok(()) => {
            // The host started draining while this reopen was in flight: keep the record so the successor still reopens it.
            if holder.is_some_and(ReopenHolder::reopening_stopped) {
                return;
            }
            store.clear(&path).await;
        }
        Err(cause) => {
            // `session_path_in_use` is the predecessor still holding the path's claim: the park is waiting, not failing,
            // so it is retried and NOT counted toward the attempt cap.
            if cause.code.as_deref() == Some("session_path_in_use") {
                let polls = held_polls.entry(path.clone()).or_insert(0);
                *polls += 1;
                if *polls == 1 || (*polls).is_multiple_of(40) {
                    eprintln!(
                        "senpi rpc successor reopen parked session {path} waiting on the holder: {cause}"
                    );
                }
                return;
            }
            let tried = attempts.entry(path.clone()).or_insert(0);
            *tried += 1;
            if *tried >= HANDOVER_PARKED_REOPEN_MAX_ATTEMPTS {
                eprintln!(
                    "senpi rpc successor gave up reopening parked session {path} after {tried} attempts: {cause}"
                );
                store.clear(&path).await;
                return;
            }
            eprintln!("senpi rpc successor reopen parked session {path} failed: {cause}");
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    #[derive(Default)]
    struct MemoryStore {
        entries: Mutex<Vec<HandoverParkedEntry>>,
        cleared: Mutex<Vec<String>>,
    }
    impl HandoverParkedStore for MemoryStore {
        fn read(&self) -> BoxFuture<'_, Vec<HandoverParkedEntry>> {
            Box::pin(async {
                self.entries
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            })
        }
        fn clear(&self, session_path: &str) -> BoxFuture<'_, ()> {
            let path = session_path.to_owned();
            Box::pin(async move {
                self.entries
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .retain(|entry| entry.session_path != path);
                self.cleared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(path);
            })
        }
        fn drop_if_file_gone(&self, _: &str) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }
    fn entry(path: &str) -> HandoverParkedEntry {
        HandoverParkedEntry {
            session_path: path.into(),
            ..Default::default()
        }
    }
    #[test]
    fn overlay_task_status_wins_over_the_session_context() {
        let mut parked = entry("p");
        parked.session_context = Some(serde_json::json!({"task_status":"completed"}));
        assert_eq!(terminal_task_status(&parked), Some("completed"));
        parked.context_overlay = Some(serde_json::json!({"task_status":"running"}));
        assert_eq!(terminal_task_status(&parked), None);
        parked.context_overlay = Some(serde_json::json!({"task_status":"lost"}));
        assert_eq!(terminal_task_status(&parked), Some("lost"));
        parked.context_overlay = Some(serde_json::json!({"other":1}));
        assert_eq!(terminal_task_status(&parked), Some("completed"));
        parked.session_context = Some(serde_json::json!({"task_status":7}));
        assert_eq!(terminal_task_status(&parked), None);
    }
    #[tokio::test]
    async fn a_terminal_parked_session_is_cleared_without_opening() {
        let store = MemoryStore::default();
        let mut parked = entry("/nonexistent/terminal.jsonl");
        parked.session_context = Some(serde_json::json!({"task_status":"failed"}));
        store
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(parked);
        let opened = Arc::new(AtomicUsize::new(0));
        let counter = opened.clone();
        let opener = move |_: HandoverParkedEntry| {
            counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) }) as BoxFuture<'static, Result<(), ReopenError>>
        };
        tokio::time::pause();
        reopen_handover_parked_sessions(&store, &opener, None).await;
        assert_eq!(opened.load(Ordering::SeqCst), 0);
        assert_eq!(
            store
                .cleared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_slice(),
            ["/nonexistent/terminal.jsonl"]
        );
    }
    #[tokio::test]
    async fn a_host_that_stopped_reopening_returns_before_the_quiet_streak_and_keeps_the_record() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("s.jsonl");
        std::fs::write(&file, "").unwrap();
        let path = file.to_string_lossy().into_owned();
        let store = MemoryStore::default();
        store
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(entry(&path));
        let draining = Arc::new(AtomicBool::new(false));
        let flag = draining.clone();
        let opener = move |_: HandoverParkedEntry| {
            flag.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(()) }) as BoxFuture<'static, Result<(), ReopenError>>
        };
        let should = Arc::new(move || !draining.load(Ordering::SeqCst));
        let should_ref = should.clone();
        let check = move || should_ref();
        let holder = ReopenHolder {
            host_instance: "me".into(),
            host_socket: None,
            should_reopen: Some(&check),
        };
        tokio::time::pause();
        reopen_handover_parked_sessions(&store, &opener, Some(&holder)).await;
        assert_eq!(
            store
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            1
        );
        assert!(
            store
                .cleared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
    }
    #[tokio::test]
    async fn a_successful_reopen_clears_the_entry_and_the_quiet_streak_ends_the_watch() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("s.jsonl");
        std::fs::write(&file, "").unwrap();
        let path = file.to_string_lossy().into_owned();
        let store = MemoryStore::default();
        store
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(entry(&path));
        let opener = |_: HandoverParkedEntry| {
            Box::pin(async { Ok(()) }) as BoxFuture<'static, Result<(), ReopenError>>
        };
        tokio::time::pause();
        reopen_handover_parked_sessions(&store, &opener, None).await;
        assert!(
            store
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
    }
}
