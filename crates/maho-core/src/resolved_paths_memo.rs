//! Port of senpi packages/coding-agent/src/core/resolved-paths-memo.ts.
//!
//! Host-scoped memo for package resolution. Pending work is shared so concurrent resource loads
//! with identical inputs await the same resolution. Refresh installs a new generation, and failure
//! eviction removes only the generation that failed.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use futures::future::{BoxFuture, FutureExt, Shared};

const MAX_ENTRIES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPathsMemoKeyInput {
    pub agent_dir: String,
    pub cwd: String,
    pub project_trusted: bool,
    pub global_settings: serde_json::Value,
    pub project_settings: serde_json::Value,
    pub additional_extension_paths: Vec<String>,
}

fn stable(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_owned(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::String(value) => serde_json::Value::String(value.clone()).to_string(),
        serde_json::Value::Array(items) => {
            format!("[{}]", items.iter().map(stable).collect::<Vec<_>>().join(","))
        }
        serde_json::Value::Object(entries) => {
            let mut pairs: Vec<(String, String)> = entries
                .iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| (key.clone(), stable(value)))
                .collect();
            pairs.sort();
            format!(
                "{{{}}}",
                pairs
                    .into_iter()
                    .map(|(key, value)| format!("{}:{}", serde_json::Value::String(key), value))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

pub fn resolved_paths_memo_key(input: &ResolvedPathsMemoKeyInput) -> String {
    let mut object = serde_json::Map::new();
    object.insert("agentDir".into(), serde_json::Value::String(input.agent_dir.clone()));
    object.insert("cwd".into(), serde_json::Value::String(input.cwd.clone()));
    object.insert("projectTrusted".into(), serde_json::Value::Bool(input.project_trusted));
    object.insert("globalSettings".into(), input.global_settings.clone());
    object.insert("projectSettings".into(), input.project_settings.clone());
    object.insert(
        "additionalExtensionPaths".into(),
        serde_json::Value::Array(input.additional_extension_paths.iter().cloned().map(serde_json::Value::String).collect()),
    );
    stable(&serde_json::Value::Object(object))
}

type MemoFuture<T> = Shared<BoxFuture<'static, Result<T, String>>>;
struct MemoEntry<T> { generation: u64, future: MemoFuture<T> }
struct MemoStore<T> { entries: BTreeMap<String, MemoEntry<T>>, lru: VecDeque<String> }

/// Store type is public for independently isolated tests; production uses the host singleton.
pub struct ResolvedPathsMemo<T> { inner: Mutex<MemoStore<T>>, generation: AtomicU64 }
impl<T> Default for ResolvedPathsMemo<T> {
    fn default() -> Self { Self { inner: Mutex::new(MemoStore { entries: BTreeMap::new(), lru: VecDeque::new() }), generation: AtomicU64::new(1) } }
}

impl<T: Clone + Send + Sync + 'static> ResolvedPathsMemo<T> {
    pub async fn resolve<F, Fut>(&self, key: String, compute: F, refresh: bool) -> Result<T, String>
    where F: FnOnce() -> Fut, Fut: Future<Output = Result<T, String>> + Send + 'static {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        let (future, entry_generation) = {
            let mut memo = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !refresh {
                if let Some(entry) = memo.entries.get(&key) {
                    let future = entry.future.clone();
                    let entry_generation = entry.generation;
                    memo.lru.retain(|candidate| candidate != &key);
                    memo.lru.push_back(key.clone());
                    (future, entry_generation)
                } else { (Self::insert(&mut memo, key.clone(), generation, compute()), generation) }
            } else { (Self::insert(&mut memo, key.clone(), generation, compute()), generation) }
        };
        let result = future.await;
        if result.is_err() {
            let mut memo = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if memo.entries.get(&key).is_some_and(|entry| entry.generation == entry_generation) {
                memo.entries.remove(&key);
                memo.lru.retain(|candidate| candidate != &key);
            }
        }
        result
    }

    fn insert<Fut>(memo: &mut MemoStore<T>, key: String, generation: u64, work: Fut) -> MemoFuture<T>
    where Fut: Future<Output = Result<T, String>> + Send + 'static {
        memo.lru.retain(|candidate| candidate != &key);
        while memo.entries.len() >= MAX_ENTRIES && !memo.entries.contains_key(&key) {
            if let Some(oldest) = memo.lru.pop_front() { memo.entries.remove(&oldest); } else { break; }
        }
        let future = work.boxed().shared();
        memo.entries.insert(key.clone(), MemoEntry { generation, future: future.clone() });
        memo.lru.push_back(key);
        future
    }

    #[cfg(test)] fn len(&self) -> usize { self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entries.len() }
}

type PathsPair = (crate::package_manager::ResolvedPaths, crate::package_manager::ResolvedPaths);
fn store() -> &'static ResolvedPathsMemo<PathsPair> {
    static MEMO: OnceLock<ResolvedPathsMemo<PathsPair>> = OnceLock::new();
    MEMO.get_or_init(ResolvedPathsMemo::default)
}

pub async fn memoize_resolved_paths<F, Fut>(key: String, compute: F, refresh: bool) -> Result<PathsPair, String>
where F: FnOnce() -> Fut, Fut: Future<Output = Result<PathsPair, String>> + Send + 'static {
    store().resolve(key, compute, refresh).await
}

