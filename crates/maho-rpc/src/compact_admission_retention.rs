//! senpi `modes/rpc/compact-admission-retention.ts`: LRU terminal-row retention and bounded
//! retired-digest ring eviction for `CompactAdmissions`.

use crate::compact_admission_error::{CompactAdmissionError, CompactAdmissionReason};
use crate::compact_admission_storage::ADMISSION_LIMITS;
use crate::compact_admission_types::Epoch;
use std::sync::{Arc, Mutex, PoisonError};

/// RULING-113: a terminal row stays answerable as the most recent LRU entry; a live row is never retained.
pub fn retain_done(epoch: &mut Epoch, key: &str, admission_id: &str) {
    if epoch.hot.contains_key(key) {
        return;
    }
    epoch.done.retain(|(k, _)| k != key);
    epoch.done.push((key.to_owned(), admission_id.to_owned()));
}

struct RetireOnDrop<'a> {
    epoch: &'a Arc<Mutex<Epoch>>,
    key: Option<String>,
    overflowed: &'a mut (dyn FnMut() + Send),
}

impl Drop for RetireOnDrop<'_> {
    fn drop(&mut self) {
        let Some(key) = self.key.take() else { return };
        let overflows = {
            let mut epoch = self.epoch.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(position) = epoch.done.iter().position(|(existing, _)| existing == &key) {
                epoch.done.remove(position);
            }
            epoch.failures.remove(&key);
            if epoch.retired.insert(key.clone()) {
                epoch.retired_order.push_back(key);
            }
            let mut overflows = 0;
            while epoch.retired.len() > ADMISSION_LIMITS.retired {
                if let Some(oldest) = epoch.retired_order.pop_front() {
                    epoch.retired.remove(&oldest);
                    overflows += 1;
                } else {
                    break;
                }
            }
            overflows
        };
        for _ in 0..overflows {
            (self.overflowed)();
        }
    }
}

/// Evict least-recent terminal rows (file and reservation) until one more row fits; each leaves
/// its digest. A row whose admission still routes a stream is skipped (#5274), yet still counts.
pub async fn reserve_row(
    epoch: &Arc<Mutex<Epoch>>,
    capacity: usize,
    routed: &(dyn Fn(&str) -> bool + Sync),
    overflowed: &mut (dyn FnMut() + Send),
) -> Result<bool, CompactAdmissionError> {
    reserve_row_with_remove(epoch, capacity, routed, overflowed, |path| async move {
        tokio::fs::remove_file(path).await
    })
    .await
}

