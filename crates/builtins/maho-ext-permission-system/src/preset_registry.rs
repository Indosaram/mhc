//! Port of `permission-system/preset-registry.ts` (senpi 903b8336).
//!
//! The preset the permission system enforces for one session. Upstream keys a `WeakMap` by the
//! session's `SessionManager` object; Rust keys by the session id string, and the retire closure
//! compares reader identity (`Arc::ptr_eq`) so retiring an older load never removes a newer one.

use crate::types::PermissionPresetName;
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};

pub type PresetReader = Arc<dyn Fn() -> Option<PermissionPresetName> + Send + Sync>;

static READERS: LazyLock<Mutex<HashMap<String, PresetReader>>> = LazyLock::new(Default::default);

fn readers() -> std::sync::MutexGuard<'static, HashMap<String, PresetReader>> {
    READERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Registers `read` for `session`; the returned retire function removes it only while it is still
/// the registered reader.
pub fn register_permission_preset_reader(
    session: &str,
    read: PresetReader,
) -> impl FnOnce() + Send + use<> {
    readers().insert(session.to_owned(), Arc::clone(&read));
    let session = session.to_owned();
    move || {
        let mut readers = readers();
        if readers
            .get(&session)
            .is_some_and(|current| Arc::ptr_eq(current, &read))
        {
            readers.remove(&session);
        }
    }
}

/// `None` when no permission system runs for the session, or its setup failed.
pub fn read_permission_preset(session: &str) -> Option<PermissionPresetName> {
    let reader = readers().get(session).cloned()?;
    reader()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retiring_an_older_reader_keeps_the_newer_one() {
        let older: PresetReader = Arc::new(|| Some(PermissionPresetName::Ask));
        let retire_older = register_permission_preset_reader("preset-registry-test", older);
        let newer: PresetReader = Arc::new(|| Some(PermissionPresetName::Auto));
        let retire_newer = register_permission_preset_reader("preset-registry-test", newer);
        retire_older();
        assert_eq!(
            read_permission_preset("preset-registry-test"),
            Some(PermissionPresetName::Auto)
        );
        retire_newer();
        assert_eq!(read_permission_preset("preset-registry-test"), None);
    }

    #[test]
    fn unknown_session_has_no_preset() {
        assert_eq!(read_permission_preset("preset-registry-missing"), None);
    }
}
