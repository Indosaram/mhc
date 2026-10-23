//! Tests matching upstream `test/permission-guidance.test.ts`.

use std::collections::HashMap;
use maho_tools::definition::ToolCall;
use senpi_desktop_protocol::PermissionDeniedData;
use senpi_desktop_tool::cua_errors::computer_failure;
use senpi_desktop_tool::tool::{create_computer_tool, ComputerToolDeps};
use serde_json::json;

use crate::fixtures;
use fixtures::{desktop_fixture, methods_of, TestToolContext};

#[tokio::test]
async fn carries_engine_permission_metadata_through_public_computer_result() {
    let mut env = HashMap::new();
    env.insert("FAKE_ENGINE_INPUT_ERROR".into(), "PermissionDenied".into());
    let fixture = desktop_fixture(None, env);

    let tool = create_computer_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });

    let input = json!({
        "action": "call",
        "chain": [{ "method": "press", "args": [["enter"]] }]
    });

    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "session-1", "/sessions/project");
    let call = ToolCall {
        id: "permission",
        params: input,
        signal: maho_tools::definition::AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };

    let result = (tool.execute)(call).await.expect("execute");
    let details = result.details.expect("details");
    assert_eq!(details.get("isError").and_then(|v| v.as_bool()), Some(true));

    let val = details.get("value").expect("value");
    assert_eq!(val.get("code").and_then(|v| v.as_str()), Some("COMPUTER_PERMISSION_REQUIRED"));
    assert_eq!(val.get("permission").and_then(|v| v.as_str()), Some("accessibility"));
    assert_eq!(val.get("settingsUrl").and_then(|v| v.as_str()), Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"));
    assert_eq!(val.get("app").and_then(|v| v.as_str()), Some("QA App"));
    assert_eq!(val.get("relaunchRequired").and_then(|v| v.as_bool()), Some(true));

    let methods = methods_of(&fixture.log).await;
    assert_eq!(methods.iter().filter(|m| m.as_str() == "keyChord").count(), 1);
}

#[tokio::test]
async fn marks_denial_inside_details_so_code_mode_marshal_reports_has_error() {
    let mut env = HashMap::new();
    env.insert("FAKE_ENGINE_INPUT_ERROR".into(), "PermissionDenied".into());
    let fixture = desktop_fixture(None, env);

    let tool = create_computer_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });

    let input = json!({
        "action": "call",
        "chain": [{ "method": "press", "args": [["enter"]] }]
    });

    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "session-1", "/sessions/project");
    let call = ToolCall {
        id: "permission",
        params: input,
        signal: maho_tools::definition::AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };

    let result = (tool.execute)(call).await.expect("execute");
    let details = result.details.expect("details");
    assert_eq!(details.get("isError").and_then(|v| v.as_bool()), Some(true));
    let val = details.get("value").expect("value");
    assert_eq!(val.get("code").and_then(|v| v.as_str()), Some("COMPUTER_PERMISSION_REQUIRED"));
}

#[test]
fn retains_engine_message_while_exposing_same_permission_payload() {
    let data = PermissionDeniedData {
        permission: senpi_desktop_protocol::TccPermission::ScreenRecording,
        settings_url: "settings:capture".to_string(),
        app: "QA App".to_string(),
        relaunch_required: true,
    };
    let error = computer_failure("PermissionDenied", "unique engine diagnostic", "ctrl+escape", Some(&data));
    assert_eq!(error.code, "COMPUTER_PERMISSION_REQUIRED");
    assert_eq!(error.permission.as_deref(), Some("screen_recording"));
    assert_eq!(error.settings_url.as_deref(), Some("settings:capture"));
    assert_eq!(error.app.as_deref(), Some("QA App"));
    assert_eq!(error.relaunch_required, Some(true));
    assert!(error.message.starts_with("COMPUTER_PERMISSION_REQUIRED: unique engine diagnostic"));
}
