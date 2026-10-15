use super::{fuzzy_files::{FuzzyFileEntry, FuzzyTraversalOptions, collect_fuzzy_file_entries, rank_fuzzy_file_entries}, registry::JsonRpcError};
use serde_json::{Value, json};
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}};

pub type Collector = Arc<dyn Fn(Vec<String>, Arc<AtomicBool>) -> Pin<Box<dyn Future<Output = Vec<FuzzyFileEntry>> + Send>> + Send + Sync>;
pub type Broadcast = Arc<dyn Fn(Value) + Send + Sync>;
pub type Ranker = Arc<dyn Fn(&str, &[FuzzyFileEntry]) -> Vec<super::fuzzy_files::FuzzyFileSearchResult> + Send + Sync>;
struct SearchSession {
    cancelled: Arc<AtomicBool>,
    query: String,
    version: u64,
    entries: Option<Arc<Vec<FuzzyFileEntry>>>,
    completed_version: Option<u64>,
}
#[derive(Default)]
struct State {
    disposed: bool,
    next_search: u64,
    active: BTreeMap<u64, Arc<AtomicBool>>,
    pending: BTreeMap<String, u64>,
    sessions: BTreeMap<String, SearchSession>,
}
struct ActiveSearch {
    state: Arc<Mutex<State>>,
    id: u64,
    token: Option<String>,
    cancelled: Arc<AtomicBool>,
}
impl Drop for ActiveSearch {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active.remove(&self.id);
        if let Some(token) = &self.token && state.pending.get(token) == Some(&self.id) {
            state.pending.remove(token);
        }
    }
}
#[derive(Clone)]
pub struct FuzzyFileSearchService {
    state: Arc<Mutex<State>>,
    collect: Collector,
    broadcast: Broadcast,
    rank: Ranker,
}
impl FuzzyFileSearchService {
    pub fn new(broadcast: Broadcast) -> Self {
        Self::with_collector(broadcast, Arc::new(|roots, cancelled| Box::pin(async move {
            collect_fuzzy_file_entries(&roots, &FuzzyTraversalOptions { cancelled, ..Default::default() }).await
        })))
    }
    pub fn with_collector(broadcast: Broadcast, collect: Collector) -> Self {
        Self::with_ranker(broadcast, collect, Arc::new(rank_fuzzy_file_entries))
    }
    pub fn with_ranker(broadcast: Broadcast, collect: Collector, rank: Ranker) -> Self {
        Self { state: Default::default(), collect, broadcast, rank }
    }
    pub async fn search(&self, query: &str, roots: Vec<String>, token: Option<String>) -> Vec<super::fuzzy_files::FuzzyFileSearchResult> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let id = {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.disposed { return Vec::new(); }
            let id = state.next_search;
            state.next_search += 1;
            if let Some(token) = &token
                && let Some(previous) = state.pending.insert(token.clone(), id)
                && let Some(previous) = state.active.get(&previous) { previous.store(true, Ordering::Release); }
            state.active.insert(id, cancelled.clone());
            id
        };
        let _active = ActiveSearch { state:self.state.clone(), id, token, cancelled:cancelled.clone() };
        let entries = if query.is_empty() { Vec::new() } else { (self.collect)(roots, cancelled.clone()).await };
        let disposed = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).disposed;
        if cancelled.load(Ordering::Acquire) || disposed { return Vec::new(); }
        // Rank on the blocking pool, without holding the shared state lock.
        let (rank, query) = (self.rank.clone(), query.to_owned());
        match tokio::task::spawn_blocking(move || rank(&query, &entries)).await {
            // A newer same-token search or a dispose may have landed during the rank; drop the stale result.
            Ok(_) if cancelled.load(Ordering::Acquire) || self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).disposed => Vec::new(),
            Ok(files) => files,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(_) => Vec::new(),
        }
    }
    pub fn start_session(&self, id: String, roots: Vec<String>) -> Result<(), JsonRpcError> {
        if id.is_empty() { return Err(JsonRpcError::new(-32600, "sessionId must not be empty")); }
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(previous) = state.sessions.remove(&id) { previous.cancelled.store(true, Ordering::Release); }
            cancelled.store(state.disposed, Ordering::Release);
            state.sessions.insert(id.clone(), SearchSession { cancelled:cancelled.clone(), query:String::new(), version:0, entries:None, completed_version:None });
        }
        let service = self.clone();
        tokio::spawn(async move {
            let entries = (service.collect)(roots, cancelled.clone()).await;
            {
                let mut state = service.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(session) = state.sessions.get_mut(&id).filter(|session| Arc::ptr_eq(&session.cancelled, &cancelled) && !cancelled.load(Ordering::Acquire)) else { return; };
                session.entries = Some(Arc::new(entries));
            }
            loop {
                let version = {
                    let state = service.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let Some(session) = state.sessions.get(&id).filter(|session| Arc::ptr_eq(&session.cancelled, &cancelled) && !cancelled.load(Ordering::Acquire)) else { return; };
                    session.version
                };
                if service.emit_version(&id, &cancelled, version).await { break; }
            }
        });
        Ok(())
    }
    pub fn update_session(&self, id: String, query: String) -> Result<(), JsonRpcError> {
        let (cancelled, version, loaded) = {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(session) = state.sessions.get_mut(&id).filter(|session| !session.cancelled.load(Ordering::Acquire)) else {
                return Err(JsonRpcError::new(-32600, format!("fuzzy file search session not found: {id}")));
            };
            session.query = query;
            session.version += 1;
            (session.cancelled.clone(), session.version, session.entries.is_some())
        };
        if loaded {
            let service = self.clone();
            tokio::spawn(async move { service.emit_version(&id, &cancelled, version).await; });
        }
        Ok(())
    }
    async fn emit_version(&self, id: &str, cancelled: &Arc<AtomicBool>, version: u64) -> bool {
        tokio::task::yield_now().await;
        let (query, entries) = {
            let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(session) = state.sessions.get(id).filter(|session| Arc::ptr_eq(&session.cancelled, cancelled) && !cancelled.load(Ordering::Acquire) && session.version == version) else { return false; };
            let Some(entries) = &session.entries else { return false; };
            if session.completed_version == Some(version) { return true; }
            (session.query.clone(), entries.clone())
        };
        // Rank on the blocking pool, outside the lock; the version re-check below still gates completion.
        let (rank, ranked_query) = (self.rank.clone(), query.clone());
        let files = match tokio::task::spawn_blocking(move || rank(&ranked_query, entries.as_slice())).await {
            Ok(files) => files,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            // Only a runtime shutdown cancels the rank; end the session loop.
            Err(_) => return true,
        };
        // The rank yielded the worker; drop a result a newer query or a stop superseded meanwhile.
        // Checking and broadcasting under the lock keeps `sessionUpdated` in version order, as the
        // inline rank under the lock did.
        {
            let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !state.sessions.get(id).is_some_and(|session| Arc::ptr_eq(&session.cancelled, cancelled) && !cancelled.load(Ordering::Acquire) && session.version == version) { return false; }
            (self.broadcast)(json!({"method":"fuzzyFileSearch/sessionUpdated","params":{"sessionId":id,"query":query,"files":files}}));
        }
        {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(session) = state.sessions.get_mut(id).filter(|session| Arc::ptr_eq(&session.cancelled, cancelled) && !cancelled.load(Ordering::Acquire) && session.version == version) else { return false; };
            session.completed_version = Some(version);
        }
        (self.broadcast)(json!({"method":"fuzzyFileSearch/sessionCompleted","params":{"sessionId":id}}));
        true
    }
    pub fn stop_session(&self, id: &str) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(session) = state.sessions.remove(id) { session.cancelled.store(true, Ordering::Release); }
    }
    pub fn dispose(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.disposed = true;
        for cancelled in state.active.values() { cancelled.store(true, Ordering::Release); }
        for session in state.sessions.values() { session.cancelled.store(true, Ordering::Release); }
        state.active.clear(); state.pending.clear(); state.sessions.clear();
    }
}
