use maho_ext_rules::rules::{project_root::find_project_root, truncator::{truncate_rule, truncate_budget, BudgetRule}, tool_paths::extract_tool_paths};
use maho_ext_api::ToolResultEvent;
#[test]
fn missing_root_marker_terminates_at_filesystem_root() { let dir = tempfile::tempdir().unwrap(); assert!(find_project_root(dir.path(), Some(&[".nonexistent-marker-senpi"])).is_none()); }
#[test]
fn root_discovery_finds_nearest_marker() { let dir = tempfile::tempdir().unwrap(); let nested = dir.path().join("nested"); std::fs::create_dir(&nested).unwrap(); std::fs::write(dir.path().join("Cargo.toml"), "").unwrap(); assert_eq!(find_project_root(&nested, None), Some(dir.path().to_path_buf())); }
#[test]
fn missing_start_returns_none() { let dir = tempfile::tempdir().unwrap(); assert!(find_project_root(&dir.path().join("missing"), None).is_none()); }
#[test]
fn small_rule_is_not_truncated() { let result = truncate_rule("a😀", 3, "r"); assert_eq!((result.body.as_str(), result.original_length, result.truncated), ("a😀", 3, false)); }
#[test]
fn truncation_notice_survives_smaller_than_notice_budget() { let result = truncate_rule("abcdef", 1, "r"); assert!(result.truncated); assert!(result.body.ends_with("r]")); }
#[test]
fn budget_drops_rule_when_notice_cannot_fit() { let result = truncate_budget(&[BudgetRule { body:"abcdef".into(), relative_path:"r".into() }], 1); assert!(result.is_empty()); }
#[test]
fn failed_tool_result_has_no_paths() { let event = ToolResultEvent { tool_name:"read".into(), tool_call_id:"c".into(), input:serde_json::json!({"path":"file"}), content:vec![], details:None, is_error:true, usage:None }; assert!(extract_tool_paths(&event, std::path::Path::new("/workspace")).is_empty()); }
#[test]
fn read_paths_are_resolved_and_deduplicated() { let event = ToolResultEvent { tool_name:"read".into(), tool_call_id:"c".into(), input:serde_json::json!({"path":"file"}), content:vec![], details:Some(serde_json::json!({"filePath":"/workspace/file"})), is_error:false, usage:None }; assert_eq!(extract_tool_paths(&event, std::path::Path::new("/workspace")), vec![std::path::PathBuf::from("/workspace/file")]); }
