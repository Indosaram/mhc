//! The supervisor's scratch helpers (senpi `rpc/host-lifecycle-scratch.ts`). The internal socket path and the
//! child pid record are implemented in `host_lifecycle` (`create_internal_socket_path`, `record_child_pid`) and
//! re-exported here; the settings reader lives here.
pub use crate::host_lifecycle::{create_internal_socket_path, record_child_pid};
use std::path::Path;

/// The settings file as JSON, or `None` when it is missing or does not parse (senpi `readSettingsFile`).
pub fn read_settings_file(settings_file: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(settings_file).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreadable_and_torn_settings_read_as_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("settings.json");
        assert_eq!(read_settings_file(&file), None);
        std::fs::write(&file, "{\"idleExitMs\":5").expect("write");
        assert_eq!(read_settings_file(&file), None);
        std::fs::write(&file, "{\"idleExitMs\":5}").expect("write");
        assert_eq!(
            read_settings_file(&file),
            Some(serde_json::json!({"idleExitMs":5}))
        );
    }
}
