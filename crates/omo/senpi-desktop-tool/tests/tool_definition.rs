//! Tests matching upstream `test/tool-definition.test.ts`.

use std::sync::Arc;
use maho_tools::definition::{ToolDefinition, ToolExposure};
use senpi_desktop_service::service::DesktopService;
use senpi_desktop_tool::activation::ComputerHandle;
use senpi_desktop_tool::params::parse_computer_params;
use senpi_desktop_tool::settings::resolve_computer_settings;
use senpi_desktop_tool::tool::{create_computer_tool, ComputerToolDeps};
use senpi_desktop_tool::tool_definition::{
    computer_tool_definition, kernel_prelude_for_computer, safety_guidelines,
};
use serde_json::json;

fn dummy_tool() -> ToolDefinition {
    let service = Arc::new(DesktopService::new(Default::default()));
    let handle = Arc::new(ComputerHandle::new(service, || resolve_computer_settings(None, "linux").expect("default Linux settings")));
    create_computer_tool(ComputerToolDeps {
        handle,
        execute_tool: None,
    })
}

#[test]
fn is_search_exposed_under_computer_name() {
    let def = dummy_tool();
    assert_eq!(def.name, "computer");
    assert_eq!(def.exposure, Some(ToolExposure::Search));
    assert_eq!(def.search_group.as_deref(), Some("desktop"));
}

#[test]
fn indexes_cua_keyword_for_tool_search() {
    let def = dummy_tool();
    let keywords = def.search_keywords.expect("keywords");
    assert!(keywords.iter().any(|k| k == "cua"));
}

#[test]
fn keeps_registration_shells_search_surface_and_prompt_guidelines() {
    let bullets = safety_guidelines();
    let def = computer_tool_definition();
    assert_eq!(def.prompt_guidelines, Some(bullets));
    assert_eq!(
        def.search_text.as_deref(),
        Some("Operate the real desktop (experimental): screenshots, clicks, typing, key chords, window list, accessibility tree, clipboard; macOS/Linux/Windows")
    );
    let kp = kernel_prelude_for_computer();
    assert_eq!(kp.exports, vec!["computer"]);
}

#[test]
fn contributes_exactly_computer_global_to_eval_kernels() {
    let kp = kernel_prelude_for_computer();
    assert_eq!(kp.exports, vec!["computer"]);
}

#[test]
fn publishes_one_root_object_schema_with_action_enum() {
    let def = dummy_tool();
    let schema = def.parameters;
    assert_eq!(schema.get("type").and_then(|v| v.as_str()), Some("object"));
    assert!(schema.get("anyOf").is_none());
    assert!(schema.get("oneOf").is_none());
    let act_enum = schema["properties"]["action"]["enum"].as_array().expect("enum");
    let names: Vec<_> = act_enum.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(names, vec!["call", "run", "capabilities", "close"]);
}

// 4 it.each for accepts action
#[test]
fn accepts_call_action() {
    let params = json!({ "action": "call", "chain": [{ "method": "screenshot" }] });
    assert!(parse_computer_params(&params).is_ok());
}

#[test]
fn accepts_run_action() {
    let params = json!({ "action": "run", "code": "return 1", "read_only": true, "timeout": 5 });
    assert!(parse_computer_params(&params).is_ok());
}

#[test]
fn accepts_capabilities_action() {
    let params = json!({ "action": "capabilities" });
    assert!(parse_computer_params(&params).is_ok());
}

#[test]
fn accepts_close_action() {
    let params = json!({ "action": "close" });
    assert!(parse_computer_params(&params).is_ok());
}

// 5 it.each for rejects with COMPUTER_INVALID_ARGUMENTS
#[test]
fn rejects_unknown_action_resume() {
    let params = json!({ "action": "resume" });
    let err = parse_computer_params(&params).unwrap_err();
    assert_eq!(err.code, "COMPUTER_INVALID_ARGUMENTS");
    assert!(err.reason.contains(r#"unknown action "resume""#));
}

#[test]
fn rejects_capabilities_with_extra_token() {
    let params = json!({ "action": "capabilities", "token": "stolen" });
    let err = parse_computer_params(&params).unwrap_err();
    assert_eq!(err.code, "COMPUTER_INVALID_ARGUMENTS");
    assert!(err.reason.contains(r#"action "capabilities" does not take token"#));
}

#[test]
fn rejects_run_with_timeout_0() {
    let params = json!({ "action": "run", "code": "return 1", "timeout": 0 });
    let err = parse_computer_params(&params).unwrap_err();
    assert_eq!(err.code, "COMPUTER_INVALID_ARGUMENTS");
    assert!(err.reason.contains("must be >= 1"));
}

#[test]
fn rejects_call_without_chain() {
    let params = json!({ "action": "call" });
    let err = parse_computer_params(&params).unwrap_err();
    assert_eq!(err.code, "COMPUTER_INVALID_ARGUMENTS");
    assert!(err.reason.contains("must have required properties chain"));
}

#[test]
fn rejects_close_with_code() {
    let params = json!({ "action": "close", "code": "x" });
    let err = parse_computer_params(&params).unwrap_err();
    assert_eq!(err.code, "COMPUTER_INVALID_ARGUMENTS");
    assert!(err.reason.contains(r#"action "close" does not take code"#));
}

#[test]
fn imports_no_permission_system_or_coding_agent() {
    // Structural architectural boundary check: senpi-desktop-tool does not link coding-agent or permission-system
    let cargo_manifest = include_str!("../Cargo.toml");
    assert!(!cargo_manifest.contains("maho-ext-permission-system"));
    assert!(!cargo_manifest.contains("maho-agent"));
}

#[test]
fn never_calls_evaluate_or_show_permission_prompt() {
    // Structural architectural boundary check: evaluate or showPermissionPrompt are not called in tool
    let src = include_str!("../src/tool.rs");
    assert!(!src.contains("evaluate("));
    assert!(!src.contains("show_permission_prompt("));
    assert!(!src.contains("showPermissionPrompt("));
}
