//! senpi `modes/rpc/compact-admission-settlement.ts`: settles a produced admission inside
//! the epoch's serial lane (durable terminal row first, synchronous activation second).

use crate::compact_admission_error::{CompactAdmissionError, CompactAdmissionReason};
use crate::compact_admission_storage::{
    AdmissionRow, AdmissionState, MAX_ADMISSION_ANSWER_BYTES, read_admission_row,
    write_admission_row,
};
use crate::compact_admission_types::{Answer, Epoch, Produced};
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledAdmission {
    pub value: Answer,
    pub cancelled: bool,
}

struct CallbackGuard {
    epoch: Arc<Mutex<Epoch>>,
    key: String,
    fingerprint: String,
    completed: bool,
}

impl Drop for CallbackGuard {
    fn drop(&mut self) {
        let mut epoch = self.epoch.lock().unwrap_or_else(PoisonError::into_inner);
        epoch.callback_active = false;
        if !self.completed {
            epoch
                .failures
                .entry(self.key.clone())
                .or_insert_with(|| AdmissionRow {
                    state: AdmissionState::Failed,
                    fingerprint: self.fingerprint.clone(),
                    failure: Some("storage_unavailable".into()),
                    answer: None,
                });
        }
    }
}

async fn without_serial<'a, T>(
    epoch: &Arc<Mutex<Epoch>>,
    key: &str,
    fingerprint: &str,
    serial_lock: &'a tokio::sync::Mutex<()>,
    serial: tokio::sync::MutexGuard<'a, ()>,
    callback: impl FnOnce() -> T,
) -> (tokio::sync::MutexGuard<'a, ()>, T) {
    epoch
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .callback_active = true;
    let mut callback_guard = CallbackGuard {
        epoch: Arc::clone(epoch),
        key: key.to_owned(),
        fingerprint: fingerprint.to_owned(),
        completed: false,
    };
    drop(serial);
    let value = callback();
    let serial = serial_lock.lock().await;
    callback_guard.completed = true;
    drop(callback_guard);
    (serial, value)
}

