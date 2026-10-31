//! `get_snapshot` (senpi `get-snapshot.ts`): the session state and the newest history entries captured at once,
//! with a marker id the connection enqueues as `snapshot_marker` right after the capture, so a client knows which
//! live records followed the snapshot. Capture and mark happen before any response-writing await.
use crate::get_entries_page::{DEFAULT_ENTRY_PAGE_BYTES, current_response_byte_budget};
use serde_json::{Value, json};

/// The snapshot record type the connection enqueues after the capture (senpi `RpcSnapshotMarker`).
pub const SNAPSHOT_MARKER: &str = "snapshot_marker";

/// Validates `tail` (default 100, non-negative) and `maxBytes` (default 8 MiB, positive).
pub fn snapshot_bounds(
    tail: Option<&Value>,
    max_bytes: Option<&Value>,
) -> Result<(usize, usize), String> {
    let tail = match tail {
        None => 100,
        Some(value) => value
            .as_u64()
            .filter(|value| *value <= 9_007_199_254_740_991)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or("tail must be a non-negative integer")?,
    };
    let max_bytes = match max_bytes {
        None => DEFAULT_ENTRY_PAGE_BYTES,
        Some(value) => value
            .as_u64()
            .filter(|value| *value > 0 && *value <= 9_007_199_254_740_991)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or("maxBytes must be a positive integer")?,
    };
    Ok((tail, max_bytes))
}

/// The `RpcSnapshot` data for `state` and the session's entries in append order; returns it with its marker id.
pub fn build_snapshot(
    state: Value,
    history: &[Value],
    tail: usize,
    max_bytes: usize,
) -> (Value, String) {
    let limit = max_bytes.min(current_response_byte_budget());
    let mut entries = Vec::new();
    let mut bytes = 2;
    for entry in history.iter().rev() {
        if entries.len() >= tail {
            break;
        }
        let projected =
            maho_core::retry_fallback::usage_limit::describe_usage_limit_entry(entry.clone());
        let size = serde_json::to_vec(&projected).map_or(0, |bytes| bytes.len())
            + usize::from(!entries.is_empty());
        if bytes + size > limit {
            break;
        }
        entries.push(projected);
        bytes += size;
    }
    entries.reverse();
    let history_truncated = entries.len() < history.len();
    let next_before = if history_truncated {
        entries
            .first()
            .and_then(|entry| entry["id"].as_str())
            .map_or(Value::Null, Into::into)
    } else {
        Value::Null
    };
    let marker_id = uuid::Uuid::new_v4().to_string();
    (
        json!({
            "state": state,
            "entries": entries,
            "history_truncated": history_truncated,
            "next_before": next_before,
            "markerId": marker_id,
        }),
        marker_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds_default_and_reject_negative_or_zero() {
        assert_eq!(
            snapshot_bounds(None, None).unwrap(),
            (100, DEFAULT_ENTRY_PAGE_BYTES)
        );
        assert_eq!(snapshot_bounds(Some(&json!(0)), None).unwrap().0, 0);
        assert_eq!(
            snapshot_bounds(Some(&json!(-1)), None).unwrap_err(),
            "tail must be a non-negative integer"
        );
        assert_eq!(
            snapshot_bounds(None, Some(&json!(0))).unwrap_err(),
            "maxBytes must be a positive integer"
        );
    }
    #[test]
    fn a_truncated_snapshot_names_the_backward_cursor_and_its_marker() {
        let history: Vec<Value> = (0..3)
            .map(|i| json!({"id":format!("e{i}"),"type":"custom"}))
            .collect();
        let (data, marker) = build_snapshot(json!({"isStreaming":false}), &history, 2, usize::MAX);
        assert_eq!(data["entries"].as_array().unwrap().len(), 2);
        assert_eq!(data["entries"][0]["id"], "e1");
        assert_eq!(data["history_truncated"], true);
        assert_eq!(data["next_before"], "e1");
        assert_eq!(data["markerId"], marker);
        let (whole, _) = build_snapshot(json!({}), &history, 100, usize::MAX);
        assert_eq!(whole["history_truncated"], false);
        assert!(whole["next_before"].is_null());
    }
}
