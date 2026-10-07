//! Port of `src/downloads-rollup.ts`: the hourly Analytics Engine to D1 rollup.

use serde_json::Value as Json;

use crate::bindings::{DownloadDatabase, FetchRequest, HttpFetcher, Statement};
use crate::env::Env;

pub const ROLLUP_DAYS: i64 = 7;

/// One aggregated download row returned by the Analytics Engine SQL API.
#[derive(Clone, Debug, PartialEq)]
pub struct RollupRow {
    pub day: String,
    pub kind: String,
    pub source: String,
    pub version: String,
    pub asset: String,
    pub count: f64,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct RollupError {
    pub message: String,
    pub status: Option<u16>,
}

impl RollupError {
    fn new(message: impl Into<String>) -> Self {
        RollupError {
            message: message.into(),
            status: None,
        }
    }
}

/// `rollupQuery`: only whole UTC days enter the window, so an upsert never replaces a
/// finished day with a partial one; `SUM(_sample_interval * double1)` restores the true
/// count when Analytics Engine samples a busy dataset.
pub fn rollup_query(dataset: &str) -> Result<String, RollupError> {
    if !is_safe_dataset(dataset) {
        return Err(RollupError::new(format!("invalid dataset name {dataset}")));
    }
    let window = ROLLUP_DAYS - 1;
    Ok(format!(
        "SELECT formatDateTime(timestamp, '%Y-%m-%d') AS day, blob1 AS kind, blob2 AS source, blob3 AS version, blob4 AS asset, SUM(_sample_interval * double1) AS count\nFROM {dataset}\nWHERE timestamp >= toStartOfDay(NOW() - INTERVAL '{window}' DAY) AND blob1 IN ('binary', 'checksums', 'engine') AND blob6 != 'qa'\nGROUP BY day, kind, source, version, asset\nFORMAT JSON"
    ))
}

fn is_safe_dataset(dataset: &str) -> bool {
    !dataset.is_empty()
        && dataset
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `parseRollupRows`: rejects a malformed payload instead of storing zeros.
pub fn parse_rollup_rows(payload: &Json) -> Result<Vec<RollupRow>, RollupError> {
    let Some(Json::Array(rows)) = payload.get("data") else {
        return Err(RollupError::new(
            "Analytics Engine response has no data array",
        ));
    };
    rows.iter().map(parse_rollup_row).collect()
}

fn parse_rollup_row(row: &Json) -> Result<RollupRow, RollupError> {
    let Some(day) = row.get("day").and_then(Json::as_str) else {
        return Err(RollupError::new("Analytics Engine row has no day"));
    };
    if !is_day(day) {
        return Err(RollupError::new("Analytics Engine row has no day"));
    }
    let count = row
        .get("count")
        .and_then(to_number)
        .filter(|count| count.is_finite() && *count >= 0.0)
        .ok_or_else(|| RollupError::new("Analytics Engine row has no count"))?;
    Ok(RollupRow {
        day: day.to_string(),
        kind: text_field(row, "kind")?,
        source: text_field(row, "source")?,
        version: text_field(row, "version")?,
        asset: text_field(row, "asset")?,
        count: count.round(),
    })
}

fn text_field(row: &Json, name: &str) -> Result<String, RollupError> {
    row.get(name)
        .and_then(Json::as_str)
        .map(str::to_string)
        .ok_or_else(|| RollupError::new(format!("Analytics Engine row has no {name}")))
}

fn is_day(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}

/// The `Number(value)` coercion the upstream parser relies on for the `count` column.
fn to_number(value: &Json) -> Option<f64> {
    match value {
        Json::Number(number) => number.as_f64(),
        Json::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Some(0.0);
            }
            trimmed.parse::<f64>().ok()
        }
        Json::Bool(flag) => Some(if *flag { 1.0 } else { 0.0 }),
        Json::Null => Some(0.0),
        Json::Array(_) | Json::Object(_) => None,
    }
}

/// `queryAnalytics`: the Cloudflare Analytics Engine SQL API, bearer-token authorized.
pub fn query_analytics(
    env: &Env,
    fetcher: &dyn HttpFetcher,
) -> Result<Vec<RollupRow>, RollupError> {
    let Some(token) = env.analytics_token.as_ref() else {
        return Err(RollupError::new("ANALYTICS_TOKEN is not configured"));
    };
    let response = fetcher
        .fetch(
            FetchRequest::post(
                format!(
                    "https://api.cloudflare.com/client/v4/accounts/{}/analytics_engine/sql",
                    env.account_id
                ),
                rollup_query(&env.analytics_dataset)?,
            )
            .with_header("Authorization", format!("Bearer {token}")),
        )
        .map_err(|error| RollupError::new(error.message))?;
    if !response.ok() {
        return Err(RollupError {
            message: format!("Analytics Engine SQL returned {}", response.status),
            status: Some(response.status),
        });
    }
    parse_rollup_rows(&response.json().map_err(|error| RollupError::new(error.message))?)
}

/// `storeRollup`: an upsert per day so an hourly rerun replaces counts instead of
/// adding them.
pub fn store_rollup(db: &dyn DownloadDatabase, rows: &[RollupRow]) -> Result<usize, RollupError> {
    if rows.is_empty() {
        return Ok(0);
    }
    let statements: Vec<Statement> = rows
        .iter()
        .map(|row| {
            Statement::new(
                "INSERT INTO downloads_daily (day, kind, source, version, asset, count) VALUES (?, ?, ?, ?, ?, ?)\n     ON CONFLICT (day, kind, source, version, asset) DO UPDATE SET count = excluded.count",
            )
            .bind_text(row.day.clone())
            .bind_text(row.kind.clone())
            .bind_text(row.source.clone())
            .bind_text(row.version.clone())
            .bind_text(row.asset.clone())
            .bind_number(row.count)
        })
        .collect();
    db.batch(statements)
        .map_err(|error| RollupError::new(error.message))?;
    Ok(rows.len())
}

/// `runDownloadsRollup`.
pub fn run_downloads_rollup(env: &Env, fetcher: &dyn HttpFetcher) -> Result<usize, RollupError> {
    let rows = query_analytics(env, fetcher)?;
    store_rollup(env.db.as_ref(), &rows)
}
