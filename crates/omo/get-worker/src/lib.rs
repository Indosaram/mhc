//! Rust port of `packages/get-worker`: the omo release HTTP service.
//!
//! The upstream package is a Cloudflare Worker that serves release downloads,
//! install scripts and download statistics, and rolls download analytics into D1 on a
//! schedule. This crate ports the routing and data behavior. The Cloudflare bindings
//! (R2, D1, Analytics Engine, edge cache, `waitUntil`, outbound `fetch`) are bounded
//! traits: production keeps the remote binding, tests inject a deterministic local
//! adapter. No binding has an inert or no-op default.

pub mod bindings;
pub mod channels;
pub mod datapoint;
pub mod download_stats;
pub mod downloads_rollup;
pub mod env;
pub mod http;
pub mod release_names;
pub mod scripts;
pub mod serve_release;

pub use bindings::{
    AnalyticsSink, CacheKey, CachePopulation, DataPoint, DbError, DeferredWork, DownloadDatabase,
    EdgeCache, FetchError, FetchRequest, FetchResponse, HttpFetcher, HttpMetadata, ObjectMeta,
    ReleaseStore, Row, Statement, StoreError, StoredObject, Value,
};
pub use channels::{serve_channel, Resolved, NPM_DIST_TAGS, POINTER_TTL_SECONDS};
pub use datapoint::{
    is_qa_install, record_download, request_country, DownloadEvent, RequestKind, ServedFrom,
};
pub use download_stats::{
    read_download_stats, serve_download_stats, DownloadStats, STATS_TTL_SECONDS,
};
pub use downloads_rollup::{
    parse_rollup_rows, query_analytics, rollup_query, run_downloads_rollup, store_rollup,
    RollupError, RollupRow, ROLLUP_DAYS,
};
pub use env::{Env, RequestContext};
pub use http::{Body, Headers, Method, Request, Response};
pub use release_names::{
    asset_kind, channel_of, channel_pointer_key, completion_marker_key, github_asset_url,
    is_beta_version, is_channel, is_release_version, release_object_key,
    release_version_of_npm_version, AssetKind, Channel, CHANNELS, GITHUB_REPOSITORY,
};
pub use scripts::{
    docs_redirect, is_script_name, script_for_root, script_named, serve_script, ScriptName,
    DOCS_URL, INSTALL_PS1, INSTALL_SH, SCRIPT_TTL_SECONDS,
};
pub use serve_release::{serve_release_asset, IMMUTABLE};

/// A failure the upstream handler lets reject; the platform maps it to a 500.
#[derive(Debug, thiserror::Error)]
pub enum RouteError {
    #[error("database error: {0}")]
    Database(#[from] DbError),
    #[error("fetch error: {0}")]
    Fetch(#[from] FetchError),
}

fn plain(status: u16, body: &str) -> Response {
    Response::new(status, Some(Body::Text(format!("{body}\n"))))
        .with_header("Content-Type", "text/plain; charset=utf-8")
        .with_header("Cache-Control", "no-store")
}

fn channel_path(pathname: &str) -> Option<String> {
    let rest = pathname.strip_prefix("/channels/")?;
    (!rest.is_empty() && !rest.contains('/')).then(|| rest.to_string())
}

fn release_path(pathname: &str) -> Option<(String, String)> {
    let rest = pathname.strip_prefix("/v/")?;
    let (version, asset) = rest.split_once('/')?;
    (!version.is_empty() && !asset.is_empty() && !asset.contains('/'))
        .then(|| (version.to_string(), asset.to_string()))
}

/// `route`: GET/HEAD only, otherwise 405 with newline-terminated text; root script or
/// docs redirect; health; download stats; script names; known channels; versioned
/// release assets; anything else 404.
///
/// # Errors
/// Returns `RouteError` for the two failures the upstream handler lets reject: a
/// database batch error and a transport-level outbound `fetch` error.
pub fn route(request: &Request, ctx: &RequestContext<'_>) -> Result<Response, RouteError> {
    if request.method != Method::Get && request.method != Method::Head {
        return Ok(plain(405, "method not allowed"));
    }
    let pathname = request.pathname();
    if pathname == "/" {
        return Ok(match script_for_root(request) {
            None => docs_redirect(),
            Some(script) => serve_script(request, ctx, script),
        });
    }
    if pathname == "/healthz" {
        return Ok(plain(200, "ok"));
    }
    if pathname == "/stats/downloads" {
        return Ok(serve_download_stats(ctx)?);
    }
    let script_name = pathname.strip_prefix('/').unwrap_or("");
    if let Some(script) = script_named(script_name) {
        return Ok(serve_script(request, ctx, script));
    }
    if let Some(raw) = channel_path(&pathname) {
        return match channel_of(&raw) {
            Some(channel) => Ok(serve_channel(request, ctx, channel)?),
            None => Ok(plain(404, "unknown channel")),
        };
    }
    if let Some((version, asset)) = release_path(&pathname) {
        return Ok(serve_release_asset(request, ctx, &version, &asset));
    }
    Ok(plain(404, "not found"))
}
