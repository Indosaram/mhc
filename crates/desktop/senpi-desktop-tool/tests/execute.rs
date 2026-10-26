//! Tests matching upstream `test/execute.test.ts`.

use std::collections::HashMap;
use std::sync::Arc;
use maho_tools::definition::AbortSignal;
use maho_tools::definition::ToolCall;
use senpi_desktop_tool::session::{ComputerModel, AUDIT_FILE_NAME};
use senpi_desktop_tool::settings::ComputerSettingsInput;
use senpi_desktop_tool::tool::{create_computer_tool, ComputerToolDeps};
use serde_json::json;

use crate::fixtures;
use fixtures::{desktop_fixture, host_context, methods_of, TestToolContext};

#[tokio::test]
async fn explicit_null_return_is_displayed_and_string_return_is_unquoted() {
    let fixture = desktop_fixture(None, HashMap::new());
    let tool = create_computer_tool(ComputerToolDeps { handle: fixture.handle.clone(), execute_tool: None });
    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "value-session", "/sessions/project");
    for (code, expected) in [("return null;", "null"), ("return 'plain';", "plain")] {
        let result = (tool.execute)(ToolCall { id: "value", params: json!({"action": "run", "code": code, "read_only": true}), signal: AbortSignal::default(), on_update: None, context: Some(&ctx) }).await.expect("value result");
        assert!(matches!(result.content.as_slice(), [maho_tools::definition::ToolContent::Text { text, .. }] if text == expected));
    }
    fixture.handle.close().await.expect("close");
}

#[tokio::test]
async fn runs_window_screenshot_chain_through_facade_and_returns_inline_image() {
    let fixture = desktop_fixture(None, HashMap::new());
    let tool = create_computer_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });

    let input = json!({
        "action": "call",
        "chain": [
            { "method": "window", "args": ["101"] },
            { "method": "screenshot" }
        ]
    });

    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "session-1", "/sessions/project");
    let call = ToolCall {
        id: "call-1",
        params: input,
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };

    let result = (tool.execute)(call).await.expect("execute");
    let images = result.content.iter().filter(|c| matches!(c, maho_tools::definition::ToolContent::Image { .. })).count();
    assert_eq!(images, 1);

    let details = result.details.expect("details");
    let val = details.get("value").expect("value");
    assert_eq!(val.get("target").and_then(|v| v.as_str()), Some("101"));
    assert_eq!(val.get("frameId").and_then(|v| v.as_str()), Some("101-1"));
    assert_eq!(val.get("width").and_then(|v| v.as_u64()), Some(1200));
    assert_eq!(val.get("height").and_then(|v| v.as_u64()), Some(800));
}

#[tokio::test]
async fn rejects_user_reset_chain_as_unknown_method_before_engine_started() {
    let fixture = desktop_fixture(None, HashMap::new());
    let tool = create_computer_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });

    let input = json!({
        "action": "call",
        "chain": [{ "method": "userReset" }]
    });

    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "session-1", "/sessions/project");
    let call = ToolCall {
        id: "call-1",
        params: input,
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };

    let res = (tool.execute)(call).await;
    assert!(res.is_err());
    assert_eq!(fixture.log.children_count.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn refuses_input_inside_read_only_run_before_it_reaches_engine() {
    let fixture = desktop_fixture(None, HashMap::new());
    let tool = create_computer_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });

    let input = json!({
        "action": "run",
        "code": "await desktop.click(1, 1);",
        "read_only": true
    });

    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "session-1", "/sessions/project");
    let call = ToolCall {
        id: "call-1",
        params: input,
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };

    let res = (tool.execute)(call).await;
    assert!(res.is_err());
    let err_msg = res.unwrap_err().to_string();
    assert!(err_msg.contains("read-only run: 'click' requires read_only: false"));

    let methods = methods_of(&fixture.log).await;
    assert!(!methods.iter().any(|m| m == "click"));
}

#[tokio::test]
async fn reaches_host_tools_from_run_code_through_injected_execute_tool() {
    let fixture = desktop_fixture(None, HashMap::new());
    let exec_tool = Arc::new(|name: String, params: serde_json::Value, _sig| {
        Box::pin(async move {
            Ok(json!({ "name": name, "params": params }))
        }) as std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value, String>> + Send>>
    });

    let tool = create_computer_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: Some(exec_tool),
    });

    let input = json!({
        "action": "run",
        "code": r#"return await tool.read({ path: "a.txt" });"#
    });

    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "session-1", "/sessions/project");
    let call = ToolCall {
        id: "call-1",
        params: input,
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };

    let result = (tool.execute)(call).await.expect("execute");
    let details = result.details.expect("details");
    let val = details.get("value").expect("value");
    assert_eq!(val.get("name").and_then(|v| v.as_str()), Some("read"));
    assert_eq!(val["params"]["path"], "a.txt");
}

