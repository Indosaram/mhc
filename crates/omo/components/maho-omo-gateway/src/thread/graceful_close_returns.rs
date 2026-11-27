//! Clean-stop settlement on graceful close (`thread/gateway/graceful-close-returns.ts`).

use serde_json::Value;

use super::store_api::GatewayStoreApi;
use super::store_ops::StoreError;
use super::types::RefusalReason;

pub const SESSION_CLOSED_ENTRY: &str = "session-closed";

/// Delivery IDs returned by the newest `session-closed` transcript entry.
pub fn returned_at_close(entries: &[Value]) -> Vec<String> {
    for entry in entries.iter().rev() {
        let is_closed = entry.get("type").and_then(Value::as_str) == Some("custom")
            && entry.get("customType").and_then(Value::as_str) == Some(SESSION_CLOSED_ENTRY);
        if !is_closed {
            continue;
        }
        if let Some(deliveries) = entry
            .get("data")
            .and_then(|d| d.get("returned"))
            .and_then(|r| r.get("deliveries"))
            .and_then(Value::as_array)
        {
            return deliveries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
        }
    }
    Vec::new()
}

/// Settles deliveries handed back at graceful close and any earlier queued rows.
pub async fn settle_graceful_close(
    store: &dyn GatewayStoreApi,
    durable_id: &str,
    returned: &[String],
) -> Result<(), StoreError> {
    let now = store.now();
    let rows = store.list(Some(durable_id), None)?;
    for row in rows {
        let matched = returned
            .iter()
            .any(|id| id == &row.delivery_id || row.ledger_id.as_deref() == Some(id.as_str()));
        if matched {
            let _ = store.refuse_returned(now, &row.delivery_id, RefusalReason::TargetClosed)?;
        } else if row.state.as_str() == "queued" && row.created_at <= now {
            let _ = store.refuse_queued(now, &row.delivery_id, RefusalReason::TargetClosed)?;
        }
    }
    Ok(())
}
