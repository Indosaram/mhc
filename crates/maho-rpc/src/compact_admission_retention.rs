//! senpi `modes/rpc/compact-admission-retention.ts`: LRU terminal-row retention and bounded
//! retired-digest ring eviction for `CompactAdmissions`.

use crate::compact_admission_error::{CompactAdmissionError, CompactAdmissionReason};
use crate::compact_admission_storage::ADMISSION_LIMITS;
use crate::compact_admission_types::Epoch;

/// RULING-113: a terminal row stays answerable as the most recent LRU entry; a live row is never retained.
pub fn retain_done(epoch: &mut Epoch, key: &str, admission_id: &str) {
    if epoch.hot.contains_key(key) {
        return;
    }
    epoch.done.retain(|(k, _)| k != key);
    epoch.done.push((key.to_owned(), admission_id.to_owned()));
}

/// Evict least-recent terminal rows (file and reservation) until one more row fits; each leaves
/// its digest. A row whose admission still routes a stream is skipped (#5274), yet still counts.
pub async fn reserve_row(
    epoch: &mut Epoch,
    capacity: usize,
    routed: &(dyn Fn(&str) -> bool + Sync),
    overflowed: &mut (dyn FnMut() + Send),
) -> Result<bool, CompactAdmissionError> {
    let mut index = 0usize;
    while index < epoch.done.len() {
        if epoch.hot.len() + epoch.done.len() < capacity {
            return Ok(true);
        }
        let (key, admission_id) = epoch.done[index].clone();
        if routed(&admission_id) {
            index += 1;
            continue;
        }
        let path = epoch.directory.join(&key);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(CompactAdmissionError::new(
                    CompactAdmissionReason::StorageUnavailable,
                ));
            }
        }
        epoch.done.remove(index);
        epoch.failures.remove(&key);
        if epoch.retired.insert(key.clone()) {
            epoch.retired_order.push_back(key);
        }
        while epoch.retired.len() > ADMISSION_LIMITS.retired {
            if let Some(oldest) = epoch.retired_order.pop_front() {
                epoch.retired.remove(&oldest);
                overflowed();
            } else {
                break;
            }
        }
    }
    Ok(epoch.hot.len() + epoch.done.len() < capacity)
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let mut overflows = 0usize;
        let ok = reserve_row(&mut epoch, 2, &|adm| adm == "routed-adm", &mut || {
            overflows += 1
        })
        .await
        .expect("reserve");
        assert!(ok);
        assert_eq!(epoch.done, vec![("k1".into(), "routed-adm".into())]);
        assert!(epoch.retired.contains("k2"));
        assert!(!dir.path().join("k2").exists());
        assert_eq!(overflows, 0);
    }
}
