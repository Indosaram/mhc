//! Port of `packages/omo-senpi/src/components/telemetry/session-summary-recovery.ts`.
//!
//! One pass over the summary directory re-sends unacknowledged events and recovers abandoned windows; safe to
//! run concurrently from any number of processes, because every hand-over is an atomic rename.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use telemetry_core::{TelemetryDiagnosticEvent, TelemetryDiagnostics, TelemetryError};

use crate::session_summary_adopt::{AdoptForeignWindowInput, adopt_foreign_window};
use crate::session_summary_files::{remove_file, rename_if_present};
use crate::session_summary_owner::{ProcessOwner, current_process_owner};
use crate::session_summary_publisher::{ClosedWindow, SummaryPublisher};
use crate::session_summary_store::{
    ConsentScope, LeftoverKind, OWNER_STALE_MS, PendingRecord, Probes, ReadResult,
    RecoveryStorage, SUMMARY_TTL_MS, SummaryStorage, WindowRecord, create_summary_storage,
    is_record_sendable, with_recovery_ops,
};
use crate::session_summary_window::{EndedBy, new_summary_epoch};

/// At most this many events wait for delivery; past it the oldest are dropped and counted in `dropped_events`.
pub const PENDING_CAP: usize = 200;
/// And at most this many bytes of them.
pub const PENDING_CAP_BYTES: u64 = 1024 * 1024;
/// A sweep starts at most this many re-sends; the rest wait for a later sweep.
pub const RESENDS_PER_SWEEP: usize = 20;
/// Re-sends of one event back off from one minute, doubling, up to an hour between attempts.
pub const RESEND_BACKOFF_FIRST_MS: f64 = 60_000.0;
pub const RESEND_BACKOFF_MAX_MS: f64 = 60.0 * 60_000.0;

pub struct SummaryRecovery {
    pub storage: RecoveryStorage,
    pub publisher: Arc<SummaryPublisher>,
    pub diagnostics: Option<TelemetryDiagnostics>,
    pub is_scope_consented: Arc<dyn Fn(&ConsentScope) -> bool + Send + Sync>,
    alive: Arc<dyn Fn(&ProcessOwner) -> bool + Send + Sync>,
    backoff: Mutex<BTreeMap<String, (f64, f64)>>,
}

impl SummaryRecovery {
    pub fn new(
        storage: RecoveryStorage,
        publisher: Arc<SummaryPublisher>,
        diagnostics: Option<TelemetryDiagnostics>,
        is_scope_consented: Arc<dyn Fn(&ConsentScope) -> bool + Send + Sync>,
    ) -> Self {
        let alive = storage.is_owner_alive();
        Self {
            storage,
            publisher,
            diagnostics,
            is_scope_consented,
            alive,
            backoff: Mutex::new(BTreeMap::new()),
        }
    }

    fn now(&self) -> f64 {
        (self.storage.storage.now)()
    }

    fn report_corrupt(&self, what: &str) {
        let Some(diagnostics) = &self.diagnostics else {
            return;
        };
        diagnostics(&telemetry_core::TelemetryDiagnosticInput {
            event: TelemetryDiagnosticEvent::TelemetryActivityStateReadFailed,
            source: "omo-native-session-summary".into(),
            error: Some(TelemetryError::new(format!(
                "unreadable session summary {what} was removed"
            ))),
            error_kind: Some(telemetry_core::TelemetryDiagnosticErrorKind::Error),
            entry_kind: None,
            error_code: None,
            count: None,
        });
    }

    fn sendable(&self, record: &WindowRecord) -> bool {
        is_record_sendable(record.consent, record.scope.as_ref(), self.is_scope_consented.as_ref())
    }

    fn pending_sendable(&self, record: &PendingRecord) -> bool {
        is_record_sendable(record.consent, record.scope.as_ref(), self.is_scope_consented.as_ref())
    }

    fn is_self(&self, owner: &ProcessOwner) -> bool {
        owner.pid == self.storage.storage.owner.pid
            && owner.boot_id == self.storage.storage.owner.boot_id
    }

    fn is_expired(&self, at: f64) -> bool {
        self.now() - at > SUMMARY_TTL_MS
    }

