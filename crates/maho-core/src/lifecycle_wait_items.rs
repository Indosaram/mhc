//! Port of senpi packages/coding-agent/src/core/lifecycle-wait-items.ts.
//!
//! Named wait items: optional producer detail beside a wait's count. It never determines custody; lists are
//! snapshots, bounded independently of `activeCount`.
use serde_json::{Map, Value, json};

const MAX_ITEMS: usize = 32;

/// senpi `WaitSourceItems`: `items` (`{id, description?, startedAtMs?}`) and `omittedItems`, each absent when empty.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WaitSourceItems {
    pub items: Option<Vec<Value>>,
    pub omitted_items: Option<u64>,
}

impl WaitSourceItems {
    /// Writes `items`/`omittedItems` into a JSON object, as upstream spreads them.
    pub fn spread_into(&self, target: &mut Map<String, Value>) {
        if let Some(items) = &self.items {
            target.insert("items".into(), Value::Array(items.clone()));
        }
        if let Some(omitted) = self.omitted_items {
            target.insert("omittedItems".into(), json!(omitted));
        }
    }
}

fn is_safe_positive_integer(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    let number = value.as_f64()?;
    (number.fract() == 0.0 && number > 0.0 && number <= 9_007_199_254_740_991.0)
        .then_some(number as u64)
}

/// senpi `normalizeWaitItems`: deduped by id (the smaller rendering wins), sorted by id, capped at 32 with the rest
/// counted in `omittedItems`.
pub fn normalize_wait_items(value: Option<&Value>, omitted: Option<&Value>) -> WaitSourceItems {
    let mut by_id: Vec<(String, Value)> = Vec::new();
    for entry in value.and_then(Value::as_array).into_iter().flatten() {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        if id.trim().is_empty() {
            continue;
        }
        let mut item = Map::new();
        item.insert("id".into(), json!(id));
        if let Some(description) = entry.get("description").and_then(Value::as_str) {
            item.insert("description".into(), json!(description));
        }
        if let Some(started) = entry.get("startedAtMs").and_then(Value::as_f64)
            && started.is_finite()
            && started.abs() <= 8.64e15
        {
            item.insert("startedAtMs".into(), entry["startedAtMs"].clone());
        }
        let item = Value::Object(item);
        match by_id.iter_mut().find(|(existing, _)| existing == id) {
            Some((_, previous)) => {
                if item.to_string() < previous.to_string() {
                    *previous = item;
                }
            }
            None => by_id.push((id.to_owned(), item)),
        }
    }
    by_id.sort_by(|(a, _), (b, _)| a.cmp(b));
    let omitted_items = by_id.len().saturating_sub(MAX_ITEMS) as u64
        + is_safe_positive_integer(omitted).unwrap_or(0);
    let items: Vec<Value> = by_id
        .into_iter()
        .take(MAX_ITEMS)
        .map(|(_, item)| item)
        .collect();
    WaitSourceItems {
        items: (!items.is_empty()).then_some(items),
        omitted_items: (omitted_items > 0).then_some(omitted_items),
    }
}

/// The one rule for a `wake_source_state` producer's item list: `items` when set, else the legacy `monitors`.
pub fn wake_source_items(data: &Value) -> Option<&Value> {
    data.get("items")
        .filter(|items| !items.is_null())
        .or_else(|| data.get("monitors").filter(|monitors| !monitors.is_null()))
}

/// senpi `waitItemSignature`: ids and descriptions plus the omitted count; a start-time change alone republishes nothing.
pub fn wait_item_signature(detail: &WaitSourceItems) -> String {
    let items = detail.items.as_ref().map(|items| {
        items
            .iter()
            .map(|item| {
                json!([
                    item["id"],
                    item.get("description").cloned().unwrap_or(Value::Null)
                ])
            })
            .collect::<Vec<_>>()
    });
    json!([items, detail.omitted_items]).to_string()
}

/// senpi `projectWaitItems`: the lifecycle shape (`{id, label?, startedAt?}`).
pub fn project_wait_items(detail: &WaitSourceItems) -> WaitSourceItems {
    WaitSourceItems {
        items: detail.items.as_ref().map(|items| {
            items
                .iter()
                .map(|item| {
                    let mut projected = Map::new();
                    projected.insert("id".into(), item["id"].clone());
                    if let Some(description) = item.get("description") {
                        projected.insert("label".into(), description.clone());
                    }
                    if let Some(started) = item.get("startedAtMs").and_then(Value::as_f64)
                        && let Some(at) =
                            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(started as i64)
                    {
                        projected.insert(
                            "startedAt".into(),
                            json!(at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()),
                        );
                    }
                    Value::Object(projected)
                })
                .collect()
        }),
        omitted_items: detail.omitted_items,
    }
}

/// senpi `readWaitItems`: reads a durable lifecycle item list back through the producer rules.
pub fn read_wait_items(value: Option<&Value>, omitted: Option<&Value>) -> WaitSourceItems {
    let items = value.and_then(Value::as_array).map(|items| {
        Value::Array(
            items
                .iter()
                .map(|item| {
                    let Some(object) = item.as_object() else {
                        return item.clone();
                    };
                    let mut producer = Map::new();
                    if let Some(id) = object.get("id") {
                        producer.insert("id".into(), id.clone());
                    }
                    if let Some(label) = object.get("label") {
                        producer.insert("description".into(), label.clone());
                    }
                    if let Some(at) = object.get("startedAt").and_then(Value::as_str)
                        && let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(at)
                    {
                        producer.insert("startedAtMs".into(), json!(parsed.timestamp_millis()));
                    }
                    Value::Object(producer)
                })
                .collect(),
        )
    });
    project_wait_items(&normalize_wait_items(items.as_ref(), omitted))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// senpi test/suite/regressions/4106-named-waits.test.ts: items dedupe, sort, cap and count the rest.
    #[test]
    fn items_are_normalized_bounded_and_projected() {
        let mut raw: Vec<Value> = (0..40)
            .map(|index| json!({"id": format!("m{index:02}")}))
            .collect();
        raw.push(json!({"id": "m00", "description": "watch"}));
        raw.push(json!({"id": " "}));
        let detail = normalize_wait_items(Some(&Value::Array(raw)), Some(&json!(3)));
        let items = detail.items.as_ref().expect("items");
        assert_eq!(items.len(), 32);
        assert_eq!(items[0]["id"], "m00");
        assert_eq!(detail.omitted_items, Some(8 + 3));
        let projected = project_wait_items(&normalize_wait_items(
            Some(&json!([{"id": "a", "description": "build", "startedAtMs": 0}])),
            None,
        ));
        assert_eq!(
            projected.items.expect("items")[0],
            json!({"id": "a", "label": "build", "startedAt": "1970-01-01T00:00:00.000Z"})
        );
        let read = read_wait_items(
            Some(&json!([{"id": "a", "label": "build", "startedAt": "1970-01-01T00:00:00.000Z"}])),
            None,
        );
        assert_eq!(read.items.expect("items")[0]["label"], "build");
        assert_eq!(
            wake_source_items(&json!({"monitors": [1]})),
            Some(&json!([1]))
        );
    }
}
