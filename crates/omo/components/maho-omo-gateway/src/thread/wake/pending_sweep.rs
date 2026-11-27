//! Pending intake sweep on startup/reconnect (`thread/wake/pending-sweep.ts`).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use super::super::intake_admission::IntakeAdmission;
use super::super::store_api::GatewayStoreApi;
use super::super::store_ops::StoreError;
use super::super::types::{DeliveryState, RefusalReason};
use super::closed_intake::settle_closed_intake;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

pub type ClosedAtFn = Arc<dyn Fn(&str) -> Option<i64> + Send + Sync>;

pub struct PendingSweepOptions {
    pub store: Arc<dyn GatewayStoreApi>,
    pub admit_intake: Arc<dyn Fn(String) -> BoxFuture<IntakeAdmission> + Send + Sync>,
    pub wake: Arc<dyn Fn(String) -> BoxFuture<()> + Send + Sync>,
    pub same_device: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    pub closed_at: ClosedAtFn,
}

pub async fn run_pending_sweep(options: &PendingSweepOptions) -> Result<(), StoreError> {
    let mut answers: HashMap<String, IntakeAdmission> = HashMap::new();
    let sessions = options.store.intake_open_sessions()?;
    for session in sessions {
        let closed_at = (options.closed_at)(&session.durable_id);
        if closed_at.is_some()
            && settle_closed_intake(&*options.store, &session.durable_id, closed_at)
                .await?
                .is_empty()
        {
            continue;
        }
        let mut wake = session.local && (options.same_device)(&session.durable_id);
        for device in &session.devices {
            let answer = if let Some(cached) = answers.get(device) {
                *cached
            } else {
                let fresh = (options.admit_intake)(device.clone()).await;
                answers.insert(device.clone(), fresh);
                fresh
            };
            if answer == IntakeAdmission::Admitted {
                wake = true;
            }
            if answer != IntakeAdmission::Refused {
                continue;
            }
            let prefix = format!("device:{device}/");
            for row in options.store.list(Some(&session.durable_id), None)? {
                if row.state == DeliveryState::Queued
                    && row
                        .ledger_id
                        .as_deref()
                        .is_some_and(|l| l.starts_with(&prefix))
                {
                    let _ = options.store.refuse_queued(
                        options.store.now(),
                        &row.delivery_id,
                        RefusalReason::SenderNotAdmitted,
                    )?;
                }
            }
        }
        if wake {
            (options.wake)(session.durable_id).await;
        }
    }
    Ok(())
}
