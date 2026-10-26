//! Port of `packages/omo-senpi/src/components/telemetry/session-summary-publisher.ts`.
//!
//! The publication protocol, in this order: (1) the exact event lands in `pending/<window_id>.json`,
//! (2) `advance` persists the next sequence, (3) the event is published, (4) the pending file is deleted only
//! on acknowledgment. A crash before (1) leaves the window for recovery; a crash after (1) leaves a pending
//! file that is re-sent with the same uuid, so the window is never published under two ids.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use futures::future::BoxFuture;
use telemetry_core::{
    TelemetryDeliveryError, TelemetryDiagnosticEvent, TelemetryDiagnostics, TelemetryError,
};

use crate::session_role::TelemetrySessionRole;
use crate::session_summary_delivery::{
    DeliveryReporter, DroppedCounter, SummaryPublication, send_summary_record,
};
use crate::session_summary_sampling::is_session_sampled;
use crate::session_summary_store::{
    ConsentScope, PendingRecord, RecordConsent, SummaryStorage, is_record_sendable,
};
use crate::session_summary_window::{
    EndedBy, SummaryMeta, SummaryWindow, summary_properties,
};

/// Resolves `Ok` only when the transport acknowledged the event (a 2xx answer). A rejection keeps the
/// pending file for a later retry, unless it is a permanent `TelemetryDeliveryError`.
pub type SummaryPublish = Arc<
    dyn Fn(SummaryPublication) -> BoxFuture<'static, Result<(), TelemetryDeliveryError>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone)]
pub struct ClosedWindow {
    pub consent: RecordConsent,
    pub scope: Option<ConsentScope>,
    pub session_hash: String,
    pub session_role: TelemetrySessionRole,
    pub sample_rate: f64,
    pub epoch: String,
    pub seq: u64,
    pub window_id: String,
    pub window: SummaryWindow,
    pub ended_by: EndedBy,
}

pub struct SummaryPublisher {
    storage: SummaryStorage,
    publish: SummaryPublish,
    diagnostics: Option<TelemetryDiagnostics>,
    transport_dropped: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
    is_scope_consented: Arc<dyn Fn(&ConsentScope) -> bool + Send + Sync>,
    in_flight: Mutex<HashSet<String>>,
    in_flight_count: tokio::sync::watch::Sender<usize>,
    dropped: Arc<AtomicU64>,
}