    /// Claimable only when BOTH hold: the owner is gone (dead pid, or a pid now held by another process) and
    /// its heartbeat is older than `OWNER_STALE_MS`.
    fn is_abandoned(&self, record: &WindowRecord) -> bool {
        self.now() - record.heartbeat_at > OWNER_STALE_MS && !(self.alive)(&record.owner)
    }

    async fn finish_claimed_window(
        &self,
        key: &str,
        claimed: crate::session_summary_store::Claimed<WindowRecord>,
    ) -> Option<u64> {
        let record = match claimed.record {
            ReadResult::Value(record) => record,
            ReadResult::Missing => {
                remove_file(&claimed.claim_path);
                return None;
            }
            ReadResult::Corrupt => {
                remove_file(&claimed.claim_path);
                self.report_corrupt("window");
                return None;
            }
        };
        // No consent mark, or its project opted out since it was written: dropped unsent (fail closed).
        if !self.sendable(&record) {
            remove_file(&claimed.claim_path);
            return None;
        }
        let next_seq = if record.window.is_none() {
            record.seq
        } else {
            record.seq + 1
        };
        // Published and continued under a new epoch: an owner misjudged dead that still publishes never
        // repeats a pair.
        let epoch = new_summary_epoch();
        let keep_numbering = || {
            // A dead process's own copy is never continued (its key names that process), so it is not
            // recreated.
            if !self.storage.is_own_copy_key(key) {
                let continued = WindowRecord {
                    owner: self.storage.storage.owner.clone(),
                    heartbeat_at: self.now(),
                    epoch: Some(epoch.clone()),
                    seq: next_seq,
                    window_id: None,
                    window: None,
                    ..record.clone()
                };
                let _created = self.storage.create_window(key, &continued);
            }
            remove_file(&claimed.claim_path);
        };
        // The pending file of this window exists when its owner died after step (1) of the publication: the
        // pending copy is the one that gets re-sent, under the same uuid, so no second window is created.
        let published_pending = record
            .window_id
            .as_deref()
            .is_some_and(|window_id| self.storage.has_pending(window_id));
        if record.window.is_none() || record.window_id.is_none() || published_pending {
            keep_numbering();
            return Some(next_seq);
        }
        let closed = ClosedWindow {
            consent: record.consent,
            scope: record.scope.clone(),
            session_hash: record.session_hash.clone(),
            session_role: record.session_role,
            sample_rate: record.sample_rate,
            epoch: epoch.clone(),
            seq: record.seq,
            window_id: record.window_id.clone().unwrap_or_default(),
            window: record.window.clone().unwrap_or_else(|| {
                crate::session_summary_window::open_window(self.now(), 0.0)
            }),
            ended_by: EndedBy::Recovered,
        };
        self.publisher.publish_window(&closed, keep_numbering).await;
        Some(next_seq)
    }

    pub async fn recover_window(&self, key: &str) -> Option<u64> {
        let claimed = self.storage.claim_window(key)?;
        self.finish_claimed_window(key, claimed).await
    }

    async fn resend_claimed_pending(
        &self,
        claimed: crate::session_summary_store::Claimed<PendingRecord>,
    ) {
        let record = match claimed.record {
            ReadResult::Value(record) => record,
            ReadResult::Missing => {
                remove_file(&claimed.claim_path);
                return;
            }
            ReadResult::Corrupt => {
                remove_file(&claimed.claim_path);
                self.report_corrupt("pending event");
                return;
            }
        };
        if !self.pending_sendable(&record) {
            remove_file(&claimed.claim_path);
            return;
        }
        // Re-owned before sending: if this process dies too, the next one resends it; while it lives,
        // nobody does.
        let reowned = PendingRecord {
            owner: self.storage.storage.owner.clone(),
            ..record.clone()
        };
        let _written = self.storage.storage.write_pending(&reowned);
        remove_file(&claimed.claim_path);
        self.publisher.deliver(&record).await;
    }

