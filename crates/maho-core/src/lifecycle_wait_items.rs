//! Port of senpi packages/coding-agent/src/core/lifecycle-wait-items.ts.
//!
//! Optional producer detail never determines custody. Lists are snapshots, bounded independently of activeCount.

use crate::session_lifecycle_types::LifecycleWaitItem;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_WAIT_ITEMS: usize = 32;

/// One named item a `wake_source_state` producer waits on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeSourceStateItem {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitSourceItems {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<WakeSourceStateItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted_items: Option<u64>,
}

/// The projected (`label`/`startedAt`) form a lifecycle wait carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectedWaitItems {
    pub items: Option<Vec<LifecycleWaitItem>>,
    pub omitted_items: Option<u64>,
}

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

fn item_json(item: &WakeSourceStateItem) -> String {
    serde_json::to_string(item).unwrap_or_default()
}

/// Dedupes by id (the smallest serialized form wins), sorts by id and caps at 32; the cut counts as omitted.
pub fn normalize_wait_items(value: Option<&Value>, omitted: Option<&Value>) -> WaitSourceItems {
    let mut by_id = std::collections::BTreeMap::<String, WakeSourceStateItem>::new();
    for entry in value.and_then(Value::as_array).into_iter().flatten() {
        let Some(object) = entry.as_object() else {
            continue;
        };
        let Some(id) = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        else {
            continue;
        };
        let item = WakeSourceStateItem {
            id: id.to_owned(),
            description: object
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned),
            started_at_ms: object
                .get("startedAtMs")
                .and_then(Value::as_f64)
                .filter(|ms| ms.is_finite() && ms.abs() <= 8.64e15),
        };
        let replace = by_id
            .get(id)
            .is_none_or(|previous| item_json(&item) < item_json(previous));
        if replace {
            by_id.insert(id.to_owned(), item);
        }
    }
    let items: Vec<_> = by_id.into_values().collect();
    let extra = omitted
        .and_then(Value::as_f64)
        .filter(|n| n.fract() == 0.0 && *n > 0.0 && *n <= MAX_SAFE_INTEGER)
        .map(|n| n as u64)
        .unwrap_or(0);
    let omitted_items = items.len().saturating_sub(MAX_WAIT_ITEMS) as u64 + extra;
    WaitSourceItems {
        items: (!items.is_empty()).then(|| items.into_iter().take(MAX_WAIT_ITEMS).collect()),
        omitted_items: (omitted_items > 0).then_some(omitted_items),
    }
}

/// The one rule for a wake_source_state producer's item list: `items` when set, else the legacy `monitors`.
pub fn wake_source_items(data: &Value) -> Option<&Value> {
    data.get("items")
        .filter(|v| !v.is_null())
        .or_else(|| data.get("monitors").filter(|v| !v.is_null()))
}

pub fn wait_item_signature(detail: &WaitSourceItems) -> String {
    let items = detail.items.as_ref().map(|items| {
        items
            .iter()
            .map(|item| serde_json::json!([item.id, item.description]))
            .collect::<Vec<_>>()
    });
    serde_json::json!([items, detail.omitted_items]).to_string()
}

fn iso_from_ms(ms: f64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

pub fn project_wait_items(detail: &WaitSourceItems) -> ProjectedWaitItems {
    ProjectedWaitItems {
        items: detail.items.as_ref().map(|items| {
            items
                .iter()
                .map(|item| LifecycleWaitItem {
                    id: item.id.clone(),
                    label: item.description.clone(),
                    started_at: item.started_at_ms.and_then(iso_from_ms),
                })
                .collect()
        }),
        omitted_items: detail.omitted_items.filter(|n| *n > 0),
    }
}

/// Reads projected items back (`label`/`startedAt`) through the same normalization.
pub fn read_wait_items(value: Option<&Value>, omitted: Option<&Value>) -> ProjectedWaitItems {
    let items = value.and_then(Value::as_array).map(|items| {
        Value::Array(
            items
                .iter()
                .map(|item| {
                    let Some(object) = item.as_object() else {
                        return item.clone();
                    };
                    let mut out = serde_json::Map::new();
                    if let Some(id) = object.get("id") {
                        out.insert("id".into(), id.clone());
                    }
                    if let Some(label) = object.get("label") {
                        out.insert("description".into(), label.clone());
                    }
                    if let Some(ms) = object
                        .get("startedAt")
                        .and_then(Value::as_str)
                        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                    {
                        out.insert(
                            "startedAtMs".into(),
                            serde_json::json!(ms.timestamp_millis()),
                        );
                    }
                    Value::Object(out)
                })
                .collect(),
        )
    });
    project_wait_items(&normalize_wait_items(items.as_ref(), omitted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn items_are_deduped_sorted_capped_and_counted() {
        let mut raw: Vec<Value> = (0..40).map(|i| json!({"id": format!("m{i:02}")})).collect();
        raw.push(json!({"id":"m00","description":"a"}));
        raw.push(json!({"id":"  "}));
        raw.push(json!("bad"));
        let detail = normalize_wait_items(Some(&json!(raw)), Some(&json!(2)));
        let items = detail.items.as_ref().expect("items");
        assert_eq!(items.len(), 32);
        assert_eq!(items[0].id, "m00");
        assert_eq!(detail.omitted_items, Some(8 + 2));
    }

    #[test]
    fn legacy_monitors_are_read_when_items_is_absent() {
        let data = json!({"monitors":[{"id":"x"}]});
        assert_eq!(wake_source_items(&data), Some(&json!([{"id":"x"}])));
    }

    #[test]
    fn projection_round_trips_labels_and_start_times() {
        let detail = normalize_wait_items(
            Some(&json!([{"id":"t","description":"build","startedAtMs":1_700_000_000_000_i64}])),
            None,
        );
        let projected = project_wait_items(&detail);
        let item = &projected.items.as_ref().expect("items")[0];
        assert_eq!(item.label.as_deref(), Some("build"));
        assert_eq!(item.started_at.as_deref(), Some("2023-11-14T22:13:20.000Z"));
        let read = read_wait_items(
            Some(&serde_json::to_value(projected.items.as_ref()).expect("json")),
            None,
        );
        assert_eq!(read, projected);
    }
}