impl SummaryPublisher {
    pub fn new(
        storage: SummaryStorage,
        publish: SummaryPublish,
        diagnostics: Option<TelemetryDiagnostics>,
        transport_dropped: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
        is_scope_consented: Arc<dyn Fn(&ConsentScope) -> bool + Send + Sync>,
    ) -> Self {
        Self {
            storage,
            publish,
            diagnostics,
            transport_dropped,
            is_scope_consented,
            in_flight: Mutex::new(HashSet::new()),
            in_flight_count: tokio::sync::watch::channel(0).0,
            dropped: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn is_in_flight(&self, uuid: &str) -> bool {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(uuid)
    }

    pub fn count_dropped(&self, count: u64) {
        self.dropped.fetch_add(count, Ordering::SeqCst);
    }

    pub fn take_dropped(&self) -> u64 {
        let transport = self.transport_dropped.as_ref().map_or(0, |dropped| dropped());
        self.dropped.swap(0, Ordering::SeqCst) + transport
    }

    pub fn report(&self, event: TelemetryDiagnosticEvent, error: Option<TelemetryError>, count: u64) {
        let Some(diagnostics) = &self.diagnostics else {
            return;
        };
        diagnostics(&telemetry_core::TelemetryDiagnosticInput {
            event,
            source: "omo-native-session-summary".into(),
            error_kind: error
                .as_ref()
                .map(|_| telemetry_core::TelemetryDiagnosticErrorKind::Error),
            error,
            entry_kind: None,
            error_code: None,
            count: (count > 0).then_some(count),
        });
    }

    fn reporter(&self) -> DeliveryReporter {
        let diagnostics = self.diagnostics.clone();
        Arc::new(move |event, error, count| {
            let Some(diagnostics) = &diagnostics else {
                return;
            };
            diagnostics(&telemetry_core::TelemetryDiagnosticInput {
                event,
                source: "omo-native-session-summary".into(),
                error_kind: error
                    .as_ref()
                    .map(|_| telemetry_core::TelemetryDiagnosticErrorKind::Error),
                error,
                entry_kind: None,
                error_code: None,
                count: (count > 0).then_some(count),
            });
        })
    }

    fn counter(&self) -> DroppedCounter {
        let dropped = self.dropped.clone();
        Arc::new(move |count| {
            dropped.fetch_add(count, Ordering::SeqCst);
        })
    }

    fn set_in_flight(&self, uuid: &str, active: bool) -> bool {
        let mut in_flight = self.in_flight.lock().unwrap_or_else(PoisonError::into_inner);
        let changed = if active {
            in_flight.insert(uuid.to_string())
        } else {
            in_flight.remove(uuid)
        };
        let count = in_flight.len();
        drop(in_flight);
        self.in_flight_count.send_replace(count);
        changed
    }

    /// The one place an event leaves for the transport: a record without the consent mark, or whose
    /// originating scope has opted out since it was written, is deleted, never sent.
    pub async fn deliver(&self, record: &PendingRecord) {
        if !is_record_sendable(
            record.consent,
            record.scope.as_ref(),
            self.is_scope_consented.as_ref(),
        ) {
            self.storage.remove_pending(&record.uuid);
            return;
        }
        if !self.set_in_flight(&record.uuid, true) {
            return;
        }
        let reporter = self.reporter();
        let counter = self.counter();
        let _retired =
            send_summary_record(record, &self.storage, &self.publish, &reporter, &counter).await;
        self.set_in_flight(&record.uuid, false);
    }

    pub async fn publish_window(&self, closed: &ClosedWindow, advance: impl FnOnce()) {
        if closed.consent != RecordConsent::Enabled {
            return;
        }
        let meta = SummaryMeta {
            ended_by: closed.ended_by,
            epoch: closed.epoch.clone(),
            seq: closed.seq,
            sample_rate: closed.sample_rate,
            sampled: is_session_sampled(&closed.session_hash, closed.sample_rate),
            session_role: closed.session_role.as_str().to_string(),
            dropped_events: self.take_dropped(),
        };
        let properties = summary_properties(&closed.session_hash, &closed.window, &meta)
            .as_object()
            .cloned()
            .unwrap_or_default();
        let record = PendingRecord {
            consent: closed.consent,
            scope: closed.scope.clone(),
            uuid: closed.window_id.clone(),
            session_hash: closed.session_hash.clone(),
            owner: self.storage.owner.clone(),
            created_at: (self.storage.now)(),
            properties,
        };
        if let Err(error) = self.storage.write_pending(&record) {
            self.report(
                TelemetryDiagnosticEvent::TelemetryActivityStateWriteFailed,
                Some(TelemetryError::new(error.to_string())),
                0,
            );
        }
        advance();
        self.deliver(&record).await;
    }

    /// Resolves once every delivery started so far has been acknowledged or given up.
    pub async fn settled(&self) {
        let mut receiver = self.in_flight_count.subscribe();
        while *receiver.borrow_and_update() > 0 {
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_summary_store::Probes;
    use futures::FutureExt;
    use serde_json::json;
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex as StdMutex;

    fn storage(dir: &std::path::Path) -> SummaryStorage {
        crate::session_summary_store::create_summary_storage(
            dir,
            crate::session_summary_owner::ProcessOwner {
                pid: 1,
                boot_id: "b".into(),
            },
            Probes::default(),
            Some(Arc::new(|| 1000.0)),
        )
    }

    fn closed(scope_cwd: &str) -> ClosedWindow {
        ClosedWindow {
            consent: RecordConsent::Enabled,
            scope: Some(ConsentScope {
                cwd: Some(scope_cwd.into()),
                config: None,
            }),
            session_hash: "a".repeat(64),
            session_role: TelemetrySessionRole::Root,
            sample_rate: 0.05,
            epoch: "0123456789abcdef".into(),
            seq: 2,
            window_id: "01234567-89ab-cdef-0123-456789abcdef".into(),
            window: crate::session_summary_window::open_window(900.0, 1.0),
            ended_by: EndedBy::Checkpoint,
        }
    }

    fn publish(
        result: Result<(), TelemetryDeliveryError>,
        seen: Arc<StdMutex<Vec<SummaryPublication>>>,
    ) -> SummaryPublish {
        Arc::new(move |publication: SummaryPublication| {
            seen.lock().unwrap().push(publication);
            let result = result.clone();
            async move { result }.boxed() as BoxFuture<'static, _>
        })
    }

    #[tokio::test]
    async fn persistence_precedes_advancement_and_acknowledgment_retires_it() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage(dir.path());
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let publisher = SummaryPublisher::new(
            storage.clone(),
            publish(Ok(()), Arc::clone(&seen)),
            None,
            None,
            Arc::new(|_| true),
        );
        let advanced = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&advanced);
        let pending_before = Arc::new(StdMutex::new(0_usize));
        let observed = Arc::clone(&pending_before);
        let storage_probe = storage.clone();
        publisher
            .publish_window(&closed("/repo"), move || {
                *observed.lock().unwrap() = storage_probe.pending_uuids().len();
                flag.store(true, Ordering::SeqCst);
            })
            .await;
        assert!(advanced.load(Ordering::SeqCst));
        assert_eq!(*pending_before.lock().unwrap(), 1);
        assert!(storage.pending_uuids().is_empty());
        {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].uuid, "01234567-89ab-cdef-0123-456789abcdef");
        assert_eq!(seen[0].timestamp_ms, 1000.0);
        assert_eq!(seen[0].properties["summary_seq"], 2);
        assert_eq!(seen[0].properties["ended_by"], "checkpoint");
        assert_eq!(seen[0].properties["session_role"], "root");
        }
        publisher.settled().await;
    }

