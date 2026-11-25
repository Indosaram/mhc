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
    let _serial = serial_lock.lock().await;

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

    let mut value = produced
        .refusal
        .as_ref()
        .and_then(|f| f())
        .unwrap_or_else(|| produced.answer.clone());
    persist_answer(&dir, key, fingerprint, retired, &value).await?;

    // Overflow during the durable write is a terminal, replayable refusal, not a fresh attempt.
    let refused = produced.refusal.as_ref().and_then(|f| f());
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
    if let Some(activate) = produced.activate.take()
        && let Err(cause) = activate()
    {
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
