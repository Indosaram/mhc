//! Port of senpi packages/coding-agent/src/core/subscription-limits/usage-cache.ts.
//!
//! The machine-wide record of usage polls. A quota belongs to an account, not to a process, so every
//! maho process on the machine reads the same file: a fresh reading is adopted instead of re-fetched, a
//! backoff one process earned holds for all of them, and a short fetch claim stops two processes that
//! wake together from asking the same question. Account names only, never key material.
//!
//! deviation: the file is read and written as `serde_json::Value`, so `LimitWindow` needs no serde derives.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::Path;

use serde_json::{Map, Value, json};

use super::limit_windows::LimitWindow;

#[derive(Debug, Clone, PartialEq)]
pub struct CachedReading {
    pub windows: Vec<LimitWindow>,
    pub plan: Option<String>,
    pub error: Option<String>,
    /// When the provider answered (or refused).
    pub updated_at: i64,
    /// Not asked again before this: the freshness window on success, the backoff on failure.
    pub next_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchClaim {
    pub claimed_at: i64,
    pub token: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageCacheFile {
    pub accounts: BTreeMap<String, CachedReading>,
    pub fetching: BTreeMap<String, FetchClaim>,
}

pub const CLAIM_MS: i64 = 30_000;

pub fn usage_key(provider: &str, account: &str) -> String {
    format!("{provider}/{account}")
}

pub fn is_due(cache: &UsageCacheFile, key: &str, now: i64) -> bool {
    if let Some(reading) = cache.accounts.get(key)
        && now < reading.next_at
    {
        return false;
    }
    match cache.fetching.get(key) {
        None => true,
        Some(claim) => now - claim.claimed_at >= CLAIM_MS,
    }
}

pub fn with_claims(
    cache: &UsageCacheFile,
    keys: &[String],
    now: i64,
    token: &str,
) -> UsageCacheFile {
    let mut fetching = cache.fetching.clone();
    for key in keys {
        fetching.insert(
            key.clone(),
            FetchClaim {
                claimed_at: now,
                token: token.to_owned(),
            },
        );
    }
    UsageCacheFile {
        accounts: cache.accounts.clone(),
        fetching,
    }
}

pub fn claimed_by(cache: &UsageCacheFile, keys: &[String], token: &str) -> Vec<String> {
    keys.iter()
        .filter(|key| {
            cache
                .fetching
                .get(*key)
                .is_some_and(|claim| claim.token == token)
        })
        .cloned()
        .collect()
}

/// Folds this process's results in and releases its claims; a failure keeps the last windows beside it.
pub fn with_results(
    cache: &UsageCacheFile,
    results: &HashMap<String, CachedReading>,
    token: &str,
) -> UsageCacheFile {
    let mut accounts = cache.accounts.clone();
    let mut fetching = cache.fetching.clone();
    for (key, reading) in results {
        if fetching.get(key).map(|claim| claim.token.as_str()) != Some(token) {
            continue;
        }
        fetching.remove(key);
        let merged = match (&reading.error, accounts.get(key)) {
            (Some(_), Some(previous)) => CachedReading {
                windows: previous.windows.clone(),
                plan: reading.plan.clone().or_else(|| previous.plan.clone()),
                ..reading.clone()
            },
            _ => reading.clone(),
        };
        accounts.insert(key.clone(), merged);
    }
    UsageCacheFile { accounts, fetching }
}

/// No file yet, or one a sibling is mid-way through replacing: everything is simply due.
pub fn read_usage_cache(path: &Path) -> UsageCacheFile {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .map(|value| sanitize(&value))
        .unwrap_or_default()
}

/// Atomic replace, last writer wins: right for a cache, and lock-free.
pub fn write_usage_cache(path: &Path, cache: &UsageCacheFile) {
    if let Err(error) = try_write(path, cache) {
        let _ = std::io::stderr().write_all(
            format!("maho subscription limits: usage cache not written: {error}\n").as_bytes(),
        );
    }
}

fn try_write(path: &Path, cache: &UsageCacheFile) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = std::path::PathBuf::from(temporary);
    write_private(&temporary, to_value(cache).to_string().as_bytes())?;
    std::fs::rename(&temporary, path)
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}

fn window_to_value(window: &LimitWindow) -> Value {
    let mut object = Map::new();
    object.insert("id".into(), json!(window.id));
    object.insert("label".into(), json!(window.label));
    if let Some(minutes) = window.window_minutes {
        object.insert("windowMinutes".into(), json!(minutes));
    }
    object.insert("usedPercent".into(), json!(window.used_percent));
    if let Some(resets_at) = &window.resets_at {
        object.insert("resetsAt".into(), json!(resets_at));
    }
    Value::Object(object)
}

fn to_value(cache: &UsageCacheFile) -> Value {
    let accounts: Map<String, Value> = cache
        .accounts
        .iter()
        .map(|(key, reading)| {
            let mut object = Map::new();
            object.insert(
                "windows".into(),
                Value::Array(reading.windows.iter().map(window_to_value).collect()),
            );
            if let Some(plan) = &reading.plan {
                object.insert("plan".into(), json!(plan));
            }
            if let Some(error) = &reading.error {
                object.insert("error".into(), json!(error));
            }
            object.insert("updatedAt".into(), json!(reading.updated_at));
            object.insert("nextAt".into(), json!(reading.next_at));
            (key.clone(), Value::Object(object))
        })
        .collect();
    let fetching: Map<String, Value> = cache
        .fetching
        .iter()
        .map(|(key, claim)| {
            (
                key.clone(),
                json!({ "claimedAt": claim.claimed_at, "token": claim.token }),
            )
        })
        .collect();
    json!({ "accounts": accounts, "fetching": fetching })
}

fn object(value: Option<&Value>) -> Option<&Map<String, Value>> {
    value.and_then(Value::as_object)
}

fn finite(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
}

fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn sanitize_window(value: &Value) -> Option<LimitWindow> {
    let window = object(Some(value))?;
    let id = text(window.get("id"))?;
    let label = text(window.get("label"))?;
    let used_percent = finite(window.get("usedPercent"))?;
    Some(LimitWindow {
        id,
        label,
        used_percent,
        window_minutes: finite(window.get("windowMinutes")).map(|minutes| minutes as u64),
        resets_at: text(window.get("resetsAt")),
    })
}

/// Shared with other processes and other versions, so what comes off disk is checked, field by field.
fn sanitize(value: &Value) -> UsageCacheFile {
    let file = object(Some(value));
    let mut cache = UsageCacheFile::default();
    for (key, raw) in object(file.and_then(|file| file.get("accounts")))
        .into_iter()
        .flatten()
    {
        let reading = object(Some(raw));
        let (Some(updated_at), Some(next_at)) = (
            finite(reading.and_then(|r| r.get("updatedAt"))),
            finite(reading.and_then(|r| r.get("nextAt"))),
        ) else {
            continue;
        };
        let windows = reading
            .and_then(|r| r.get("windows"))
            .and_then(Value::as_array)
            .map(|windows| windows.iter().filter_map(sanitize_window).collect())
            .unwrap_or_default();
        cache.accounts.insert(
            key.clone(),
            CachedReading {
                windows,
                plan: text(reading.and_then(|r| r.get("plan"))),
                error: text(reading.and_then(|r| r.get("error"))),
                updated_at: updated_at as i64,
                next_at: next_at as i64,
            },
        );
    }
    for (key, raw) in object(file.and_then(|file| file.get("fetching")))
        .into_iter()
        .flatten()
    {
        let claim = object(Some(raw));
        if let (Some(claimed_at), Some(token)) = (
            finite(claim.and_then(|c| c.get("claimedAt"))),
            text(claim.and_then(|c| c.get("token"))),
        ) {
            cache.fetching.insert(
                key.clone(),
                FetchClaim {
                    claimed_at: claimed_at as i64,
                    token,
                },
            );
        }
    }
    cache
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(error: Option<&str>, updated_at: i64, next_at: i64) -> CachedReading {
        CachedReading {
            windows: if error.is_some() {
                vec![]
            } else {
                vec![LimitWindow {
                    id: "claude:5h".into(),
                    label: "5h".into(),
                    window_minutes: Some(300),
                    used_percent: 10.0,
                    resets_at: None,
                }]
            },
            plan: None,
            error: error.map(str::to_owned),
            updated_at,
            next_at,
        }
    }

    #[test]
    fn due_respects_freshness_and_live_claims() {
        let mut cache = UsageCacheFile::default();
        assert!(is_due(&cache, "k", 0));
        cache.accounts.insert("k".into(), reading(None, 0, 100));
        assert!(!is_due(&cache, "k", 99));
        assert!(is_due(&cache, "k", 100));
        let claimed = with_claims(&cache, &["k".to_owned()], 100, "t");
        assert!(!is_due(&claimed, "k", 100 + CLAIM_MS - 1));
        assert!(is_due(&claimed, "k", 100 + CLAIM_MS));
        assert_eq!(
            claimed_by(&claimed, &["k".to_owned()], "t"),
            vec!["k".to_owned()]
        );
        assert!(claimed_by(&claimed, &["k".to_owned()], "other").is_empty());
    }

    #[test]
    fn results_need_the_claim_token_and_a_failure_keeps_previous_windows() {
        let mut cache = UsageCacheFile::default();
        let mut prev = reading(None, 0, 1);
        prev.plan = Some("pro".into());
        cache.accounts.insert("k".into(), prev);
        let claimed = with_claims(&cache, &["k".to_owned()], 5, "t");
        let results = HashMap::from([("k".to_owned(), reading(Some("rate limited"), 6, 9))]);
        let ignored = with_results(&claimed, &results, "stolen");
        assert_eq!(ignored, claimed);
        let folded = with_results(&claimed, &results, "t");
        assert!(folded.fetching.is_empty());
        assert_eq!(folded.accounts["k"].error.as_deref(), Some("rate limited"));
        assert_eq!(folded.accounts["k"].windows.len(), 1);
        assert_eq!(folded.accounts["k"].plan.as_deref(), Some("pro"));
    }

    #[test]
    fn the_file_round_trips_and_corrupt_input_is_empty() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory
            .path()
            .join("nested")
            .join("subscription-usage.json");
        assert_eq!(read_usage_cache(&path), UsageCacheFile::default());
        let mut cache = UsageCacheFile::default();
        cache
            .accounts
            .insert("anthropic-subscription/default".into(), reading(None, 1, 2));
        cache = with_claims(&cache, &["x".to_owned()], 3, "tok");
        write_usage_cache(&path, &cache);
        assert_eq!(read_usage_cache(&path), cache);
        std::fs::write(&path, "{not json").expect("write");
        assert_eq!(read_usage_cache(&path), UsageCacheFile::default());
        std::fs::write(&path, r#"{"accounts":{"a":{"updatedAt":"x","nextAt":1},"b":{"updatedAt":1,"nextAt":2,"windows":[{"id":"i"}]}},"fetching":{"c":{"claimedAt":1}}}"#).expect("write");
        let sanitized = read_usage_cache(&path);
        assert_eq!(sanitized.accounts.len(), 1);
        assert!(sanitized.accounts["b"].windows.is_empty());
        assert!(sanitized.fetching.is_empty());
    }
}
