//! Port of senpi packages/agent/src/search/index.ts.

use maho_ai::types::BoxFuture;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub text: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionSearchTop {
    pub entry_id: String,
    pub snippet: Option<String>,
    pub timestamp: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionSearchHit {
    pub session_id: String,
    pub score: Option<f64>,
    pub top: Option<SessionSearchTop>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EntrySearchHit {
    pub session_id: String,
    pub entry_id: String,
    pub timestamp: i64,
    pub snippet: Option<String>,
    pub score: Option<f64>,
}

/// `searchEntries` is optional in TS; `None` is the faithful analogue of the absent method.
pub trait SessionSearchService: Send + Sync {
    fn search_sessions(&self, query: SearchQuery) -> BoxFuture<'static, Vec<SessionSearchHit>>;
    fn search_entries(&self, query: SearchQuery) -> Option<BoxFuture<'static, Vec<EntrySearchHit>>>;
    fn sync(&self) -> BoxFuture<'static, ()>;
    fn notify(&self, session_id: String);
    fn remove(&self, session_id: String) -> BoxFuture<'static, ()>;
    fn close(&self) -> BoxFuture<'static, ()>;
}
