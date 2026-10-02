use std::{fs, os::unix::fs::PermissionsExt};
use maho_ext_compaction::log::{CompactionLogger, format_line};
use serde_json::{Value, json};

#[test]
fn javascript_log_graph_preserves_coercions_and_shared_identity() {
    use maho_ext_compaction::log::{LogValue, safe_graph_value};
    let values = vec![LogValue::Object(vec![("tokens".into(),1),("origin".into(),2),("reason".into(),3),("count".into(),4)]),
        LogValue::BigInt("9007199254740993".into()), LogValue::Array(vec![5,6,7,2,0]), LogValue::Undefined,
        LogValue::Json(json!(42)), LogValue::Symbol("Symbol(route)".into()), LogValue::Function("function route() {}".into()), LogValue::Undefined];
    assert_eq!(safe_graph_value(&values,0),Some(json!({"tokens":"9007199254740993","count":42,"origin":["Symbol(route)","function route() {}",null,"[Circular]","[Circular]"]})));
    let line = maho_ext_compaction::log::format_graph_line("fixed", "debug", "idle_trigger", &values, &[("origin".into(),0),("reason".into(),0)]);
    let parsed: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(parsed["origin"], parsed["reason"]);
    assert!(parsed["origin"].is_object());
}

#[test]
fn allowlist_removes_nested_sensitive_fields() {
    let data = json!({"tokens":42,"secret":"credential","origin":{"route":"local","secret":"credential"}});
    let line = format_line("2026-10-01T00:00:00.000Z", "debug", "idle_trigger", data.as_object());
    let parsed: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(parsed["tokens"], 42);
    assert_eq!(parsed["origin"], json!({"route":"local"}));
    assert!(parsed.get("secret").is_none());
}

#[test]
fn logger_writes_jsonl_with_restricted_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let mut logger = CompactionLogger::new(Some(dir.path()), None, Some(false));
    logger.info("idle_trigger", None);
    let path = dir.path().join("logs/compaction.log");
    let text = fs::read_to_string(&path).unwrap();
    let parsed: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(parsed["event"], "idle_trigger");
    assert_eq!(parsed["level"], "info");
    assert!(text.ends_with('\n'));
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(dir.path().join("logs")).unwrap().permissions().mode() & 0o777, 0o700);
}

#[test]
fn logger_rotates_before_appending_over_limit() {
    let dir = tempfile::tempdir().unwrap();
    let mut logger = CompactionLogger::new(Some(dir.path()), Some(1), Some(false));
    logger.debug("idle_trigger", None);
    logger.info("idle_applied", None);
    let first: Value = serde_json::from_str(fs::read_to_string(dir.path().join("logs/compaction.log.1")).unwrap().trim()).unwrap();
    let second: Value = serde_json::from_str(fs::read_to_string(dir.path().join("logs/compaction.log")).unwrap().trim()).unwrap();
    assert_eq!(first["event"], "idle_trigger");
    assert_eq!(second["event"], "idle_applied");
}

#[test]
fn missing_agent_directory_disables_sink_and_filesystem() {
    let mut logger = CompactionLogger::new(None, None, Some(false));
    logger.log("debug", "idle_trigger", None, Some(&|_| panic!("sink invoked")));
}

#[test]
fn unknown_events_are_not_written() {
    let dir = tempfile::tempdir().unwrap();
    let mut logger = CompactionLogger::new(Some(dir.path()), None, Some(false));
    logger.info("unknown", None);
    assert!(!dir.path().join("logs").exists());
}

#[test]
fn sink_observes_line_before_filesystem_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("logs/compaction.log");
    let called = std::cell::Cell::new(false);
    let mut logger = CompactionLogger::new(Some(dir.path()), None, Some(false));
    logger.log("debug", "idle_trigger", None, Some(&|line| {
        assert!(!path.exists());
        assert_eq!(serde_json::from_str::<Value>(line).unwrap()["event"], "idle_trigger");
        called.set(true);
    }));
    assert!(called.get());
    assert!(path.exists());
}