    #[tokio::test]
    async fn a_transient_rejection_keeps_the_pending_file() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage(dir.path());
        let publisher = SummaryPublisher::new(
            storage.clone(),
            publish(
                Err(TelemetryDeliveryError::new(Some(503), None)),
                Arc::new(StdMutex::new(Vec::new())),
            ),
            None,
            None,
            Arc::new(|_| true),
        );
        publisher.publish_window(&closed("/repo"), || {}).await;
        assert_eq!(
            storage.pending_uuids(),
            vec!["01234567-89ab-cdef-0123-456789abcdef".to_string()]
        );
        publisher.settled().await;
        assert!(!publisher.is_in_flight("01234567-89ab-cdef-0123-456789abcdef"));
    }

    #[tokio::test]
    async fn an_opted_out_scope_never_sends_and_drops_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage(dir.path());
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let publisher = SummaryPublisher::new(
            storage.clone(),
            publish(Ok(()), Arc::clone(&seen)),
            None,
            None,
            Arc::new(|scope| scope.cwd.as_deref() != Some("/opt/out")),
        );
        publisher.publish_window(&closed("/opt/out"), || {}).await;
        assert!(seen.lock().unwrap().is_empty());
        assert!(storage.pending_uuids().is_empty());
    }

    #[tokio::test]
    async fn unconsented_windows_are_never_published() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage(dir.path());
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let publisher = SummaryPublisher::new(
            storage.clone(),
            publish(Ok(()), Arc::clone(&seen)),
            None,
            None,
            Arc::new(|_| true),
        );
        let mut window = closed("/repo");
        window.consent = RecordConsent::Unconsented;
        publisher.publish_window(&window, || {}).await;
        assert!(seen.lock().unwrap().is_empty());
        assert!(storage.pending_uuids().is_empty());
    }

    #[tokio::test]
    async fn duplicate_delivery_of_one_uuid_is_deduplicated() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage(dir.path());
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let publisher = SummaryPublisher::new(
            storage.clone(),
            publish(Ok(()), Arc::clone(&seen)),
            None,
            None,
            Arc::new(|_| true),
        );
        publisher.publish_window(&closed("/repo"), || {}).await;
        let record = PendingRecord {
            consent: RecordConsent::Enabled,
            scope: Some(ConsentScope {
                cwd: Some("/repo".into()),
                config: None,
            }),
            uuid: "01234567-89ab-cdef-0123-456789abcdef".into(),
            session_hash: "a".repeat(64),
            owner: storage.owner.clone(),
            created_at: 1000.0,
            properties: serde_json::Map::from_iter([("turns".to_string(), json!(1))]),
        };
        publisher.deliver(&record).await;
        assert_eq!(seen.lock().unwrap().len(), 2);
        assert_eq!(seen.lock().unwrap()[1].properties["turns"], 1);
    }

    #[test]
    fn dropped_counters_accumulate_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let transport = Arc::new(AtomicU64::new(3));
        let observed = Arc::clone(&transport);
        let publisher = SummaryPublisher::new(
            storage(dir.path()),
            Arc::new(|_| -> BoxFuture<'static, Result<(), TelemetryDeliveryError>> {
                async { Ok(()) }.boxed()
            }),
            None,
            Some(Arc::new(move || observed.load(Ordering::SeqCst))),
            Arc::new(|_| true),
        );
        publisher.count_dropped(2);
        assert_eq!(publisher.take_dropped(), 5);
        assert_eq!(publisher.take_dropped(), 3);
    }
}
