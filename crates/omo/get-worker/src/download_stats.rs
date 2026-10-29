//! Port of `src/download-stats.ts`: the public download totals.

use serde::Serialize;

use crate::bindings::{CacheKey, CachePopulation, DbError, DownloadDatabase, Statement};
use crate::env::RequestContext;
use crate::http::{Body, Response};

pub const STATS_TTL_SECONDS: u64 = 300;

/// The public download totals (`Response.json(stats)`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadStats {
    pub served_from_mirror: i64,
    pub redirected_to_git_hub: i64,
    pub adjustments: i64,
    /// Installs GitHub's `download_count` never sees: mirror-served binaries plus
    /// signed adjustments.
    pub uncounted_by_git_hub: i64,
    pub rolled_up_through: Option<String>,
}

/// `readDownloadStats`: one batch of three statements, summed by source.
pub fn read_download_stats(db: &dyn DownloadDatabase) -> Result<DownloadStats, DbError> {
    let results = db.batch(vec![
        Statement::new(
            "SELECT source, SUM(count) AS total FROM downloads_daily WHERE kind = 'binary' GROUP BY source",
        ),
        Statement::new("SELECT COALESCE(SUM(delta), 0) AS total FROM download_adjustments"),
        Statement::new("SELECT MAX(day) AS day FROM downloads_daily"),
    ])?;
    let rows: Vec<(String, i64)> = results
        .first()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|row| Some((row.text("source")?.to_string(), row.integer("total")?)))
        .collect();
    let sum = |sources: &[&str]| -> i64 {
        rows.iter()
            .filter(|(source, _)| sources.contains(&source.as_str()))
            .map(|(_, total)| *total)
            .sum()
    };
    let adjustments = results
        .get(1)
        .and_then(|rows| rows.first())
        .and_then(|row| row.integer("total"))
        .unwrap_or(0);
    let rolled_up_through = results
        .get(2)
        .and_then(|rows| rows.first())
        .and_then(|row| row.text("day"))
        .map(str::to_string);
    let served_from_mirror = sum(&["r2", "cache"]);
    Ok(DownloadStats {
        served_from_mirror,
        redirected_to_git_hub: sum(&["github"]),
        adjustments,
        uncounted_by_git_hub: (served_from_mirror + adjustments).max(0),
        rolled_up_through,
    })
}

/// `serveDownloadStats`: an edge-cache hit short-circuits; a miss reads the database
/// and queues the cache population.
pub fn serve_download_stats(ctx: &RequestContext<'_>) -> Result<Response, DbError> {
    let cache_key = CacheKey::get("https://get.omo.dev/stats/downloads");
    if let Some(cached) = ctx.cache.match_request(&cache_key) {
        return Ok(cached);
    }
    let stats = read_download_stats(ctx.env.db.as_ref())?;
    let body = serde_json::to_string(&stats).map_err(|error| DbError {
        message: error.to_string(),
    })?;
    let response = Response::new(200, Some(Body::Text(body)))
        .with_header("Content-Type", "application/json")
        .with_header(
            "Cache-Control",
            format!("public, max-age={STATS_TTL_SECONDS}"),
        )
        .with_header("Access-Control-Allow-Origin", "*");
    ctx.wait_until.wait_until(CachePopulation {
        key: cache_key,
        response: response.clone(),
    });
    Ok(response)
}
