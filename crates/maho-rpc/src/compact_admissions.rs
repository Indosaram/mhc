//! senpi `modes/rpc/compact-admissions.ts`: host-owned epoch attempt index.
//! Hash-file lookup is bounded by one <=131,584-byte row; hot results are never history.

pub use crate::compact_admission_error::{CompactAdmissionError, CompactAdmissionReason};
use crate::compact_admission_producer::{
    ProducerAbortSignal, ProducerLossKind, ProducerLosses, record_producer_loss,
    run_admission_producer,
};
use crate::compact_admission_retention::{reserve_row, retain_done};
use crate::compact_admission_settlement::settle_produced_admission;
pub use crate::compact_admission_storage::ADMISSION_LIMITS;
use crate::compact_admission_storage::{
    AdmissionRow, AdmissionState, read_admission_row, write_admission_row,
};
use crate::compact_admission_types::{AdmissionIdentity, Answer, Epoch, HotSender, Produced};
use crate::compact_recovery_ids::parse_bounded_id;
use futures::future::BoxFuture;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    Cancelled,
    AlreadyComplete,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionResult {
    pub state: AdmissionState,
    pub answer: Answer,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdmissionsStatus {
    pub abandoned: u64,
    pub release_failed: u64,
    pub retired_overflow: u64,
}

pub type RoutedFn = Arc<dyn Fn(&str, &str) -> bool + Send + Sync>;

enum Registered {
    Complete(Answer),
    Join(watch::Receiver<Option<Result<Answer, CompactAdmissionError>>>),
    Fresh(HotSender),
}

struct AdmissionsState {
    epochs: HashMap<String, Arc<Mutex<Epoch>>>,
    losses: ProducerLosses,
    retired_overflow: u64,
}

pub struct CompactAdmissions {
    state: Arc<Mutex<AdmissionsState>>,
    max_entries: usize,
    max_bytes: usize,
    routed: RoutedFn,
}

impl Default for CompactAdmissions {
    fn default() -> Self {
        Self::new(
            ADMISSION_LIMITS.entries,
            ADMISSION_LIMITS.bytes,
            Arc::new(|_, _| false),
        )
    }
}

impl CompactAdmissions {
    pub fn new(max_entries: usize, max_bytes: usize, routed: RoutedFn) -> Self {
        Self {
            state: Arc::new(Mutex::new(AdmissionsState {
                epochs: HashMap::new(),
                losses: ProducerLosses::default(),
                retired_overflow: 0,
            })),
            max_entries,
            max_bytes,
            routed,
        }
    }

    pub fn status(&self) -> AdmissionsStatus {
        let guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        AdmissionsStatus {
            abandoned: guard.losses.abandoned,
            release_failed: guard.losses.release_failed,
            retired_overflow: guard.retired_overflow,
        }
    }

    fn key(&self, id: &str) -> Result<String, CompactAdmissionError> {
        if parse_bounded_id(id).is_err() {
            return Err(CompactAdmissionError::new(
                CompactAdmissionReason::MalformedRequest,
            ));
        }
        Ok(hex::encode(Sha256::digest(id.as_bytes())))
    }

    fn get_or_create_epoch(
        &self,
        socket_epoch: &str,
        peer: &str,
    ) -> Result<Arc<Mutex<Epoch>>, CompactAdmissionError> {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = guard.epochs.get(socket_epoch) {
            return Ok(existing.clone());
        }
        let dir_name = format!("senpi-admissions-{}", uuid::Uuid::new_v4());
        let dir = std::env::temp_dir().join(dir_name);
        std::fs::create_dir_all(&dir)
            .map_err(|_| CompactAdmissionError::new(CompactAdmissionReason::StorageUnavailable))?;
        let epoch = Arc::new(Mutex::new(Epoch::new(peer.to_owned(), dir)));
        guard.epochs.insert(socket_epoch.to_owned(), epoch.clone());
        Ok(epoch)
    }

    fn get_epoch(&self, socket_epoch: &str) -> Option<Arc<Mutex<Epoch>>> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .epochs
            .get(socket_epoch)
            .cloned()
    }

    fn end_epoch(&self, epoch: &Arc<Mutex<Epoch>>) {
        let producer = {
            let mut guard = epoch.lock().unwrap_or_else(PoisonError::into_inner);
            guard.ended = true;
            guard.producer.clone()
        };
        let state = self.state.clone();
        let ep = epoch.clone();
        producer.end(move || {
            let hot_count = ep.lock().unwrap_or_else(PoisonError::into_inner).hot.len() as u64;
            let mut s = state.lock().unwrap_or_else(PoisonError::into_inner);
            record_producer_loss(&mut s.losses, ProducerLossKind::Abandoned, hot_count, None);
        });
    }

    async fn acquire_serial(
        epoch: &Arc<Mutex<Epoch>>,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, CompactAdmissionError> {
        let serial_lock = {
            let mut guard = epoch.lock().unwrap_or_else(PoisonError::into_inner);
            if guard.waiting >= ADMISSION_LIMITS.hot {
                return Err(CompactAdmissionError::new(
                    CompactAdmissionReason::AdmissionBusy,
                ));
            }
            guard.waiting += 1;
            guard.serial_lock.clone()
        };
        let lock_guard = serial_lock.lock_owned().await;
        epoch.lock().unwrap_or_else(PoisonError::into_inner).waiting -= 1;
        Ok(lock_guard)
    }

    pub async fn run<F>(
        &self,
        identity: AdmissionIdentity,
        produce: F,
    ) -> Result<Answer, CompactAdmissionError>
    where
        F: FnOnce(
            ProducerAbortSignal,
        ) -> BoxFuture<'static, Result<Produced, CompactAdmissionError>>,
    {
        let key = self.key(&identity.admission_id)?;
        let epoch = self.get_or_create_epoch(&identity.socket_epoch, &identity.peer)?;

        let registered = {
            let _serial = Self::acquire_serial(&epoch).await?;
            let (ended, dir, failures) = {
                let guard = epoch.lock().unwrap_or_else(PoisonError::into_inner);
                (guard.ended, guard.directory.clone(), guard.failures.clone())
            };
            if ended {
                return Err(CompactAdmissionError::new(
                    CompactAdmissionReason::AdmissionRetired,
                ));
            }
            if let Some(row) = read_admission_row(&dir, Some(&failures), &key).await? {
                if row.fingerprint != identity.fingerprint {
                    return Err(CompactAdmissionError::new(
                        CompactAdmissionReason::AdmissionConflict,
                    ));
                }
                {
                    let mut guard = epoch.lock().unwrap_or_else(PoisonError::into_inner);
                    retain_done(&mut guard, &key, &identity.admission_id);
                }
                match row.state {
                    AdmissionState::Complete => {
                        let Some(raw) = row.answer else {
                            return Err(CompactAdmissionError::new(
                                CompactAdmissionReason::StorageUnavailable,
                            ));
                        };
                        let parsed: Answer = serde_json::from_str(&raw).map_err(|_| {
                            CompactAdmissionError::new(CompactAdmissionReason::StorageUnavailable)
                        })?;
                        Registered::Complete(parsed)
                    }
                    AdmissionState::Cancelled => {
                        return Err(CompactAdmissionError::new(
                            CompactAdmissionReason::AdmissionRetired,
                        ));
                    }
                    AdmissionState::Failed => {
                        let reason = if row.failure.as_deref() == Some("storage_unavailable") {
                            CompactAdmissionReason::StorageUnavailable
                        } else {
                            CompactAdmissionReason::AdmissionRetired
                        };
                        return Err(CompactAdmissionError::new(reason));
                    }
                    AdmissionState::Pending => {
                        let guard = epoch.lock().unwrap_or_else(PoisonError::into_inner);
                        let Some(tx) = guard.hot.get(&key) else {
                            return Err(CompactAdmissionError::new(
                                CompactAdmissionReason::StorageUnavailable,
                            ));
                        };
                        Registered::Join(tx.subscribe())
                    }
                }
            } else {
                if identity.retry {
                    return Err(CompactAdmissionError::new(
                        CompactAdmissionReason::AdmissionUnknown,
                    ));
                }
                let capacity = self.max_entries.min(self.max_bytes / ADMISSION_LIMITS.row);
                let mut ep_snapshot = {
                    let mut guard = epoch.lock().unwrap_or_else(PoisonError::into_inner);
                    if guard.retired.contains(&key) {
                        return Err(CompactAdmissionError::new(
                            CompactAdmissionReason::AdmissionRetired,
                        ));
                    }
                    if guard.hot.len() >= ADMISSION_LIMITS.hot {
                        return Err(CompactAdmissionError::new(
                            CompactAdmissionReason::AdmissionBusy,
                        ));
                    }
                    // Reserve while holding no lock across await by temporarily taking state.
                    std::mem::replace(&mut *guard, Epoch::new(String::new(), PathBuf::new()))
                };
                let socket_epoch = identity.socket_epoch.clone();
                let routed = self.routed.clone();
                let mut overflowed = || {
                    let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                    if s.retired_overflow == 0 {
                        eprintln!(
                            "senpi rpc admission retired-ID ring overflowed ({} digests); older IDs are forgotten",
                            ADMISSION_LIMITS.retired
                        );
                    }
                    s.retired_overflow += 1;
                };
                let reserved = reserve_row(
                    &mut ep_snapshot,
                    capacity,
                    &|adm| routed(&socket_epoch, adm),
                    &mut overflowed,
                )
                .await;
                {
                    let mut guard = epoch.lock().unwrap_or_else(PoisonError::into_inner);
                    // Preserve any `ended` flag set concurrently while reserving.
                    ep_snapshot.ended |= guard.ended;
                    ep_snapshot.waiting = guard.waiting;
                    *guard = ep_snapshot;
                }
                if !reserved? {
                    return Err(CompactAdmissionError::new(
                        CompactAdmissionReason::AdmissionLimit,
                    ));
                }
                write_admission_row(
                    &dir,
                    &key,
                    &AdmissionRow {
                        state: AdmissionState::Pending,
                        fingerprint: identity.fingerprint.clone(),
                        failure: None,
                        answer: None,
                    },
                )
                .await?;
                let (tx, _rx) = watch::channel(None);
                epoch
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .hot
                    .insert(key.clone(), tx.clone());
                Registered::Fresh(tx)
            }
        };

        let pending_tx = match registered {
            Registered::Complete(ans) => return Ok(ans),
            Registered::Join(mut rx) => loop {
                if let Some(res) = rx.borrow_and_update().clone() {
                    return res;
                }
                if rx.changed().await.is_err() {
                    return Err(CompactAdmissionError::new(
                        CompactAdmissionReason::StorageUnavailable,
                    ));
                }
            },
            Registered::Fresh(tx) => tx,
        };

        let signal = epoch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .producer
            .signal();

        struct HotGuard {
            epoch: Arc<Mutex<Epoch>>,
            key: String,
            admission_id: String,
        }
        impl Drop for HotGuard {
            fn drop(&mut self) {
                let mut guard = self.epoch.lock().unwrap_or_else(PoisonError::into_inner);
                guard.hot.remove(&self.key);
                retain_done(&mut guard, &self.key, &self.admission_id);
            }
        }
        let _hot_guard = HotGuard {
            epoch: epoch.clone(),
            key: key.clone(),
            admission_id: identity.admission_id.clone(),
        };

        let state_for_release = self.state.clone();
        let produced_res = run_admission_producer(
            signal,
            || {
                produce(
                    epoch
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .producer
                        .signal(),
                )
            },
            move |mut produced| async move {
                if let Some(release) = produced.release.take()
                    && let Err(cause) = release().await
                {
                    let mut s = state_for_release
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    record_producer_loss(
                        &mut s.losses,
                        ProducerLossKind::ReleaseFailed,
                        1,
                        Some(&cause),
                    );
                }
            },
        )
        .await;

        let mut produced = match produced_res {
            Ok(p) => p,
            Err(cause) => {
                self.record_failure(&epoch, &key, &identity.fingerprint, &pending_tx, cause)
                    .await
                    .err()
                    .map_or_else(
                        || {
                            Err(CompactAdmissionError::new(
                                CompactAdmissionReason::StorageUnavailable,
                            ))
                        },
                        Err,
                    )?;
                unreachable!()
            }
        };

        let release_fn = produced.release.take();
        let settled =
            settle_produced_admission(&epoch, &key, &identity.fingerprint, produced).await;
        match settled {
            Ok(settled) => {
                if settled.cancelled
                    && let Some(release) = release_fn
                {
                    let _ = release().await;
                }
                pending_tx.send_replace(Some(Ok(settled.value.clone())));
                Ok(settled.value)
            }
            Err(cause) => {
                if let Some(release) = release_fn {
                    let _ = release().await;
                }
                Err(self
                    .record_failure(&epoch, &key, &identity.fingerprint, &pending_tx, cause)
                    .await
                    .expect_err("failure"))
            }
        }
    }

    async fn record_failure(
        &self,
        epoch: &Arc<Mutex<Epoch>>,
        key: &str,
        fingerprint: &str,
        pending_tx: &HotSender,
        mut failure: CompactAdmissionError,
    ) -> Result<(), CompactAdmissionError> {
        let row = AdmissionRow {
            state: AdmissionState::Failed,
            fingerprint: fingerprint.to_owned(),
            failure: (failure.reason == CompactAdmissionReason::StorageUnavailable)
                .then(|| "storage_unavailable".to_owned()),
            answer: None,
        };
        if row.failure.is_some() {
            epoch
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .failures
                .insert(key.to_owned(), row.clone());
        }
        let dir = {
            let _serial = Self::acquire_serial(epoch).await;
            epoch
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .directory
                .clone()
        };
        if let Err(storage_cause) = write_admission_row(&dir, key, &row).await {
            eprintln!("senpi rpc admission failure row write failed: {storage_cause}");
            epoch
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .failures
                .insert(
                    key.to_owned(),
                    AdmissionRow {
                        failure: Some("storage_unavailable".into()),
                        ..row
                    },
                );
            failure = CompactAdmissionError::new(CompactAdmissionReason::StorageUnavailable);
        }
        pending_tx.send_replace(Some(Err(failure.clone())));
        Err(failure)
    }

    pub async fn pending(
        &self,
        socket_epoch: &str,
        admission_id: &str,
    ) -> Result<bool, CompactAdmissionError> {
        let key = self.key(admission_id)?;
        let Some(epoch) = self.get_epoch(socket_epoch) else {
            return Ok(false);
        };
        if epoch.lock().unwrap_or_else(PoisonError::into_inner).ended {
            return Ok(false);
        }
        let _serial = Self::acquire_serial(&epoch).await?;
        let (dir, failures) = {
            let guard = epoch.lock().unwrap_or_else(PoisonError::into_inner);
            (guard.directory.clone(), guard.failures.clone())
        };
        Ok(read_admission_row(&dir, Some(&failures), &key)
            .await?
            .is_some_and(|r| r.state == AdmissionState::Pending))
    }

    pub async fn cancel(
        &self,
        socket_epoch: &str,
        admission_id: &str,
    ) -> Result<CancelOutcome, CompactAdmissionError> {
        let key = self.key(admission_id)?;
        let Some(epoch) = self.get_epoch(socket_epoch) else {
            return Ok(CancelOutcome::Unknown);
        };
        let _serial = Self::acquire_serial(&epoch).await?;
        let (dir, failures, is_retired) = {
            let guard = epoch.lock().unwrap_or_else(PoisonError::into_inner);
            (
                guard.directory.clone(),
                guard.failures.clone(),
                guard.retired.contains(&key),
            )
        };
        let Some(row) = read_admission_row(&dir, Some(&failures), &key).await? else {
            return Ok(if is_retired {
                CancelOutcome::AlreadyComplete
            } else {
                CancelOutcome::Unknown
            });
        };
        match row.state {
            AdmissionState::Complete | AdmissionState::Failed => Ok(CancelOutcome::AlreadyComplete),
            AdmissionState::Cancelled => Ok(CancelOutcome::Cancelled),
            AdmissionState::Pending => {
                write_admission_row(
                    &dir,
                    &key,
                    &AdmissionRow {
                        state: AdmissionState::Cancelled,
                        ..row
                    },
                )
                .await?;
                Ok(CancelOutcome::Cancelled)
            }
        }
    }

    pub async fn result(
        &self,
        socket_epoch: &str,
        admission_id: &str,
    ) -> Result<Option<AdmissionResult>, CompactAdmissionError> {
        let key = self.key(admission_id)?;
        let Some(epoch) = self.get_epoch(socket_epoch) else {
            return Ok(None);
        };
        let _serial = Self::acquire_serial(&epoch).await?;
        let (dir, failures) = {
            let guard = epoch.lock().unwrap_or_else(PoisonError::into_inner);
            (guard.directory.clone(), guard.failures.clone())
        };
        let Some(row) = read_admission_row(&dir, Some(&failures), &key).await? else {
            return Ok(None);
        };
        let Some(raw) = row.answer else {
            return Ok(None);
        };
        let answer: Answer = serde_json::from_str(&raw)
            .map_err(|_| CompactAdmissionError::new(CompactAdmissionReason::StorageUnavailable))?;
        Ok(Some(AdmissionResult {
            state: row.state,
            answer,
        }))
    }

    pub async fn forget_peer(&self, socket_epoch: &str) {
        let Some(epoch) = self.get_epoch(socket_epoch) else {
            return;
        };
        self.end_epoch(&epoch);
        let receivers: Vec<watch::Receiver<Option<Result<Answer, CompactAdmissionError>>>> = {
            epoch
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .hot
                .values()
                .map(watch::Sender::subscribe)
                .collect()
        };
        for mut rx in receivers {
            while rx.borrow_and_update().is_none() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
        }
        let serial_lock = epoch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .serial_lock
            .clone();
        let _serial = serial_lock.lock().await;
        let dir = {
            let guard = epoch.lock().unwrap_or_else(PoisonError::into_inner);
            guard.producer.close();
            guard.directory.clone()
        };
        let _ = tokio::fs::remove_dir_all(&dir).await;
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .epochs
            .remove(socket_epoch);
    }

    pub fn end_connection(&self, peer: &str) {
        let epochs: Vec<Arc<Mutex<Epoch>>> = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .epochs
            .values()
            .filter(|ep| ep.lock().unwrap_or_else(PoisonError::into_inner).peer == peer)
            .cloned()
            .collect();
        for ep in epochs {
            self.end_epoch(&ep);
        }
    }

    pub async fn forget_connection(&self, peer: &str) {
        let ids: Vec<String> = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .epochs
            .iter()
            .filter(|(_, ep)| ep.lock().unwrap_or_else(PoisonError::into_inner).peer == peer)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.forget_peer(&id).await;
        }
    }

    pub async fn dispose(&self) {
        let (all_epochs, ids): (Vec<Arc<Mutex<Epoch>>>, Vec<String>) = {
            let guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            (
                guard.epochs.values().cloned().collect(),
                guard.epochs.keys().cloned().collect(),
            )
        };
        for ep in all_epochs {
            self.end_epoch(&ep);
        }
        for id in ids {
            self.forget_peer(&id).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn identity(id: &str, fp: &str, retry: bool) -> AdmissionIdentity {
        AdmissionIdentity {
            socket_epoch: "se-1".into(),
            peer: "peer-1".into(),
            admission_id: id.into(),
            fingerprint: fp.into(),
            retry,
        }
    }

    #[tokio::test]
    async fn replays_completed_refuses_conflict_and_rejects_unconfirmed_retry() {
        let admissions = CompactAdmissions::default();
        let fp = "a".repeat(64);
        let calls = Arc::new(AtomicUsize::new(0));

        let c1 = calls.clone();
        let ans1 = admissions
            .run(identity("adm-1", &fp, false), |_| {
                c1.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {
                    let mut map = Answer::new();
                    map.insert("sessionId".into(), json!("s1"));
                    Ok(Produced::simple(map))
                })
            })
            .await
            .expect("first run");
        assert_eq!(ans1["sessionId"], "s1");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Idempotent replay with same fingerprint
        let c2 = calls.clone();
        let ans2 = admissions
            .run(identity("adm-1", &fp, true), |_| {
                c2.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(Produced::simple(Answer::new())) })
            })
            .await
            .expect("replay");
        assert_eq!(ans2, ans1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Conflict with different fingerprint
        let err = admissions
            .run(identity("adm-1", &"b".repeat(64), false), |_| {
                Box::pin(async { Ok(Produced::simple(Answer::new())) })
            })
            .await
            .expect_err("conflict");
        assert_eq!(err.reason, CompactAdmissionReason::AdmissionConflict);

        // Retry with unknown ID refuses with AdmissionUnknown
        let unknown = admissions
            .run(identity("adm-never", &fp, true), |_| {
                Box::pin(async { Ok(Produced::simple(Answer::new())) })
            })
            .await
            .expect_err("unknown retry");
        assert_eq!(unknown.reason, CompactAdmissionReason::AdmissionUnknown);

        admissions.dispose().await;
    }

    #[tokio::test]
    async fn concurrent_duplicate_joins_in_flight_producer() {
        let admissions = Arc::new(CompactAdmissions::default());
        let fp = "c".repeat(64);
        let calls = Arc::new(AtomicUsize::new(0));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();

        let a1 = admissions.clone();
        let c1 = calls.clone();
        let fp1 = fp.clone();
        let h1 = tokio::spawn(async move {
            a1.run(identity("adm-join", &fp1, false), |_| {
                c1.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    let _ = entered_tx.send(());
                    let _ = gate_rx.await;
                    let mut map = Answer::new();
                    map.insert("joined".into(), json!(true));
                    Ok(Produced::simple(map))
                })
            })
            .await
        });

        let _ = entered_rx.await;
        assert!(
            admissions
                .pending("se-1", "adm-join")
                .await
                .expect("pending")
        );

        let a2 = admissions.clone();
        let fp2 = fp.clone();
        let h2 = tokio::spawn(async move {
            a2.run(identity("adm-join", &fp2, false), |_| {
                Box::pin(async { Ok(Produced::simple(Answer::new())) })
            })
            .await
        });

        let _ = gate_tx.send(());
        let r1 = h1.await.expect("h1").expect("ok1");
        let r2 = h2.await.expect("h2").expect("ok2");
        assert_eq!(r1["joined"], true);
        assert_eq!(r1, r2);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        admissions.dispose().await;
    }

    #[tokio::test]
    async fn cancel_while_pending_marks_cancelled_and_releases_producer() {
        let admissions = Arc::new(CompactAdmissions::default());
        let fp = "d".repeat(64);
        let released = Arc::new(AtomicUsize::new(0));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();

        let a1 = admissions.clone();
        let fp1 = fp.clone();
        let rel = released.clone();
        let h1 = tokio::spawn(async move {
            a1.run(identity("adm-cancel", &fp1, false), |_| {
                Box::pin(async move {
                    let _ = entered_tx.send(());
                    let _ = gate_rx.await;
                    let mut map = Answer::new();
                    map.insert("generation".into(), json!(1));
                    Ok(Produced {
                        answer: map,
                        release: Some(Box::new(move || {
                            Box::pin(async move {
                                rel.fetch_add(1, Ordering::SeqCst);
                                Ok(())
                            })
                        })),
                        activate: None,
                        refusal: None,
                    })
                })
            })
            .await
        });

        let _ = entered_rx.await;
        assert_eq!(
            admissions
                .cancel("se-1", "adm-cancel")
                .await
                .expect("cancel"),
            CancelOutcome::Cancelled
        );
        let _ = gate_tx.send(());
        let _ = h1.await.expect("join");
        assert_eq!(released.load(Ordering::SeqCst), 1);
        let res = admissions
            .result("se-1", "adm-cancel")
            .await
            .expect("res")
            .expect("present");
        assert_eq!(res.state, AdmissionState::Cancelled);
        admissions.dispose().await;
    }
}
