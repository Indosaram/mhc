//! Port of `packages/omo-senpi/src/components/telemetry/session-summary-delivery.ts`.
//!
//! Sends one pending event and retires its file on acknowledgment. A rejection keeps the file for a later
//! retry, unless it is a permanent `TelemetryDeliveryError` (the event itself was refused), which deletes it
//! and counts it as dropped.

use std::sync::Arc;

use serde_json::{Map, Value};
use telemetry_core::{TelemetryDiagnosticEvent, TelemetryError};

use crate::session_summary_publisher::SummaryPublish;
use crate::session_summary_store::{PendingRecord, SummaryStorage};

/// The one event shape that leaves for the transport: the window's close time is the same on every re-send.
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryPublication {
    pub uuid: String,
    pub properties: Map<String, Value>,
    pub timestamp_ms: f64,
}

impl SummaryPublication {
    pub fn of(record: &PendingRecord) -> Self {
        Self {
            uuid: record.uuid.clone(),
            properties: record.properties.clone(),
            timestamp_ms: record.created_at,
        }
    }
}

pub type DeliveryReporter = Arc<dyn Fn(TelemetryDiagnosticEvent, Option<TelemetryError>, u64) + Send + Sync>;
pub type DroppedCounter = Arc<dyn Fn(u64) + Send + Sync>;

/// Sends one record; `true` means the pending file was retired (acknowledged, or permanently refused).
pub async fn send_summary_record(
    record: &PendingRecord,
    storage: &SummaryStorage,
    publish: &SummaryPublish,
    report: &DeliveryReporter,
    count_dropped: &DroppedCounter,
) -> bool {
    let publication = SummaryPublication::of(record);
    match (publish)(publication).await {
        Ok(()) => {
            storage.remove_pending(&record.uuid);
            true
        }
        Err(error) => {
            report(
                TelemetryDiagnosticEvent::TelemetryCaptureFailed,
                Some(TelemetryError::new(error.to_string())),
                0,
            );
            if error.permanent {
                storage.remove_pending(&record.uuid);
                count_dropped(1);
                return true;
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_summary_store::{
        ConsentScope, Probes, RecordConsent, create_summary_storage,
    };
    use futures::FutureExt;
    use futures::future::BoxFuture;
    use serde_json::json;
    use telemetry_core::TelemetryDeliveryError;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    fn record(uuid: &str) -> PendingRecord {
        PendingRecord {
            consent: RecordConsent::Enabled,
            scope: Some(ConsentScope {
                cwd: Some("/repo".into()),
                config: None,
            }),
            uuid: uuid.into(),
            session_hash: "h".into(),
            owner: crate::session_summary_owner::ProcessOwner {
                pid: 1,
                boot_id: "b".into(),
            },
            created_at: 1000.0,
            properties: Map::from_iter([("turns".to_string(), json!(2))]),
        }
    }

    fn storage(dir: &std::path::Path) -> SummaryStorage {
        create_summary_storage(
            dir,
            crate::session_summary_owner::ProcessOwner {
                pid: 1,
                boot_id: "b".into(),
            },
            Probes::default(),
            Some(Arc::new(|| 1000.0)),
        )
    }

    fn publish(result: Result<(), TelemetryDeliveryError>) -> SummaryPublish {
        Arc::new(move |_: SummaryPublication| {
            let result = result.clone();
            async move { result }.boxed() as BoxFuture<'static, _>
        })
    }

    #[tokio::test]
    async fn acknowledged_delivery_retires_the_pending_file() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage(dir.path());
        let uuid = "01234567-89ab-cdef-0123-456789abcdef";
        storage.write_pending(&record(uuid)).unwrap();
        let dropped = Arc::new(AtomicU64::new(0));
        let observed = Arc::clone(&dropped);
        let report: DeliveryReporter = Arc::new(|_, _, _| {});
        let counter: DroppedCounter = Arc::new(move |count| { observed.fetch_add(count, Ordering::SeqCst); });
        let retired = send_summary_record(
            &record(uuid),
            &storage,
            &publish(Ok(())),
            &report,
            &counter,
        )
        .await;
        assert!(retired);
        assert!(storage.pending_uuids().is_empty());
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn transient_failure_keeps_the_file_and_permanent_refusal_counts_it() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage(dir.path());
        let uuid = "01234567-89ab-cdef-0123-456789abcdef";
        storage.write_pending(&record(uuid)).unwrap();
        let dropped = Arc::new(AtomicU64::new(0));
        let observed = Arc::clone(&dropped);
        let reports = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&reports);
        let report: DeliveryReporter = Arc::new(move |event, error, _| {
            recorded.lock().unwrap().push((event, error));
        });
        let counter: DroppedCounter = Arc::new(move |count| {
            observed.fetch_add(count, Ordering::SeqCst);
        });
        let transient = publish(Err(TelemetryDeliveryError::new(
            Some(500),
            Some(TelemetryError::new("server")),
        )));
        assert!(!send_summary_record(&record(uuid), &storage, &transient, &report, &counter).await);
        assert_eq!(storage.pending_uuids(), vec![uuid.to_string()]);
        assert_eq!(dropped.load(Ordering::SeqCst), 0);

        let permanent = publish(Err(TelemetryDeliveryError::new(Some(400), None)));
        assert!(send_summary_record(&record(uuid), &storage, &permanent, &report, &counter).await);
        assert!(storage.pending_uuids().is_empty());
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert_eq!(reports.lock().unwrap().len(), 2);
    }

    #[test]
    fn publication_carries_the_record_identity_and_close_time() {
        let record = record("01234567-89ab-cdef-0123-456789abcdef");
        let publication = SummaryPublication::of(&record);
        assert_eq!(publication.uuid, record.uuid);
        assert_eq!(publication.timestamp_ms, 1000.0);
        assert_eq!(publication.properties["turns"], 2);
    }
}
