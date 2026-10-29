//! Port of `packages/omo-senpi/src/components/telemetry/session-summary-lazy-recovery.ts`.
//!
//! A directory holding only this process's own windows and in-flight events has nothing a sweep would touch,
//! so the common startup never builds the recovery side. The Rust host has no sidecar to load, so "lazy"
//! here means the recovery runtime is built on the first sweep that has work, and retried if building fails.

use std::sync::{Arc, Mutex, PoisonError};

use telemetry_core::TelemetryDiagnostics;

use crate::session_summary_files::list_names;
use crate::session_summary_publisher::SummaryPublisher;
use crate::session_summary_recovery::{RecoveryRuntime, create_recovery_runtime};
use crate::session_summary_store::{ConsentScope, SummaryStorage};

pub struct LazySummaryRecovery {
    storage: SummaryStorage,
    publisher: Arc<SummaryPublisher>,
    is_scope_consented: Arc<dyn Fn(&ConsentScope) -> bool + Send + Sync>,
    diagnostics: Option<TelemetryDiagnostics>,
    own_window_keys: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    runtime: Mutex<Option<Arc<RecoveryRuntime>>>,
}

impl LazySummaryRecovery {
    pub fn new(
        storage: SummaryStorage,
        publisher: Arc<SummaryPublisher>,
        is_scope_consented: Arc<dyn Fn(&ConsentScope) -> bool + Send + Sync>,
        diagnostics: Option<TelemetryDiagnostics>,
        own_window_keys: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        Self {
            storage,
            publisher,
            is_scope_consented,
            diagnostics,
            own_window_keys,
            runtime: Mutex::new(None),
        }
    }

