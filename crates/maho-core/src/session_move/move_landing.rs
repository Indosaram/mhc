//! Landing metadata and reconciliation for a move's single commit point.
use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};

use super::move_ledger::{FailReason, MoveRecord, MoveStatus};
use super::move_ledger_lock::MOVE_RECORD_LOCK_STALE_MS;
use crate::paths::{PathInputOptions, resolve_path};

pub const COMMIT_SETTLE_AFTER_MS: i64 = 3 * MOVE_RECORD_LOCK_STALE_MS;
const UPTIME_RESOLUTION_MS: i64 = 2_000;

#[derive(Clone)]
pub struct StampClock {
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub uptime: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub host: String,
}

impl Default for StampClock {
    fn default() -> Self {
        Self {
            now: Arc::new(|| chrono::Utc::now().timestamp_millis()),
            uptime: Arc::new(system_uptime_ms),
            host: gethostname::gethostname().to_string_lossy().into_owned(),
        }
    }
}

fn system_uptime_ms() -> i64 {
    #[cfg(target_os = "linux")]
    if let Ok(text) = std::fs::read_to_string("/proc/uptime")
        && let Some(first) = text.split_whitespace().next()
        && let Ok(secs) = first.parse::<f64>()
    {
        return (secs * 1000.0).round() as i64;
    }
    0
}

pub fn stamp_fields(clock: &StampClock) -> BTreeMap<String, Value> {
    let mut map = BTreeMap::new();
    map.insert("up".to_owned(), json!((clock.uptime)()));
    map.insert("host".to_owned(), json!(clock.host));
    map
}

fn wall_age(value: &Value, now: i64) -> Option<i64> {
    let at = value.get("at").and_then(Value::as_str)?;
    let parsed = chrono::DateTime::parse_from_rfc3339(at).ok()?;
    Some(now - parsed.timestamp_millis())
}

fn machine_age(value: &Value, clock: &StampClock) -> Option<i64> {
    let up = value.get("up").and_then(Value::as_i64)?;
    let host = value.get("host").and_then(Value::as_str)?;
    if host != clock.host {
        return None;
    }
    let age = (clock.uptime)() - up;
    if age < -UPTIME_RESOLUTION_MS {
        None
    } else {
        Some(age.max(0))
    }
}

pub fn stamp_age(value: &Value, clock: &StampClock) -> Option<i64> {
    if let Some(machine) = machine_age(value, clock)
        && machine < COMMIT_SETTLE_AFTER_MS
    {
        return Some(machine);
    }
    wall_age(value, (clock.now)())
}

pub fn is_commit_stamp(value: &Value, stamp: &str) -> bool {
    value.get("at").and_then(Value::as_str) == Some(stamp) && parse_landing(value).committing
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandingOutcome {
    Absent,
    MovedIn,
    Revoked,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Landing {
    pub target: Option<String>,
    pub cwd: Option<String>,
    pub committing: bool,
}
pub fn parse_landing(value: &Value) -> Landing {
    Landing {
        target: value
            .get("target")
            .and_then(Value::as_str)
            .map(str::to_owned),
        cwd: value.get("cwd").and_then(Value::as_str).map(str::to_owned),
        committing: value.get("committing").and_then(Value::as_bool) == Some(true),
    }
}
pub fn settled_at(landing: &Landing, target: impl Into<String>) -> Landing {
    Landing {
        target: Some(target.into()),
        cwd: landing.cwd.clone(),
        committing: false,
    }
}
pub fn move_folder(cwd: Option<&str>) -> Option<String> {
    let base = std::env::current_dir().unwrap_or_default();
    let base_str = base.to_string_lossy();
    cwd.map(|c| resolve_path(c, &base_str, &PathInputOptions::default()))
}
pub fn held_elsewhere(landing: &Landing) -> String {
    format!(
        "it was readied for {} and lands only there",
        landing.cwd.as_deref().unwrap_or("another folder")
    )
}
pub fn settled_landing(
    record: Option<&MoveRecord>,
    committing: bool,
    outcome: LandingOutcome,
) -> Option<MoveRecord> {
    settled_landing_with_age(record, committing, outcome, None)
}
pub fn settled_landing_with_age(
    record: Option<&MoveRecord>,
    committing: bool,
    outcome: LandingOutcome,
    stamp_age_ms: Option<i64>,
) -> Option<MoveRecord> {
    match record {
        Some(MoveRecord::Status(MoveStatus::Receiving)) if committing => match outcome {
            LandingOutcome::Absent => {
                if let Some(age) = stamp_age_ms
                    && (0..COMMIT_SETTLE_AFTER_MS).contains(&age)
                {
                    None
                } else {
                    Some(MoveRecord::Status(MoveStatus::Failed {
                        reason: FailReason::ImportFailed,
                    }))
                }
            }
            LandingOutcome::MovedIn => Some(MoveRecord::Status(MoveStatus::Accepted)),
            LandingOutcome::Revoked => Some(MoveRecord::Status(MoveStatus::AcceptedRevoked)),
        },
        Some(MoveRecord::Status(MoveStatus::Accepted)) if outcome == LandingOutcome::Revoked => {
            Some(MoveRecord::Status(MoveStatus::AcceptedRevoked))
        }
        _ => None,
    }
}
pub fn lands_at(record: Option<&MoveRecord>, landing: &Landing, target: &str) -> bool {
    matches!(record, Some(MoveRecord::Status(MoveStatus::Receiving)))
        && !landing.committing
        && landing.target.as_deref() == Some(target)
}
pub fn landed_elsewhere(record: Option<&MoveRecord>, landing: &Landing) -> String {
    if matches!(
        record,
        Some(MoveRecord::Status(MoveStatus::AcceptedRevoked))
    ) {
        return "it was revoked here and never lands again".into();
    }
    if landing.committing {
        return "an import stopped mid-commit; only its landed file is accepted".into();
    }
    if matches!(record, Some(MoveRecord::Status(MoveStatus::Accepted))) {
        return "it already landed here once; a move lands once".into();
    }
    landing.target.as_ref().map_or_else(
        || "its landing target is unknown".into(),
        |target| format!("it lands only as {target}"),
    )
}
