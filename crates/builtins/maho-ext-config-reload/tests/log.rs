use std::{fs, os::unix::fs::PermissionsExt};
use maho_ext_config_reload::log::{ConfigReloadLogger, LogEvent, LogLevel};

#[test]
fn writes_parseable_jsonl_entries_and_private_permissions() {
    let root = tempfile::tempdir().unwrap();
    let mut logger = ConfigReloadLogger::new(root.path(), None).unwrap();
    let status = logger.log(LogLevel::Info, LogEvent::ChangeDetected { registration_id: "settings", paths: &["/workspace/settings.json".into()], deferred: false });
    assert!(status.written && !status.disabled);
    let path = root.path().join("logs/config-reload.log");
    let content = fs::read_to_string(&path).unwrap();
    let entry: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
    assert_eq!(entry["level"], "info");
    assert_eq!(entry["event"], "change_detected");
    assert_eq!(entry["paths"], serde_json::json!(["/workspace/settings.json"]));
    assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o600);
}
#[test]
fn rotates_previous_entry_when_size_cap_would_be_exceeded() {
    let root = tempfile::tempdir().unwrap();
    let mut logger = ConfigReloadLogger::new(root.path(), Some(130)).unwrap();
    logger.log(LogLevel::Info, LogEvent::RegistrationAdded { id: "first-registration" });
    logger.log(LogLevel::Info, LogEvent::RegistrationAdded { id: "second-registration" });
    let old: serde_json::Value = serde_json::from_str(fs::read_to_string(root.path().join("logs/config-reload.log.1")).unwrap().trim()).unwrap();
    let new: serde_json::Value = serde_json::from_str(fs::read_to_string(root.path().join("logs/config-reload.log")).unwrap().trim()).unwrap();
    assert_eq!(old["id"], "first-registration");
    assert_eq!(new["id"], "second-registration");
}
#[test]
fn write_failure_disables_future_attempts() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("logs"), "blocks directory").unwrap();
    let mut logger = ConfigReloadLogger::new(root.path(), None).unwrap();
    let first = logger.log(LogLevel::Warn, LogEvent::WatcherStarted { target_count: 1.0 });
    let next = logger.log(LogLevel::Warn, LogEvent::WatcherStarted { target_count: 2.0 });
    assert!(!first.written && first.disabled);
    assert_eq!(first, next);
}
#[test]
fn sensitive_paths_are_omitted_and_text_is_redacted() {
    let root = tempfile::tempdir().unwrap();
    let mut logger = ConfigReloadLogger::new(root.path(), None).unwrap();
    logger.log(LogLevel::Error, LogEvent::WatcherError { path: "/workspace/auth.json", message: "Authorization: Bearer fixture-not-a-credential" });
    let entry: serde_json::Value = serde_json::from_str(fs::read_to_string(root.path().join("logs/config-reload.log")).unwrap().trim()).unwrap();
    assert!(entry.get("path").is_none());
    assert_eq!(entry["message"], "Authorization: Bearer [redacted]");
}
