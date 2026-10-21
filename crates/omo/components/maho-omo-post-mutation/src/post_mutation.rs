//! File-for-file native Rust port of upstream `post-mutation.ts`.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};
use comment_checker_core::extract_apply_patch_edits;
use maho_ext_api::ToolResultEvent;
use serde_json::Value;
use tokio::sync::watch;

/// Names of tools that perform filesystem mutations.
pub const MUTATION_TOOL_NAMES: &[&str] = &["write", "edit", "apply_patch"];

/// Check whether a tool name is a recognized mutation tool.
pub fn is_mutation_tool_name(tool_name: &str) -> bool {
    MUTATION_TOOL_NAMES.contains(&tool_name)
}

/// Generic post-mutation event representation matching upstream `PostMutationEvent`.
#[derive(Debug, Clone, PartialEq)]
pub struct PostMutationEvent {
    pub tool_name: String,
    pub input: Value,
    pub details: Option<Value>,
}

impl PostMutationEvent {
    pub fn new(tool_name: impl Into<String>, input: Value, details: Option<Value>) -> Self {
        Self {
            tool_name: tool_name.into(),
            input,
            details,
        }
    }
}

impl From<&ToolResultEvent> for PostMutationEvent {
    fn from(event: &ToolResultEvent) -> Self {
        Self {
            tool_name: event.tool_name.clone(),
            input: event.input.clone(),
            details: event.details.clone(),
        }
    }
}

fn add_path(paths: &mut Vec<String>, val: Option<&Value>) {
    if let Some(s) = val.and_then(Value::as_str)
        && !s.is_empty()
        && !paths.iter().any(|p| p == s)
    {
        paths.push(s.to_string());
    }
}

fn add_array_paths(paths: &mut Vec<String>, val: Option<&Value>) {
    if let Some(arr) = val.and_then(Value::as_array) {
        for item in arr {
            add_path(paths, Some(item));
        }
    }
}

/// Extracts mutated file paths from a mutation event matching upstream `extractMutatedFilePaths`.
pub fn extract_mutated_file_paths(event: &PostMutationEvent) -> Vec<String> {
    let mut paths = Vec::new();

    // 1. Direct input path and filePath (upstream excludes empty string, preserves valid whitespace filenames)
    add_path(&mut paths, event.input.get("path"));
    add_path(&mut paths, event.input.get("filePath"));

    // 2. Array forms paths and filePaths
    add_array_paths(&mut paths, event.input.get("paths"));
    add_array_paths(&mut paths, event.input.get("filePaths"));

    // 3. Extract apply_patch edits via comment_checker_core
    let details_val = event.details.as_ref().unwrap_or(&Value::Null);
    let args_map = event.input.as_object();
    for edit in extract_apply_patch_edits(details_val, args_map) {
        let p = edit.file_path;
        if !p.is_empty() && !paths.iter().any(|existing| existing == &p) {
            paths.push(p);
        }
    }

    // 4. Raw input string prefix scan ("*** Add File: ", "*** Update File: ", "*** Move to: ")
    // Upstream: ONLY the sliced prefix line is trimmed: add(line.slice(prefix.length).trim())
    if let Some(input_str) = event.input.get("input").and_then(Value::as_str) {
        for line in input_str.lines() {
            for prefix in &["*** Add File: ", "*** Update File: ", "*** Move to: "] {
                if let Some(rest) = line.strip_prefix(prefix) {
                    let trimmed = rest.trim();
                    if !trimmed.is_empty() && !paths.iter().any(|p| p == trimmed) {
                        paths.push(trimmed.to_string());
                    }
                }
            }
        }
    }

    // 5. Array items in "files" and "changes"
    for key in &["files", "changes"] {
        if let Some(arr) = event.input.get(*key).and_then(Value::as_array) {
            for item in arr {
                if item.is_object() {
                    add_path(&mut paths, item.get("path"));
                    add_path(&mut paths, item.get("filePath"));
                    add_path(&mut paths, item.get("movePath"));
                }
            }
        }
    }

    paths
}

type SharedResult<T> = watch::Receiver<Option<T>>;
type FlightMap<T> = HashMap<String, SharedResult<T>>;

struct LeaderGuard<T: Clone + Send + Sync + 'static> {
    flights: Arc<Mutex<FlightMap<T>>>,
    key: String,
}

impl<T: Clone + Send + Sync + 'static> Drop for LeaderGuard<T> {
    fn drop(&mut self) {
        let mut lock = self.flights.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        lock.remove(&self.key);
    }
}

/// Generic, thread-safe, cancellation-safe single-flight execution coalescer.
#[derive(Debug, Clone)]
pub struct SingleFlight<T: Clone + Send + Sync + 'static> {
    flights: Arc<Mutex<FlightMap<T>>>,
}