async fn reserve_row_with_remove<F, Fut>(
    epoch: &Arc<Mutex<Epoch>>,
    capacity: usize,
    routed: &(dyn Fn(&str) -> bool + Sync),
    overflowed: &mut (dyn FnMut() + Send),
    mut remove: F,
) -> Result<bool, CompactAdmissionError>
where
    F: FnMut(std::path::PathBuf) -> Fut,
    Fut: std::future::Future<Output = std::io::Result<()>>,
{
    let mut index = 0usize;
    loop {
        let candidate = {
            let epoch = epoch.lock().unwrap_or_else(PoisonError::into_inner);
            if epoch.hot.len() + epoch.done.len() < capacity {
                return Ok(true);
            }
            epoch.done.get(index).map(|(key, admission_id)| {
                (key.clone(), admission_id.clone(), epoch.directory.join(key))
            })
        };
        let Some((key, admission_id, path)) = candidate else {
            return Ok(false);
        };
        if routed(&admission_id) {
            index += 1;
            continue;
        }
        let mut retire = RetireOnDrop {
            epoch,
            key: Some(key),
            overflowed: &mut *overflowed,
        };
        match remove(path).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                retire.key = None;
                return Err(CompactAdmissionError::new(
                    CompactAdmissionReason::StorageUnavailable,
                ));
            }
        }
        drop(retire);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_during_unlink_retires_the_evicted_digest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut state = Epoch::new("p1".into(), dir.path().to_path_buf());
        std::fs::write(dir.path().join("victim"), b"{}").expect("row");
        retain_done(&mut state, "victim", "adm");
        let epoch = Arc::new(Mutex::new(state));
        let mut overflows = 0;
        let mut overflowed = || overflows += 1;
        let mut reserve = Box::pin(reserve_row_with_remove(
            &epoch,
            1,
            &|_| false,
            &mut overflowed,
            |_| std::future::pending(),
        ));
        std::future::poll_fn(|cx| {
            assert!(reserve.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        std::fs::remove_file(dir.path().join("victim")).expect("unlink completed");
        drop(reserve);
        let state = epoch.lock().expect("epoch");
        assert!(
            state.retired.contains("victim"),
            "removed row must not become a fresh admission"
        );
        assert!(state.done.is_empty());
    }

    #[tokio::test]
    async fn failed_unlink_keeps_the_original_row_reserved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut state = Epoch::new("p1".into(), dir.path().to_path_buf());
        retain_done(&mut state, "victim", "adm");
        let epoch = Arc::new(Mutex::new(state));
        let result = reserve_row_with_remove(&epoch, 1, &|_| false, &mut || {}, |_| async {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        })
        .await;
        assert_eq!(
            result.expect_err("unlink failure").reason,
            CompactAdmissionReason::StorageUnavailable
        );
        let state = epoch.lock().expect("epoch");
        assert_eq!(state.done.len(), 1);
        assert!(!state.retired.contains("victim"));
    }

    #[test]
    fn retain_done_moves_key_to_mru_and_skips_hot() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut epoch = Epoch::new("p1".into(), dir.path().to_path_buf());
        retain_done(&mut epoch, "k1", "a1");
        retain_done(&mut epoch, "k2", "a2");
        retain_done(&mut epoch, "k1", "a1");
        assert_eq!(
            epoch.done,
            vec![("k2".into(), "a2".into()), ("k1".into(), "a1".into())]
        );

        let (tx, _rx) = tokio::sync::watch::channel(None);
        epoch.hot.insert("k3".into(), tx);
        retain_done(&mut epoch, "k3", "a3");
        assert_eq!(epoch.done.len(), 2);
    }

    #[tokio::test]
    async fn reserve_row_evicts_non_routed_lru_and_caps_retired_ring() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut epoch = Epoch::new("p1".into(), dir.path().to_path_buf());
        tokio::fs::write(dir.path().join("k1"), b"{}")
            .await
            .expect("w1");
        tokio::fs::write(dir.path().join("k2"), b"{}")
            .await
            .expect("w2");
        retain_done(&mut epoch, "k1", "routed-adm");
        retain_done(&mut epoch, "k2", "idle-adm");

        let epoch = Arc::new(Mutex::new(epoch));
        let mut overflows = 0usize;
        let ok = reserve_row(&epoch, 2, &|adm| adm == "routed-adm", &mut || {
            overflows += 1
        })
        .await
        .expect("reserve");
        assert!(ok);
        let epoch = epoch.lock().expect("epoch");
        assert_eq!(epoch.done, vec![("k1".into(), "routed-adm".into())]);
        assert!(epoch.retired.contains("k2"));
        assert!(!dir.path().join("k2").exists());
        assert_eq!(overflows, 0);
    }

    #[tokio::test]
    async fn retired_overflow_callback_mutates_and_can_reenter_epoch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut epoch = Epoch::new("p1".into(), dir.path().to_path_buf());
        for index in 0..ADMISSION_LIMITS.retired {
            let key = format!("old-{index}");
            epoch.retired.insert(key.clone());
            epoch.retired_order.push_back(key);
        }
        tokio::fs::write(dir.path().join("victim"), b"{}")
            .await
            .expect("row");
        retain_done(&mut epoch, "victim", "adm");
        let epoch = Arc::new(Mutex::new(epoch));
        let mut overflows = 0;
        let mut unlocked = false;
        assert!(
            reserve_row(&epoch, 1, &|_| false, &mut || {
                overflows += 1;
                unlocked = epoch.try_lock().is_ok();
            })
            .await
            .expect("reserve")
        );
        assert_eq!(overflows, 1);
        assert!(
            unlocked,
            "overflow callback must not run under the epoch lock"
        );
        assert_eq!(
            epoch.lock().expect("epoch").retired.len(),
            ADMISSION_LIMITS.retired
        );
    }
}
