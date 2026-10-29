//! Acquisition-scoped provenance of proven-dead takeovers.
use super::lock_record::LockRecord;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
fn proofs() -> &'static Mutex<HashMap<PathBuf, (String, LockRecord)>> {
    static PROOFS: OnceLock<Mutex<HashMap<PathBuf, (String, LockRecord)>>> = OnceLock::new();
    PROOFS.get_or_init(Mutex::default)
}
pub fn remember_proven_dead_takeover(path: &Path, nonce: &str, previous: LockRecord) {
    proofs()
        .lock()
        .expect("takeover mutex")
        .insert(path.to_path_buf(), (nonce.to_string(), previous));
}
pub fn proven_dead_takeover_for(path: &Path, nonce: &str) -> Option<LockRecord> {
    proofs()
        .lock()
        .expect("takeover mutex")
        .get(path)
        .filter(|(current, _)| current == nonce)
        .map(|(_, owner)| owner.clone())
}
pub fn forget_proven_dead_takeover(path: &Path, nonce: &str) {
    let mut proofs = proofs().lock().expect("takeover mutex");
    if proofs
        .get(path)
        .is_some_and(|(current, _)| current == nonce)
    {
        proofs.remove(path);
    }
}
