//! Settle queued intake before a close on request (`thread/wake/closed-intake.ts`).

use super::super::store_api::GatewayStoreApi;
use super::super::store_ops::StoreError;
use super::super::types::{DeliveryRow, DeliveryState, RefusalReason};

/// Settles queued rows created at or before `closed_at` as `target_closed`, leaving newer queued rows.
pub async fn settle_closed_intake(
    store: &dyn GatewayStoreApi,
    durable_id: &str,
    closed_at: Option<i64>,
) -> Result<Vec<DeliveryRow>, StoreError> {
    let queued: Vec<DeliveryRow> = store
        .list(Some(durable_id), None)?
        .into_iter()
        .filter(|row| row.state == DeliveryState::Queued)
        .collect();
    let Some(cutoff) = closed_at else {
        return Ok(queued);
    };
    let mut remaining = Vec::new();
    for row in queued {
        if row.created_at > cutoff {
            remaining.push(row);
        } else {
            let _ =
                store.refuse_queued(store.now(), &row.delivery_id, RefusalReason::TargetClosed)?;
        }
    }
    Ok(remaining)
}
