//! Port of senpi `packages/agent/src/execution/wake-outbox-record.ts`.

use std::collections::{BTreeMap, HashSet};

use serde_json::json;

use crate::durable::store::StoreValue;

use super::wake_types::{
    OutboxRefusalEntry, WAKE_DROPPED_SOURCE, WakeDropReason, WakeEvent, WakeOutboxRecord,
    WakeRefusal,
};

fn is_non_empty_str(v: Option<&StoreValue>) -> Option<&str> {
    v.and_then(StoreValue::as_str).filter(|s| !s.is_empty())
}

fn wake_of(value: &StoreValue) -> Option<WakeEvent> {
    let obj = value.as_object()?;
    let event_id = is_non_empty_str(obj.get("eventId"))?.to_owned();
    let scope = is_non_empty_str(obj.get("scope"))?.to_owned();
    let source = is_non_empty_str(obj.get("source"))?.to_owned();
    let payload = obj.get("payload")?.clone();
    Some(WakeEvent {
        event_id,
        scope,
        source,
        payload,
    })
}

fn refusal_of(value: &StoreValue) -> Option<OutboxRefusalEntry> {
    let obj = value.as_object()?;
    let at = obj.get("at")?.as_i64()?;
    let reason_str = obj.get("reason")?.as_str()?;
    let reason = WakeRefusal::parse(reason_str)?;
    Some(OutboxRefusalEntry { reason, at })
}

#[derive(Debug, Clone, Default)]
pub struct DecodedOutbox {
    pub record: WakeOutboxRecord,
    pub malformed: Vec<StoreValue>,
}

pub fn decode_outbox(value: Option<&StoreValue>) -> Result<DecodedOutbox, String> {
    let Some(value) = value else {
        return Ok(DecodedOutbox::default());
    };
    let Some(obj) = value.as_object() else {
        return Err("corrupt wake outbox record".to_owned());
    };
    let Some(pending_arr) = obj.get("pending").and_then(StoreValue::as_array) else {
        return Err("corrupt wake outbox record".to_owned());
    };
    let Some(closed_obj) = obj.get("closed").and_then(StoreValue::as_object) else {
        return Err("corrupt wake outbox record".to_owned());
    };

    let mut malformed = Vec::new();
    let mut parse_wakes = |arr: &[StoreValue]| -> Vec<WakeEvent> {
        let mut out = Vec::new();
        for entry in arr {
            if let Some(wake) = wake_of(entry) {
                out.push(wake);
            } else {
                malformed.push(entry.clone());
            }
        }
        out
    };

    let pending = parse_wakes(pending_arr);
    let drops = obj
        .get("drops")
        .and_then(StoreValue::as_array)
        .map_or_else(Vec::new, |arr| parse_wakes(arr));

    let mut refused = BTreeMap::new();
    if let Some(refused_obj) = obj.get("refused").and_then(StoreValue::as_object) {
        for (event_id, entry) in refused_obj {
            if let Some(r) = refusal_of(entry) {
                refused.insert(event_id.clone(), r);
            }
        }
    }

    let mut closed = BTreeMap::new();
    for (scope, at) in closed_obj {
        if let Some(ts) = at.as_i64() {
            closed.insert(scope.clone(), ts);
        }
    }

    Ok(DecodedOutbox {
        record: WakeOutboxRecord {
            pending,
            drops,
            refused,
            closed,
        },
        malformed,
    })
}

pub fn malformed_scope(entry: &StoreValue) -> Option<String> {
    let obj = entry.as_object()?;
    is_non_empty_str(obj.get("scope")).map(str::to_owned)
}

pub fn add_drop<F>(
    drops: &[WakeEvent],
    scope: &str,
    reason: WakeDropReason,
    count: u64,
    fresh: &mut HashSet<String>,
    mint: F,
) -> Vec<WakeEvent>
where
    F: FnOnce() -> String,
{
    let reason_str = reason.as_str();
    let pos = drops.iter().rposition(|report| {
        fresh.contains(&report.event_id)
            && report.scope == scope
            && report
                .payload
                .as_object()
                .and_then(|p| p.get("reason"))
                .and_then(StoreValue::as_str)
                == Some(reason_str)
    });
    if let Some(idx) = pos
        && let Some(prev_count) = drops[idx]
            .payload
            .as_object()
            .and_then(|p| p.get("count"))
            .and_then(StoreValue::as_u64)
    {
        let mut next = drops.to_vec();
        next[idx].payload = json!({
            "reason": reason_str,
            "count": prev_count + count,
        });
        return next;
    }
    let event_id = mint();
    fresh.insert(event_id.clone());
    let mut next = drops.to_vec();
    next.push(WakeEvent {
        event_id,
        scope: scope.to_owned(),
        source: WAKE_DROPPED_SOURCE.to_owned(),
        payload: json!({
            "reason": reason_str,
            "count": count,
        }),
    });
    next
}