    /// The loaded recovery side; `None` until the first sweep with work.
    pub fn runtime(&self) -> Option<Arc<RecoveryRuntime>> {
        self.runtime
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn load(&self) -> Arc<RecoveryRuntime> {
        let mut runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
        runtime.clone().unwrap_or_else(|| {
            let built = Arc::new(create_recovery_runtime(
                self.storage.clone(),
                Arc::clone(&self.publisher),
                self.diagnostics.clone(),
                Arc::clone(&self.is_scope_consented),
            ));
            *runtime = Some(Arc::clone(&built));
            built
        })
    }

    /// A temp or claim file left by any process, a pending event not in flight, or a window this process did
    /// not write.
    fn has_work(&self) -> bool {
        let mut names = list_names(&self.storage.dir);
        names.extend(list_names(&self.storage.dir.join("pending")));
        if names
            .iter()
            .any(|name| name.ends_with(".tmp") || name.contains(".claimed-"))
        {
            return true;
        }
        if self
            .storage
            .pending_uuids()
            .iter()
            .any(|uuid| !self.publisher.is_in_flight(uuid))
        {
            return true;
        }
        let own: std::collections::BTreeSet<String> = (self.own_window_keys)().into_iter().collect();
        self.storage
            .window_keys()
            .into_iter()
            .any(|key| !own.contains(&key))
    }

    pub async fn sweep(&self) {
        if let Some(runtime) = self.runtime() {
            runtime.recovery.sweep().await;
            return;
        }
        if !self.has_work() {
            return;
        }
        let runtime = self.load();
        runtime.recovery.sweep().await;
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_summary_store::{Probes, RecordConsent, WindowRecord, create_summary_storage};
    use futures::FutureExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fixture() -> (tempfile::TempDir, SummaryStorage, Arc<SummaryPublisher>) {
        let dir = tempfile::tempdir().unwrap();
        let storage = create_summary_storage(
            dir.path(),
            crate::session_summary_owner::ProcessOwner {
                pid: 1,
                boot_id: "boot-self".into(),
            },
            Probes {
                is_pid_alive: Some(Arc::new(|_| false)),
                process_start_time: Some(Arc::new(|_| None)),
            },
            Some(Arc::new(|| 1000.0)),
        );
        let publisher = Arc::new(SummaryPublisher::new(
            storage.clone(),
            Arc::new(|_| -> futures::future::BoxFuture<'static, Result<(), telemetry_core::TelemetryDeliveryError>> {
                async { Ok(()) }.boxed()
            }),
            None,
            None,
            Arc::new(|_| true),
        ));
        (dir, storage, publisher)
    }

    fn abandoned_record() -> WindowRecord {
        WindowRecord {
            consent: RecordConsent::Enabled,
            scope: Some(ConsentScope {
                cwd: Some("/repo".into()),
                config: None,
            }),
            session_hash: "a".repeat(64),
            session_role: crate::session_role::TelemetrySessionRole::Root,
            owner: crate::session_summary_owner::ProcessOwner {
                pid: 9,
                boot_id: "boot-peer".into(),
            },
            heartbeat_at: -1_000_000.0,
            epoch: Some("0123456789abcdef".into()),
            seq: 0,
            sample_rate: 0.05,
            window_id: None,
            window: None,
        }
    }

    #[tokio::test]
    async fn a_quiet_directory_loads_nothing() {
        let (_dir, storage, publisher) = fixture();
        let own: Arc<dyn Fn() -> Vec<String> + Send + Sync> = Arc::new(Vec::new);
        let recovery = LazySummaryRecovery::new(
            storage.clone(),
            publisher,
            Arc::new(|_| true),
            None,
            own,
        );
        recovery.sweep().await;
        assert!(recovery.runtime().is_none());
    }

    #[tokio::test]
    async fn a_foreign_window_loads_and_sweeps_once() {
        let (dir, storage, publisher) = fixture();
        // Match session-summary-lazy-recovery.test.ts: recovery starts from an open orphan window.
        let mut record = abandoned_record();
        record.window_id = Some("01234567-89ab-cdef-0123-456789abcdef".into());
        record.window = Some(crate::session_summary_window::open_window(-1_000_000.0, 0.0));
        storage.write_window("peer", &record).unwrap();
        let loads = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&loads);
        let recovery = LazySummaryRecovery::new(
            storage.clone(),
            publisher,
            Arc::new(|_| true),
            None,
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Vec::new()
            }),
        );
        recovery.sweep().await;
        assert!(recovery.runtime().is_some());
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        match storage.read_window("peer") {
            crate::session_summary_store::ReadResult::Value(continued) => {
                assert_eq!(continued.seq, 1);
                assert_eq!(continued.owner.pid, 1);
                assert!(continued.window.is_none());
            }
            other => panic!("expected a continued window, got {other:?}"),
        }
        recovery.sweep().await;
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        let _ = dir;
    }

    #[tokio::test]
    async fn pending_and_leftover_work_trigger_a_sweep() {
        let (dir, storage, publisher) = fixture();
        std::fs::create_dir_all(dir.path().join("session-summary")).unwrap();
        // A live owner's temp must survive; use an explicitly dead foreign owner here.
        std::fs::write(dir.path().join("session-summary/x.json.9-boot-peer.tmp"), "{}").unwrap();
        let recovery = LazySummaryRecovery::new(
            storage.clone(),
            publisher,
            Arc::new(|_| true),
            None,
            Arc::new(Vec::<String>::new),
        );
        recovery.sweep().await;
        assert!(recovery.runtime().is_some());
        assert!(!dir.path().join("session-summary/x.json.9-boot-peer.tmp").exists());
    }

    #[tokio::test]
    async fn own_windows_alone_are_not_work() {
        let (_dir, storage, publisher) = fixture();
        storage.write_window("mine", &abandoned_record()).unwrap();
        let recovery = LazySummaryRecovery::new(
            storage,
            publisher,
            Arc::new(|_| true),
            None,
            Arc::new(|| vec!["mine".to_string()]),
        );
        recovery.sweep().await;
        assert!(recovery.runtime().is_none());
    }
}