#[tokio::test]
async fn returns_live_capabilities_as_facade_value() {
    let fixture = desktop_fixture(None, HashMap::new());
    let tool = create_computer_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });

    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "session-1", "/sessions/project");
    let call = ToolCall {
        id: "call-1",
        params: json!({ "action": "capabilities" }),
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };

    let result = (tool.execute)(call).await.expect("execute");
    let details = result.details.expect("details");
    let val = details.get("value").expect("value");
    assert_eq!(val.get("backend").and_then(|v| v.as_str()), Some("fake"));
    assert_eq!(val.get("stopPath").and_then(|v| v.as_str()), Some("global"));
    assert_eq!(val.get("focusGuard").and_then(|v| v.as_bool()), Some(true));
}

#[tokio::test]
async fn opens_session_with_audit_path_and_coordinate_safe_caps_for_claude() {
    let settings = ComputerSettingsInput {
        max_width: Some(2000),
        screenshot_max_bytes: Some(1_000_000),
        ..Default::default()
    };
    let fixture = desktop_fixture(Some(settings), HashMap::new());

    let claude = ComputerModel {
        id: "claude-sonnet-4-6".into(),
        provider: "anthropic".into(),
        api: "anthropic-messages".into(),
        compat: None,
    };
    let ctx = host_context(Some(claude));
    fixture.handle.activate(&ctx).await.expect("activate");

    let reqs = fixture.log.requests.lock().await.clone();
    let open = reqs.iter().find(|r| r.method == "session.open").expect("open");
    let params = open.params.as_ref().unwrap();
    assert!(params.get("auditPath").and_then(|v| v.as_str()).unwrap().contains(AUDIT_FILE_NAME));
    assert_eq!(params["captureCaps"]["maxWidth"], 2000);
    assert_eq!(params["captureCaps"]["coordinateSafe"], true);
    assert_eq!(params["captureCaps"]["maxBytes"], 1_000_000);
}

#[tokio::test]
async fn turns_auditing_off_and_keeps_native_caps_when_settings_ask() {
    let mut settings = ComputerSettingsInput::default();
    settings.audit_log = Some(senpi_desktop_tool::settings::AuditLogSettings { enabled: false });
    let fixture = desktop_fixture(Some(settings), HashMap::new());

    let detailed_model = ComputerModel {
        id: "gpt-5.6".into(),
        provider: "openai".into(),
        api: "openai-responses".into(),
        compat: Some(json!({})),
    };
    let ctx = host_context(Some(detailed_model));
    fixture.handle.activate(&ctx).await.expect("activate");

    let reqs = fixture.log.requests.lock().await.clone();
    let open = reqs.iter().find(|r| r.method == "session.open").expect("open");
    let params = open.params.as_ref().unwrap();
    assert!(params.get("auditPath").is_none() || params.get("auditPath") == Some(&serde_json::Value::Null));
    assert_eq!(params["captureCaps"]["coordinateSafe"], false);
}

// 2 it.each parameter expansions for macosCanary
#[tokio::test]
async fn carries_session_canary_policy_into_engine_session() {
    let settings = ComputerSettingsInput { macos_canary: Some("session".into()), ..Default::default() };
    let fixture = desktop_fixture(Some(settings), HashMap::new());
    let ctx = host_context(None);
    fixture.handle.activate(&ctx).await.expect("activate");

    let reqs = fixture.log.requests.lock().await.clone();
    let open = reqs.iter().find(|r| r.method == "session.open").expect("open");
    assert_eq!(open.params.as_ref().unwrap().get("macosCanary").and_then(|v| v.as_str()), Some("session"));
}

#[tokio::test]
async fn carries_off_canary_policy_into_engine_session() {
    let settings = ComputerSettingsInput { macos_canary: Some("off".into()), ..Default::default() };
    let fixture = desktop_fixture(Some(settings), HashMap::new());
    let ctx = host_context(None);
    fixture.handle.activate(&ctx).await.expect("activate");

    let reqs = fixture.log.requests.lock().await.clone();
    let open = reqs.iter().find(|r| r.method == "session.open").expect("open");
    assert_eq!(open.params.as_ref().unwrap().get("macosCanary").and_then(|v| v.as_str()), Some("off"));
}

#[tokio::test]
async fn arms_configured_stop_chord_once_across_repeated_calls() {
    let settings = ComputerSettingsInput { stop_hotkey: Some("ctrl+alt+shift+f12".into()), ..Default::default() };
    let fixture = desktop_fixture(Some(settings), HashMap::new());
    let ctx = host_context(None);

    fixture.handle.activate(&ctx).await.expect("activate 1");
    fixture.handle.activate(&ctx).await.expect("activate 2");

    let reqs = fixture.log.requests.lock().await.clone();
    let starts: Vec<_> = reqs.iter().filter(|r| r.method == "stopPath.start").collect();
    let opens: Vec<_> = reqs.iter().filter(|r| r.method == "session.open").collect();

    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].params.as_ref().unwrap().get("chord").and_then(|v| v.as_str()), Some("ctrl+alt+shift+f12"));
    assert_eq!(opens.len(), 1);
}