impl<T: Clone + Send + Sync + 'static> Default for SingleFlight<T> {
    fn default() -> Self {
        Self {
            flights: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl<T: Clone + Send + Sync + 'static> SingleFlight<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run work for a key with single-flight coalescing and RAII cancellation cleanup.
    pub async fn run<F, Fut>(&self, key: &str, work: F) -> T
    where
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = T> + Send,
    {
        enum State<T: Clone + Send + Sync + 'static> {
            Leader(LeaderGuard<T>, watch::Sender<Option<T>>),
            Follower(watch::Receiver<Option<T>>),
        }

        loop {
            let state = {
                let mut lock = self.flights.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(rx) = lock.get(key) {
                    State::Follower(rx.clone())
                } else {
                    let (tx, rx) = watch::channel(None);
                    lock.insert(key.to_string(), rx);
                    let guard = LeaderGuard {
                        flights: Arc::clone(&self.flights),
                        key: key.to_string(),
                    };
                    State::Leader(guard, tx)
                }
            };

            match state {
                State::Follower(mut rx) => {
                    loop {
                        if let Some(val) = rx.borrow().as_ref() {
                            return val.clone();
                        }
                        if rx.changed().await.is_err() {
                            // Leader dropped without producing a result (aborted/cancelled)
                            if let Some(val) = rx.borrow().as_ref() {
                                return val.clone();
                            }
                            break;
                        }
                    }
                    // Leader was aborted; loop back to retry (one follower will become new leader)
                    continue;
                }
                State::Leader(guard, tx) => {
                    let result = work().await;
                    let _ = tx.send(Some(result.clone()));
                    drop(guard); // Cleans up map entry
                    return result;
                }
            }
        }
    }
}

static FLIGHTS: LazyLock<SingleFlight<Arc<dyn Any + Send + Sync>>> =
    LazyLock::new(SingleFlight::new);

/// Global single-flight execution helper for string-identified paths.
/// Concurrent callers for the same path must use the same result type.
pub async fn run_single_flight<T, F, Fut>(path: &str, work: F) -> T
where
    T: Clone + Send + Sync + 'static,
    F: FnOnce() -> Fut + Send,
    Fut: Future<Output = T> + Send,
{
    let result = FLIGHTS
        .run(path, || async { Arc::new(work().await) as Arc<dyn Any + Send + Sync> })
        .await;
    match result.downcast_ref::<T>() {
        Some(value) => value.clone(),
        None => panic!("concurrent single-flight callers must use the same result type"),
    }
}

/// Session notice tracking matching upstream `createPostMutationSessionState`.
#[derive(Debug, Default, Clone)]
pub struct PostMutationSessionState {
    notices: Arc<Mutex<HashMap<String, HashSet<String>>>>,
}

impl PostMutationSessionState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn should_notice(&self, key: &str, session_id: Option<&str>) -> bool {
        let sid = session_id.unwrap_or("anonymous");
        let mut lock = self.notices.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let set = lock.entry(sid.to_string()).or_default();
        set.insert(key.to_string())
    }

    /// Reset session state matching upstream `reset(sessionId?: string): void { if (sessionId) notices.delete(sessionId) }`.
    /// When sessionId is None or empty, this is a NO-OP.
    pub fn reset(&self, session_id: Option<&str>) {
        if let Some(sid) = session_id
            && !sid.is_empty()
        {
            let mut lock = self.notices.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            lock.remove(sid);
        }
    }
}

pub fn create_post_mutation_session_state() -> PostMutationSessionState {
    PostMutationSessionState::new()
}

#[cfg(test)]
mod tests {
    use super::run_single_flight;
    use std::future::{poll_fn, Future};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Poll;
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn coalesces_duplicate_global_paths_and_releases_completed_flights() {
        // Given: a leader blocked on an exact completion signal.
        let runs = AtomicUsize::new(0);
        let (release_tx, release_rx) = oneshot::channel();
        let mut first = Box::pin(run_single_flight("global-coalescing.ts", || async {
            runs.fetch_add(1, Ordering::SeqCst);
            release_rx.await.expect("test releases the leader");
            42
        }));
        let mut second = Box::pin(run_single_flight("global-coalescing.ts", || async {
            runs.fetch_add(1, Ordering::SeqCst);
            7
        }));

        // When: both calls are polled before the leader is released.
        poll_fn(|cx| {
            assert!(first.as_mut().poll(cx).is_pending());
            assert!(second.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        release_tx.send(()).expect("leader still awaits release");
        let results = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(first, second)
        })
        .await
        .expect("both callers receive the completed result");

        // Then: only the leader ran, and the path can be used again.
        assert_eq!(results, (42, 42));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(run_single_flight("global-coalescing.ts", || async { 9 }).await, 9);
    }

    #[tokio::test]
    async fn global_paths_are_independent_and_failed_results_are_not_cached() {
        // Given: one path remains in flight.
        let mut blocked = Box::pin(run_single_flight("global-blocked.ts", || async {
            std::future::pending::<i32>().await
        }));
        poll_fn(|cx| {
            assert!(blocked.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;

        // When: another path completes with an error result.
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_single_flight("global-error.ts", || async { Err::<i32, _>("failed") }),
        )
        .await
        .expect("unrelated paths execute independently");

        // Then: its error is returned, and subsequent work can succeed.
        assert_eq!(result, Err("failed"));
        assert_eq!(
            run_single_flight("global-error.ts", || async { Ok::<_, &str>(42) }).await,
            Ok(42)
        );
        drop(blocked);
        assert_eq!(run_single_flight("global-blocked.ts", || async { 7 }).await, 7);
    }
}
