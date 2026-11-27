//! Port of senpi `packages/agent/src/durable/wake-host-store-alarm.ts`.

use std::sync::Arc;

use maho_ai::types::BoxFuture;

use super::store::{DurableStore, DurableStoreError, StoreValue, StoreWrite};

/// Where a timer host keeps one session's armed alarm, durably.
pub trait AlarmPersistence: Send + Sync {
    fn read(&self) -> BoxFuture<'_, Result<Option<i64>, DurableStoreError>>;
    /// Durably arm at `at`, or clear with `None`.
    fn write(&self, at: Option<i64>) -> BoxFuture<'_, Result<(), DurableStoreError>>;
}

pub const WAKE_ALARM_DOCUMENT: &str = "durable.wake.alarm";

struct StoreAlarmPersistence {
    store: Arc<dyn DurableStore>,
    epoch: u64,
}

impl AlarmPersistence for StoreAlarmPersistence {
    fn read(&self) -> BoxFuture<'_, Result<Option<i64>, DurableStoreError>> {
        Box::pin(async move {
            let doc = self.store.get_document(WAKE_ALARM_DOCUMENT).await?;
            Ok(doc.and_then(|value| value.as_i64().or_else(|| value.as_f64().map(|n| n as i64))))
        })
    }

    fn write(&self, at: Option<i64>) -> BoxFuture<'_, Result<(), DurableStoreError>> {
        Box::pin(async move {
            let write = match at {
                None => StoreWrite::Delete {
                    key: WAKE_ALARM_DOCUMENT.to_owned(),
                },
                Some(time) => StoreWrite::Put {
                    key: WAKE_ALARM_DOCUMENT.to_owned(),
                    data: StoreValue::from(time),
                },
            };
            self.store
                .commit(self.epoch, vec![write], Box::new(|_| {}))
                .await?;
            Ok(())
        })
    }
}

/// The alarm as a document in the session's own DurableStore, written under the owner `epoch`.
pub fn store_alarm_persistence(
    store: Arc<dyn DurableStore>,
    epoch: u64,
) -> Arc<dyn AlarmPersistence> {
    Arc::new(StoreAlarmPersistence { store, epoch })
}