/// Settle a produced admission inside the epoch's serial lane: persist its terminal row, then activate it.
/// `cancelled` is true when the answer was stored without activation, so the caller must release the producer.
pub async fn settle_produced_admission(
    epoch: &Arc<Mutex<Epoch>>,
    key: &str,
    fingerprint: &str,
    mut produced: Produced,
) -> Result<SettledAdmission, CompactAdmissionError> {
    let serial_lock = {
        let guard = epoch.lock().unwrap_or_else(PoisonError::into_inner);
        guard.serial_lock.clone()
    };
    let mut serial = serial_lock.lock().await;
    if epoch
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .callback_active
    {
        return Err(CompactAdmissionError::new(
            CompactAdmissionReason::AdmissionBusy,
        ));
    }

    let (dir, failures_snapshot, ended) = {
        let guard = epoch.lock().unwrap_or_else(PoisonError::into_inner);
        (guard.directory.clone(), guard.failures.clone(), guard.ended)
    };
    let row = read_admission_row(&dir, Some(&failures_snapshot), key).await?;
    let retired = ended || row.is_some_and(|r| r.state == AdmissionState::Cancelled);

    async fn persist_answer(
        dir: &std::path::Path,
        key: &str,
        fingerprint: &str,
        retired: bool,
        value: &Answer,
    ) -> Result<(), CompactAdmissionError> {
        let answer = serde_json::to_string(value)
            .map_err(|_| CompactAdmissionError::new(CompactAdmissionReason::StorageUnavailable))?;
        if answer.len() > MAX_ADMISSION_ANSWER_BYTES {
            return Err(CompactAdmissionError::new(
                CompactAdmissionReason::StorageUnavailable,
            ));
        }
        write_admission_row(
            dir,
            key,
            &AdmissionRow {
                state: if retired {
                    AdmissionState::Cancelled
                } else {
                    AdmissionState::Complete
                },
                fingerprint: fingerprint.to_owned(),
                failure: None,
                answer: Some(answer),
            },
        )
        .await
    }

    let initial_refusal = if let Some(refusal) = produced.refusal.take() {
        let (reacquired, (refusal, result)) =
            without_serial(epoch, key, fingerprint, &serial_lock, serial, move || {
                let result = refusal();
                (refusal, result)
            })
            .await;
        serial = reacquired;
        produced.refusal = Some(refusal);
        result
    } else {
        None
    };
    let mut value = initial_refusal.unwrap_or_else(|| produced.answer.clone());
    persist_answer(&dir, key, fingerprint, retired, &value).await?;

    // Overflow during the durable write is a terminal, replayable refusal, not a fresh attempt.
    let refused = if let Some(refusal) = produced.refusal.take() {
        let (reacquired, (refusal, result)) =
            without_serial(epoch, key, fingerprint, &serial_lock, serial, move || {
                let result = refusal();
                (refusal, result)
            })
            .await;
        serial = reacquired;
        produced.refusal = Some(refusal);
        result
    } else {
        None
    };
    if let Some(ref ref_val) = refused
        && serde_json::to_string(ref_val).ok() != serde_json::to_string(&value).ok()
    {
        value = ref_val.clone();
        persist_answer(&dir, key, fingerprint, retired, &value).await?;
    }

    let now_ended = epoch.lock().unwrap_or_else(PoisonError::into_inner).ended;
    if retired || now_ended || refused.is_some() {
        return Ok(SettledAdmission {
            value,
            cancelled: true,
        });
    }

    // RULING-93: durable terminal row FIRST, synchronous activation SECOND.
    let activation_result = if let Some(activate) = produced.activate.take() {
        let (reacquired, result) =
            without_serial(epoch, key, fingerprint, &serial_lock, serial, activate).await;
        serial = reacquired;
        result
    } else {
        Ok(())
    };
    if let Err(cause) = activation_result {
        eprintln!("senpi rpc admission activation failed; releasing subscription: {cause}");
        epoch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .failures
            .insert(
                key.to_owned(),
                AdmissionRow {
                    state: AdmissionState::Failed,
                    fingerprint: fingerprint.to_owned(),
                    failure: Some("storage_unavailable".into()),
                    answer: None,
                },
            );
        return Err(CompactAdmissionError::new(
            CompactAdmissionReason::StorageUnavailable,
        ));
    }

    drop(serial);
    Ok(SettledAdmission {
        value,
        cancelled: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn settlement_callbacks_run_outside_serial_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let epoch = Arc::new(Mutex::new(Epoch::new(
            "p1".into(),
            dir.path().to_path_buf(),
        )));
        let serial = epoch.lock().expect("epoch").serial_lock.clone();
        let refusal_unlocked = Arc::new(AtomicBool::new(false));
        let activate_unlocked = Arc::new(AtomicBool::new(false));
        let refusal_serial = Arc::clone(&serial);
        let refusal_seen = Arc::clone(&refusal_unlocked);
        let activate_serial = Arc::clone(&serial);
        let activate_seen = Arc::clone(&activate_unlocked);
        let produced = Produced {
            answer: Answer::new(),
            release: None,
            refusal: Some(Box::new(move || {
                refusal_seen.store(refusal_serial.try_lock().is_ok(), Ordering::SeqCst);
                None
            })),
            activate: Some(Box::new(move || {
                activate_seen.store(activate_serial.try_lock().is_ok(), Ordering::SeqCst);
                Ok(())
            })),
        };
        settle_produced_admission(&epoch, "k1", &"a".repeat(64), produced)
            .await
            .expect("settle");
        assert!(refusal_unlocked.load(Ordering::SeqCst));
        assert!(activate_unlocked.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn panicked_activation_does_not_leave_replayable_success() {
        use futures::FutureExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let epoch = Arc::new(Mutex::new(Epoch::new(
            "p1".into(),
            dir.path().to_path_buf(),
        )));
        let produced = Produced {
            answer: Answer::new(),
            release: None,
            refusal: None,
            activate: Some(Box::new(|| panic!("activation panic"))),
        };
        let fingerprint = "a".repeat(64);
        let result = std::panic::AssertUnwindSafe(settle_produced_admission(
            &epoch,
            "k1",
            &fingerprint,
            produced,
        ))
        .catch_unwind()
        .await;
        assert!(result.is_err());
        let guard = epoch.lock().expect("epoch");
        assert!(!guard.callback_active);
        assert!(
            guard.failures.contains_key("k1"),
            "panicked activation must not replay success"
        );
    }

    #[tokio::test]
    async fn cancelled_serial_reacquisition_clears_busy_and_records_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let epoch = Arc::new(Mutex::new(Epoch::new(
            "p1".into(),
            dir.path().to_path_buf(),
        )));
        let serial = epoch.lock().expect("epoch").serial_lock.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        let produced = Produced {
            answer: Answer::new(),
            release: None,
            refusal: None,
            activate: Some(Box::new(move || {
                send.send(serial.try_lock_owned().expect("callback must run unlocked"))
                    .expect("receiver");
                Ok(())
            })),
        };
        let spawned_epoch = Arc::clone(&epoch);
        let task = tokio::spawn(async move {
            settle_produced_admission(&spawned_epoch, "k1", &"a".repeat(64), produced).await
        });
        let held = tokio::time::timeout(std::time::Duration::from_secs(5), receive)
            .await
            .expect("activation reached")
            .expect("serial lock");
        task.abort();
        assert!(task.await.expect_err("settlement cancelled").is_cancelled());
        drop(held);
        let guard = epoch.lock().expect("epoch");
        assert!(!guard.callback_active);
        assert!(guard.failures.contains_key("k1"));
    }

    #[tokio::test]
    async fn serial_wait_does_not_hold_epoch_mutex() {
        let dir = tempfile::tempdir().expect("tempdir");
        let epoch = Arc::new(Mutex::new(Epoch::new(
            "p1".into(),
            dir.path().to_path_buf(),
        )));
        let serial = epoch.lock().expect("epoch").serial_lock.clone();
        let held = serial.lock().await;
        let fingerprint = "a".repeat(64);
        let mut settlement = Box::pin(settle_produced_admission(
            &epoch,
            "k1",
            &fingerprint,
            Produced::simple(Answer::new()),
        ));
        std::future::poll_fn(|cx| {
            assert!(settlement.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert!(
            epoch.try_lock().is_ok(),
            "epoch mutex must remain available while waiting"
        );
        drop(held);
        settlement.await.expect("settle");
    }

    #[tokio::test]
    async fn persists_then_activates_on_normal_settlement() {
        let dir = tempfile::tempdir().expect("tempdir");
        let epoch = Arc::new(Mutex::new(Epoch::new(
            "p1".into(),
            dir.path().to_path_buf(),
        )));
        let activated = Arc::new(AtomicBool::new(false));
        let act_flag = activated.clone();
        let mut answer = Answer::new();
        answer.insert("success".into(), serde_json::json!(true));
        let produced = Produced {
            answer: answer.clone(),
            release: None,
            activate: Some(Box::new(move || {
                act_flag.store(true, Ordering::SeqCst);
                Ok(())
            })),
            refusal: None,
        };
        let settled = settle_produced_admission(&epoch, "k1", &"a".repeat(64), produced)
            .await
            .expect("settle");
        assert!(!settled.cancelled);
        assert!(activated.load(Ordering::SeqCst));
        let row = read_admission_row(dir.path(), None, "k1")
            .await
            .expect("read")
            .expect("present");
        assert_eq!(row.state, AdmissionState::Complete);
    }

    #[tokio::test]
    async fn cancelled_or_refused_settlement_skips_activation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let epoch = Arc::new(Mutex::new(Epoch::new(
            "p1".into(),
            dir.path().to_path_buf(),
        )));
        let fp = "b".repeat(64);
        write_admission_row(
            dir.path(),
            "k1",
            &AdmissionRow {
                fingerprint: fp.clone(),
                state: AdmissionState::Cancelled,
                failure: None,
                answer: None,
            },
        )
        .await
        .expect("write");

        let activated = Arc::new(AtomicBool::new(false));
        let act_flag = activated.clone();
        let mut answer = Answer::new();
        answer.insert("ok".into(), serde_json::json!(true));
        let produced = Produced {
            answer,
            release: None,
            activate: Some(Box::new(move || {
                act_flag.store(true, Ordering::SeqCst);
                Ok(())
            })),
            refusal: None,
        };
        let settled = settle_produced_admission(&epoch, "k1", &fp, produced)
            .await
            .expect("settle");
        assert!(settled.cancelled);
        assert!(!activated.load(Ordering::SeqCst));
        let row = read_admission_row(dir.path(), None, "k1")
            .await
            .expect("read")
            .expect("present");
        assert_eq!(row.state, AdmissionState::Cancelled);
    }
}