/// Preserve the public process-wide clear API for explicit cache invalidation.
pub fn clear_resolved_paths_memo() {
    let mut memo = store().inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    memo.entries.clear();
    memo.lru.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn input() -> ResolvedPathsMemoKeyInput {
        ResolvedPathsMemoKeyInput {
            agent_dir: "/agent".into(),
            cwd: "/work".into(),
            project_trusted: false,
            global_settings: json!({ "a": 1, "b": 2 }),
            project_settings: json!({}),
            additional_extension_paths: vec!["/x".into()],
        }
    }

    #[test]
    fn the_key_is_stable_regardless_of_object_order() {
        let mut first = input();
        first.global_settings = json!({ "a": 1, "b": 2 });
        let mut second = input();
        second.global_settings = json!({ "b": 2, "a": 1 });
        assert_eq!(resolved_paths_memo_key(&first), resolved_paths_memo_key(&second));
    }

    #[test]
    fn a_different_input_yields_a_different_key() {
        let mut other = input();
        other.cwd = "/elsewhere".into();
        assert_ne!(resolved_paths_memo_key(&input()), resolved_paths_memo_key(&other));
    }

    #[test]
    fn project_trust_is_independent_of_project_settings() {
        let untrusted = input();
        let mut trusted = input();
        trusted.project_trusted = true;
        assert_eq!(untrusted.project_settings, trusted.project_settings);
        assert_ne!(resolved_paths_memo_key(&untrusted), resolved_paths_memo_key(&trusted));
    }

    #[tokio::test]
    async fn same_key_shares_in_flight_computation() {
        let memo = ResolvedPathsMemo::<usize>::default();
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let first=memo.resolve("shared".into(), { let calls=calls.clone(); move || async move { calls.fetch_add(1,Ordering::SeqCst); let _=started_tx.send(()); let _=release_rx.await; Ok(11) } }, false);
        tokio::pin!(first);
        assert!(futures::poll!(first.as_mut()).is_pending());
        let mut started_rx=started_rx;
        tokio::time::timeout(std::time::Duration::from_secs(2),&mut started_rx).await.expect("start bounded").expect("computation started");
        let second=memo.resolve("shared".into(),||async{Ok(12)},false);
        tokio::pin!(second);
        assert!(futures::poll!(second.as_mut()).is_pending());
        let _=release_tx.send(());
        let (first,second)=tokio::join!(first,second);
        assert_eq!((first,second),(Ok(11),Ok(11)));
        assert_eq!(calls.load(Ordering::SeqCst),1);
    }

    #[tokio::test]
    async fn refresh_replaces_value_and_cached_generation_is_retained() {
        let memo = ResolvedPathsMemo::<usize>::default();
        assert_eq!(memo.resolve("refresh".into(),||async{Ok(1)},false).await,Ok(1));
        assert_eq!(memo.resolve("refresh".into(),||async{Ok(2)},true).await,Ok(2));
        assert_eq!(memo.resolve("refresh".into(),||async{Ok(3)},false).await,Ok(2));
    }

    #[tokio::test]
    async fn least_recently_used_entry_is_evicted_at_capacity() {
        let memo = ResolvedPathsMemo::<usize>::default();
        for index in 0..MAX_ENTRIES { assert_eq!(memo.resolve(format!("k{index}"),move||async move{Ok(index)},false).await,Ok(index)); }
        assert_eq!(memo.resolve("k0".into(),||async{Ok(100)},false).await,Ok(0));
        let count=std::sync::Arc::new(AtomicUsize::new(0));
        let new_count=count.clone();
        assert_eq!(memo.resolve("new".into(),move||async move{new_count.fetch_add(1,Ordering::SeqCst);Ok(101)},false).await,Ok(101));
        let evicted_count=count.clone();
        assert_eq!(memo.resolve("k1".into(),move||async move{evicted_count.fetch_add(1,Ordering::SeqCst);Ok(102)},false).await,Ok(102));
        assert_eq!(count.load(Ordering::SeqCst),2);
        assert_eq!(memo.len(),MAX_ENTRIES);
    }

    #[tokio::test]
    async fn old_failed_waiter_cannot_evict_refreshed_generation() {
        let memo=std::sync::Arc::new(ResolvedPathsMemo::<usize>::default());
        let (started_tx,started_rx)=tokio::sync::oneshot::channel();
        let (release_tx,release_rx)=tokio::sync::oneshot::channel();
        let old={let memo=memo.clone();tokio::spawn(async move{memo.resolve("race".into(),move||async move{let _=started_tx.send(());let _=release_rx.await;Err("old".into())},false).await})};
        tokio::time::timeout(std::time::Duration::from_secs(2),started_rx).await.expect("old start bounded").expect("old computation started");
        assert_eq!(memo.resolve("race".into(),||async{Ok(2)},true).await,Ok(2));
        let _=release_tx.send(());
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(2),old).await.expect("old waiter bounded").expect("old task"),Err("old".into()));
        assert_eq!(memo.resolve("race".into(),||async{Ok(3)},false).await,Ok(2));
    }

    #[tokio::test]
    async fn each_test_store_is_independent() {
        let first=ResolvedPathsMemo::<usize>::default();let second=ResolvedPathsMemo::<usize>::default();
        assert_eq!(first.resolve("k".into(),||async{Ok(1)},false).await,Ok(1));
        assert_eq!(second.resolve("k".into(),||async{Ok(2)},false).await,Ok(2));
    }
}
