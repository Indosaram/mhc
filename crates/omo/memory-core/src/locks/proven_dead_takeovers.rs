use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use super::lock_record::LockRecord;

#[derive(Debug, Clone)]
struct ProvenDeadTakeover {
    previous: LockRecord,
    takeover_nonce: String,
}

static PROVEN_DEAD_TAKEOVERS: Mutex<Option<HashMap<PathBuf, ProvenDeadTakeover>>> =
    Mutex::new(None);

fn lock_key(lock_path: &Path) -> PathBuf {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::support::paths::resolve_from(&base, lock_path)
}

pub fn record_proven_dead_takeover(
    lock_path: &Path,
    previous_owner: LockRecord,
    takeover_nonce: &str,
) {
    let mut guard = PROVEN_DEAD_TAKEOVERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let map = guard.get_or_insert_with(HashMap::new);
    map.insert(
        lock_key(lock_path),
        ProvenDeadTakeover {
            previous: previous_owner,
            takeover_nonce: takeover_nonce.to_string(),
        },
    );
}

pub fn proven_dead_takeover_for(lock_path: &Path, acquisition_nonce: &str) -> Option<LockRecord> {
    let guard = PROVEN_DEAD_TAKEOVERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let map = guard.as_ref()?;
    let entry = map.get(&lock_key(lock_path))?;
    (entry.takeover_nonce == acquisition_nonce).then(|| entry.previous.clone())
}

pub fn forget_proven_dead_takeover(lock_path: &Path, acquisition_nonce: &str) {
    let mut guard = PROVEN_DEAD_TAKEOVERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(map) = guard.as_mut() {
        let key = lock_key(lock_path);
        if map
            .get(&key)
            .is_some_and(|entry| entry.takeover_nonce == acquisition_nonce)
        {
            map.remove(&key);
        }
    }
}
