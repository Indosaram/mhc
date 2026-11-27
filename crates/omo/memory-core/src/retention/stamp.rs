//! Resume points persisted in `runtime/retention.json` (#12532, #12538).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::support::random::random_uuid;

use super::budget::RetentionCursor;
use super::cap::RuntimeCapCursor;

pub const RETENTION_STAMP_CONFIG_KEY: &str = "omo.retentionAt";
const CURSOR_FILE: &str = "retention.json";

pub fn retention_cursor_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(CURSOR_FILE)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionResumePoints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<RetentionCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<RuntimeCapCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap_stalled: Option<bool>,
}

fn parse_cap_cursor(val: Option<&Value>) -> Option<RuntimeCapCursor> {
    let obj = val?.as_object()?;
    if obj.get("store").and_then(Value::as_str) != Some("cap") {
        return None;
    }
    let tier = obj.get("tier").and_then(Value::as_str)?.to_owned();
    let last_name = obj.get("lastName").and_then(Value::as_str)?.to_owned();
    let clock = obj.get("clock").and_then(Value::as_i64)?;
    Some(RuntimeCapCursor {
        store: "cap".to_owned(),
        tier,
        clock,
        last_name,
    })
}

pub fn read_retention_resume_points(runtime_dir: &Path) -> RetentionResumePoints {
    let Ok(raw) = std::fs::read_to_string(retention_cursor_path(runtime_dir)) else {
        return RetentionResumePoints::default();
    };
    let Ok(val) = serde_json::from_str::<Value>(&raw) else {
        return RetentionResumePoints::default();
    };
    let Some(obj) = val.as_object() else {
        return RetentionResumePoints::default();
    };

    // Backwards-compatibility: an older file stored cap cursor directly in `cursor`.
    if let Some(legacy_cap) = parse_cap_cursor(obj.get("cursor")) {
        return RetentionResumePoints {
            cursor: None,
            cap: Some(legacy_cap),
            cap_stalled: None,
        };
    }

    let cap = parse_cap_cursor(obj.get("cap"));
    let cap_stalled =
        if cap.is_some() && obj.get("capStalled").and_then(Value::as_bool) == Some(true) {
            Some(true)
        } else {
            None
        };
    let cursor = obj.get("cursor").and_then(Value::as_object).and_then(|c| {
        let store = c.get("store").and_then(Value::as_str)?.to_owned();
        let last_name = c.get("lastName").and_then(Value::as_str).map(str::to_owned);
        Some(RetentionCursor { store, last_name })
    });

    RetentionResumePoints {
        cursor,
        cap,
        cap_stalled,
    }
}

/// True when the next bind must resume work regardless of completion stamp: a store-walk cursor,
/// or a non-stalled cap resume point.
pub fn retention_resume_pending(runtime_dir: &Path) -> bool {
    let points = read_retention_resume_points(runtime_dir);
    points.cursor.is_some() || (points.cap.is_some() && points.cap_stalled != Some(true))
}

pub fn write_retention_resume_points(
    runtime_dir: &Path,
    points: &RetentionResumePoints,
) -> std::io::Result<()> {
    std::fs::create_dir_all(runtime_dir)?;
    let target = retention_cursor_path(runtime_dir);
    let temp = runtime_dir.join(format!("{CURSOR_FILE}.tmp-{}", random_uuid()));
    let mut obj = serde_json::Map::new();
    obj.insert("version".to_owned(), Value::Number(1u64.into()));
    if let Some(ref c) = points.cursor {
        obj.insert(
            "cursor".to_owned(),
            serde_json::to_value(c)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
        );
    }
    if let Some(ref cap) = points.cap {
        obj.insert(
            "cap".to_owned(),
            serde_json::to_value(cap)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
        );
        if points.cap_stalled == Some(true) {
            obj.insert("capStalled".to_owned(), Value::Bool(true));
        }
    }
    let json = serde_json::to_string(&Value::Object(obj))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if let Err(err) = std::fs::write(&temp, format!("{json}\n")) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&temp, &target) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    Ok(())
}

pub fn clear_retention_cursor(runtime_dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(retention_cursor_path(runtime_dir)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}