    fn sweep_leftovers(&self) {
        for leftover in self.storage.leftovers() {
            if (self.alive)(&leftover.owner) {
                continue;
            }
            if leftover.kind == LeftoverKind::Temp {
                remove_file(&leftover.path);
                continue;
            }
            let base = leftover
                .path
                .to_string_lossy()
                .rsplit_once(".claimed-")
                .map(|(base, _)| std::path::PathBuf::from(base));
            match base {
                Some(base) if rename_if_present(&leftover.path, &base) => {}
                _ => remove_file(&leftover.path),
            }
        }
    }

    fn may_resend(&self, uuid: &str) -> bool {
        let backoff = self.backoff.lock().unwrap_or_else(PoisonError::into_inner);
        match backoff.get(uuid) {
            None => true,
            Some((at, _)) => self.now() >= *at,
        }
    }

    fn resend_queued(&self, uuid: &str) {
        let mut backoff = self.backoff.lock().unwrap_or_else(PoisonError::into_inner);
        let delay = (backoff
            .get(uuid)
            .map_or(RESEND_BACKOFF_FIRST_MS / 2.0, |(_, delay)| *delay)
            * 2.0)
            .min(RESEND_BACKOFF_MAX_MS);
        backoff.insert(uuid.to_string(), (self.now() + delay, delay));
    }

    /// Oldest first, past either cap: a long outage cannot grow the spool without bound.
    fn drop_over_cap(&self, uuids: &[String]) -> BTreeSet<String> {
        let mut aged: Vec<(String, Option<PendingRecord>, f64, u64)> = uuids
            .iter()
            .map(|uuid| {
                let record = match self.storage.read_pending(uuid) {
                    ReadResult::Value(record) => Some(record),
                    ReadResult::Missing | ReadResult::Corrupt => None,
                };
                let created_at = record.as_ref().map_or(0.0, |record| record.created_at);
                (
                    uuid.clone(),
                    record,
                    created_at,
                    self.storage.storage.pending_bytes(uuid),
                )
            })
            .collect();
        aged.sort_by(|left, right| left.2.total_cmp(&right.2));
        let mut files = aged.len();
        let mut bytes: u64 = aged.iter().map(|entry| entry.3).sum();
        let mut dropped = BTreeSet::new();
        for (uuid, record, _, entry_bytes) in aged {
            if files <= PENDING_CAP && bytes <= PENDING_CAP_BYTES {
                break;
            }
            if self.publisher.is_in_flight(&uuid) {
                continue;
            }
            self.storage.storage.remove_pending(&uuid);
            self.backoff
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&uuid);
            dropped.insert(uuid);
            files = files.saturating_sub(1);
            bytes = bytes.saturating_sub(entry_bytes);
            if record.is_some_and(|record| self.pending_sendable(&record)) {
                self.publisher.count_dropped(1);
            }
        }
        dropped
    }

    async fn sweep_pending(&self) {
        let uuids = self.storage.storage.pending_uuids();
        let dropped = self.drop_over_cap(&uuids);
        let mut resends = 0_usize;
        for uuid in uuids {
            if dropped.contains(&uuid) {
                continue;
            }
            let record = match self.storage.read_pending(&uuid) {
                ReadResult::Missing => continue,
                ReadResult::Corrupt => ReadResult::Corrupt,
                ReadResult::Value(record) => ReadResult::Value(record),
            };
            if self.publisher.is_in_flight(&uuid) {
                continue;
            }
            if let ReadResult::Value(record) = &record {
                // Written by an opted-out session, or before consent marks existed: deleted unsent, never
                // counted.
                if record.consent != crate::session_summary_store::RecordConsent::Enabled {
                    self.storage.storage.remove_pending(&uuid);
                    continue;
                }
                if self.is_expired(record.created_at) {
                    self.storage.storage.remove_pending(&uuid);
                    if self.pending_sendable(record) {
                        self.publisher.count_dropped(1);
                    }
                    continue;
                }
                if !self.is_self(&record.owner) && (self.alive)(&record.owner) {
                    continue;
                }
            }
            if resends >= RESENDS_PER_SWEEP || !self.may_resend(&uuid) {
                continue;
            }
            resends += 1;
            self.resend_queued(&uuid);
            match record {
                ReadResult::Value(record) if self.is_self(&record.owner) => {
                    self.publisher.deliver(&record).await;
                }
                _ => {
                    if let Some(claimed) = self.storage.claim_pending(&uuid) {
                        self.resend_claimed_pending(claimed).await;
                    }
                }
            }
        }
    }

    async fn sweep_windows(&self) {
        for key in self.storage.storage.window_keys() {
            match self.storage.storage.read_window(&key) {
                ReadResult::Missing => continue,
                ReadResult::Corrupt => {
                    if let Some(claimed) = self.storage.claim_window(&key) {
                        let _ignored = self.finish_claimed_window(&key, claimed).await;
                    }
                    continue;
                }
                ReadResult::Value(record) => {
                    if record.consent != crate::session_summary_store::RecordConsent::Enabled {
                        self.storage.storage.remove_window(&key);
                        continue;
                    }
                    if self.is_expired(record.heartbeat_at) {
                        self.storage.storage.remove_window(&key);
                        // An opted-out scope's lost window is not counted either: the count would describe
                        // that scope.
                        if record.window.is_some() && self.sendable(&record) {
                            self.publisher.count_dropped(1);
                        }
                        continue;
                    }
                    if record.window.is_some() && self.is_abandoned(&record) {
                        let _ignored = self.recover_window(&key).await;
                    }
                }
            }
        }
    }

    /// One pass over the summary directory; safe to run concurrently from any number of processes.
    pub async fn sweep(&self) {
        self.sweep_leftovers();
        self.sweep_windows().await;
        self.sweep_pending().await;
    }

    pub fn adopt(
        &self,
        key: &str,
        own_copy: &str,
        stored: &WindowRecord,
        is_scope_consented: &(dyn Fn(&ConsentScope) -> bool + Send + Sync),
        fresh: &dyn Fn(crate::session_summary_adopt::FreshArgs) -> crate::omo_native_session_summary::LiveWindow,
        persist: &dyn Fn(&crate::omo_native_session_summary::LiveWindow),
    ) -> crate::omo_native_session_summary::LiveWindow {
        adopt_foreign_window(AdoptForeignWindowInput {
            storage: &self.storage,
            key,
            own_copy,
            stored,
            is_scope_consented,
            fresh,
            persist,
        })
    }
}

