//! Port of `src/env.ts`: the service's bindings and per-request context.

use crate::bindings::{
    AnalyticsSink, DeferredWork, DownloadDatabase, EdgeCache, HttpFetcher, ReleaseStore,
};

/// Upstream `Env`: release storage, database, download analytics, identifiers.
pub struct Env {
    pub releases: Box<dyn ReleaseStore>,
    pub db: Box<dyn DownloadDatabase>,
    pub downloads: Box<dyn AnalyticsSink>,
    pub account_id: String,
    pub analytics_dataset: String,
    pub analytics_token: Option<String>,
}

/// Upstream `RequestContext`: the bindings plus the edge cache, the deferred-work
/// sink, and the outbound `fetch` seam.
pub struct RequestContext<'a> {
    pub env: &'a Env,
    pub cache: &'a dyn EdgeCache,
    pub wait_until: &'a dyn DeferredWork,
    pub fetch: &'a dyn HttpFetcher,
}
