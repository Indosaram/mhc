//! Worker subscription-limit mirror (senpi#3695, `session-worker-client.ts` / `session-worker.ts`): the host's
//! poller feeds only the host's store and a worker process reads its own copy, so the two stores mirror each other.
//! The host posts everything it holds with `prepare`, then each change as `subscription_limits { accounts }`; the
//! worker adopts them and mirrors its own turn writes back as `subscription_limits { limits }`, which the host
//! adopts. Every store applies the same total order (`SubscriptionLimitStore::adopt`), so the echo ends when a copy
//! comes straight back.
//!
//! Cross-lane: `maho_core::subscription_limits::limit_store::{MirroredLimits, SubscriptionLimitStore::{adopt,
//! mirror_snapshot, subscribe_mirror, unsubscribe_mirror}}` are the upstream `limit-store.ts` symbols (senpi
//! 06c6f35c53, d3f1260bdb) owned by the core lane; `MirroredLimits` carries the upstream camelCase wire shape.
use maho_core::subscription_limits::limit_store::{MirroredLimits, subscription_limits};
use serde_json::Value;

/// The host store's records with their ordering key, for `prepare.subscriptionLimits`.
pub fn mirror_snapshot_wire() -> Vec<Value> {
    subscription_limits()
        .mirror_snapshot()
        .iter()
        .filter_map(|limits| serde_json::to_value(limits).ok())
        .collect()
}

/// Adopts mirrored copies into this process's store; a malformed copy is reported and skipped.
pub fn adopt_wire(records: &[Value]) {
    for record in records {
        match serde_json::from_value::<MirroredLimits>(record.clone()) {
            Ok(limits) => subscription_limits().adopt(limits),
            Err(error) => {
                eprintln!("senpi subscription limits: a mirrored record is invalid: {error}")
            }
        }
    }
}

/// Unsubscribes its store mirror listener when dropped (senpi `stopLimitMirror`).
pub struct LimitMirror(u64);
impl Drop for LimitMirror {
    fn drop(&mut self) {
        subscription_limits().unsubscribe_mirror(self.0);
    }
}

/// Hears every change of this process's store with its ordering key; `forward` posts it to the other side. Listener
/// failures are contained by the store.
pub fn subscribe_mirror(forward: impl Fn(Value) + Send + Sync + 'static) -> LimitMirror {
    LimitMirror(subscription_limits().subscribe_mirror(std::sync::Arc::new(
        move |limits: &MirroredLimits| {
            if let Ok(value) = serde_json::to_value(limits) {
                forward(value);
            }
        },
    )))
}