/// Builds the storage a purge needs; the entry path never carries it.
pub fn purge_opted_out_storage(
    state_dir: &std::path::Path,
    is_scope_consented: Arc<dyn Fn(&ConsentScope) -> bool + Send + Sync>,
    is_pid_alive: Option<crate::session_summary_liveness::PidProbe>,
) {
    let storage = create_summary_storage(
        state_dir,
        current_process_owner(),
        Probes {
            is_pid_alive,
            process_start_time: None,
        },
        None,
    );
    crate::session_summary_optout_purge::purge_opted_out_queue(
        &with_recovery_ops(storage),
        is_scope_consented.as_ref(),
    );
}

/// The recorder's whole recovery side, built once on first use.
pub struct RecoveryRuntime {
    pub recovery: Arc<SummaryRecovery>,
}

pub fn create_recovery_runtime(
    storage: SummaryStorage,
    publisher: Arc<SummaryPublisher>,
    diagnostics: Option<TelemetryDiagnostics>,
    is_scope_consented: Arc<dyn Fn(&ConsentScope) -> bool + Send + Sync>,
) -> RecoveryRuntime {
    RecoveryRuntime {
        recovery: Arc::new(SummaryRecovery::new(
            with_recovery_ops(storage),
            publisher,
            diagnostics,
            is_scope_consented,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_summary_publisher::SummaryPublish;
    use crate::session_summary_store::{RecordConsent, PendingRecord};
    use crate::session_summary_window::{open_window, summary_properties};
    use futures::future::BoxFuture;
    use futures::FutureExt;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    fn owner(pid: u32, boot_id: &str) -> ProcessOwner {
        ProcessOwner {
            pid,
            boot_id: boot_id.into(),
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        storage: SummaryStorage,
        alive: Arc<AtomicBool>,
        sent: Arc<Mutex<Vec<String>>>,
        published: Arc<AtomicU64>,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let alive = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&alive);
        let storage = create_summary_storage(
            dir.path(),
            owner(1, "boot-self"),
            Probes {
                is_pid_alive: Some(Arc::new(move |_| observed.load(Ordering::SeqCst))),
                process_start_time: Some(Arc::new(|_| None)),
            },
            Some(Arc::new(|| 1000.0)),
        );
        Fixture {
            dir,
            storage,
            alive,
            sent: Arc::new(Mutex::new(Vec::new())),
            published: Arc::new(AtomicU64::new(0)),
        }
    }

    fn publisher(fixture: &Fixture) -> Arc<SummaryPublisher> {
        let sent = Arc::clone(&fixture.sent);
        let published = Arc::clone(&fixture.published);
        let publish: SummaryPublish = Arc::new(move |publication| {
            sent.lock().unwrap().push(publication.uuid.clone());
            published.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }.boxed() as BoxFuture<'static, _>
        });
        Arc::new(SummaryPublisher::new(
            fixture.storage.clone(),
            publish,
            None,
            None,
            Arc::new(|_| true),
        ))
    }

    /// Sixteen minutes before the fixture clock, past `OWNER_STALE_MS` but far inside `SUMMARY_TTL_MS`.
    const STALE_HEARTBEAT: f64 = -1_000_000.0;

    fn record(consent: RecordConsent, pid: u32, boot_id: &str, heartbeat: f64) -> WindowRecord {
        WindowRecord {
            consent,
            scope: Some(ConsentScope {
                cwd: Some("/repo".into()),
                config: None,
            }),
            session_hash: "a".repeat(64),
            session_role: crate::session_role::TelemetrySessionRole::Root,
            owner: owner(pid, boot_id),
            heartbeat_at: heartbeat,
            epoch: Some("0123456789abcdef".into()),
            seq: 0,
            sample_rate: 0.05,
            window_id: Some("01234567-89ab-cdef-0123-456789abcdef".into()),
            window: Some(open_window(0.0, 1.0)),
        }
    }

    fn recovery(fixture: &Fixture) -> SummaryRecovery {
        SummaryRecovery::new(
            with_recovery_ops(fixture.storage.clone()),
            publisher(fixture),
            None,
            Arc::new(|_| true),
        )
    }

    #[tokio::test]
    async fn an_abandoned_window_is_published_as_recovered_and_continued() {
        let fixture = fixture();
        fixture
            .storage
            .write_window("session", &record(RecordConsent::Enabled, 2, "boot-peer", STALE_HEARTBEAT))
            .unwrap();
        let recovery = recovery(&fixture);
        recovery.sweep().await;
        assert_eq!(fixture.published.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture.sent.lock().unwrap().as_slice(),
            ["01234567-89ab-cdef-0123-456789abcdef"]
        );
        let continued = fixture.storage.read_window("session");
        match continued {
            ReadResult::Value(continued) => {
                assert_eq!(continued.seq, 1);
                assert_eq!(continued.owner, owner(1, "boot-self"));
                assert!(continued.window.is_none());
                assert_ne!(continued.epoch.as_deref(), Some("0123456789abcdef"));
            }
            other => panic!("expected a continued window, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_live_owners_window_is_left_alone() {
        let fixture = fixture();
        fixture.alive.store(true, Ordering::SeqCst);
        let path = fixture.dir.path().join("session-summary/owners/2.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            json!({"pid": 2, "bootId": "boot-peer", "startTime": null, "heartbeatAt": 0.0}).to_string(),
        )
        .unwrap();
        fixture
            .storage
            .write_window("session", &record(RecordConsent::Enabled, 2, "boot-peer", STALE_HEARTBEAT))
            .unwrap();
        recovery(&fixture).sweep().await;
        assert_eq!(fixture.published.load(Ordering::SeqCst), 0);
        assert!(matches!(
            fixture.storage.read_window("session"),
            ReadResult::Value(_)
        ));
    }

    #[tokio::test]
    async fn a_pending_window_is_not_published_twice() {
        let fixture = fixture();
        let window = record(RecordConsent::Enabled, 2, "boot-peer", STALE_HEARTBEAT);
        fixture.storage.write_window("session", &window).unwrap();
        fixture
            .storage
            .write_pending(&PendingRecord {
                consent: RecordConsent::Enabled,
                scope: window.scope.clone(),
                uuid: "01234567-89ab-cdef-0123-456789abcdef".into(),
                session_hash: window.session_hash.clone(),
                owner: owner(2, "boot-peer"),
                created_at: 0.0,
                properties: serde_json::Map::new(),
            })
            .unwrap();
        let recovery = recovery(&fixture);
        recovery.sweep().await;
        assert_eq!(fixture.published.load(Ordering::SeqCst), 1);
        assert!(matches!(
            fixture.storage.read_window("session"),
            ReadResult::Value(record) if record.seq == 1 && record.window.is_none()
        ));
    }

    #[tokio::test]
    async fn opted_out_and_corrupt_windows_are_dropped_without_sending() {
        let fixture = fixture();
        fixture
            .storage
            .write_window(
                "opted-out",
                &record(RecordConsent::Unconsented, 2, "boot-peer", STALE_HEARTBEAT),
            )
            .unwrap();
        std::fs::write(
            fixture.dir.path().join("session-summary/corrupt.json"),
            "{not json",
        )
        .unwrap();
        recovery(&fixture).sweep().await;
        assert_eq!(fixture.published.load(Ordering::SeqCst), 0);
        assert!(fixture.storage.window_keys().is_empty());
    }

    #[tokio::test]
    async fn expired_windows_are_removed_and_counted() {
        let fixture = fixture();
        fixture
            .storage
            .write_window(
                "stale",
                &record(RecordConsent::Enabled, 1, "boot-self", -2.0 * SUMMARY_TTL_MS),
            )
            .unwrap();
        let recovery = recovery(&fixture);
        recovery.sweep().await;
        assert!(fixture.storage.window_keys().is_empty());
        assert_eq!(recovery.publisher.take_dropped(), 1);
    }

    #[tokio::test]
    async fn a_dead_owners_pending_event_is_claimed_resent_and_retired() {
        let fixture = fixture();
        let uuid = "01234567-89ab-cdef-0123-456789abcdef";
        let mut properties = summary_properties(
            &"a".repeat(64),
            &open_window(0.0, 1.0),
            &crate::session_summary_window::SummaryMeta {
                ended_by: EndedBy::Checkpoint,
                epoch: "0123456789abcdef".into(),
                seq: 0,
                sample_rate: 0.05,
                sampled: false,
                session_role: "root".into(),
                dropped_events: 0,
            },
        );
        properties
            .as_object_mut()
            .unwrap()
            .retain(|key, _| key == "turns");
        fixture
            .storage
            .write_pending(&PendingRecord {
                consent: RecordConsent::Enabled,
                scope: Some(ConsentScope {
                    cwd: Some("/repo".into()),
                    config: None,
                }),
                uuid: uuid.into(),
                session_hash: "a".repeat(64),
                owner: owner(2, "boot-peer"),
                created_at: 1000.0,
                properties: properties.as_object().cloned().unwrap_or_default(),
            })
            .unwrap();
        recovery(&fixture).sweep().await;
        assert_eq!(fixture.sent.lock().unwrap().as_slice(), [uuid]);
        assert!(fixture.storage.pending_uuids().is_empty());
    }

    #[tokio::test]
    async fn leftovers_of_dead_owners_are_removed_and_live_ones_restored() {
        let fixture = fixture();
        let dir = fixture.dir.path().join("session-summary");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("w.json.2-boot-peer.tmp"), "{}").unwrap();
        std::fs::write(dir.join("w.json.claimed-2-boot-peer"), "{}").unwrap();
        std::fs::write(dir.join("kept.json.claimed-1-boot-self"), "{}").unwrap();
        recovery(&fixture).sweep().await;
        assert!(!dir.join("w.json.2-boot-peer.tmp").exists());
        assert!(!dir.join("w.json.claimed-2-boot-peer").exists());
        assert!(dir.join("kept.json.claimed-1-boot-self").exists());
    }
}
