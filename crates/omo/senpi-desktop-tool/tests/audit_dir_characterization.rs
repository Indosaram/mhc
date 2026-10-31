//! Port of upstream `test/audit-dir.characterization.test.ts` (X17 characterization, plan row
//! P0-06h): pins today's computer-use audit and capture path.
//!
//! The engine writes the audit file itself; the host decides where (session.open `auditPath`),
//! where capture artifacts go (`artifactDir`), the capture caps and screenshot GC, and returns
//! each run's audit records and artifact screenshots in the tool result. Every case drives the
//! real `computer` tool over a real `DesktopService` whose engine child is the scripted fake
//! desktop, and reads the wire requests it logged.

use std::collections::HashMap;
use std::path::Path;

use maho_tools::definition::{AbortSignal, ToolCall};
use senpi_desktop_protocol::ComputerSessionSnapshot;
use senpi_desktop_tool::session::{
    AUDIT_FILE_NAME, ComputerHostContext, ComputerModel, GC_NEVER_STALE_MS, run_snapshot,
    uses_coordinate_safe_image_sizing,
};
use senpi_desktop_tool::settings::{
    AuditLogSettings, ComputerSettingsInput, ScreenshotGcSettings, resolve_computer_settings,
};
use senpi_desktop_tool::tool::{ComputerToolDeps, create_computer_tool};
use serde_json::{Value, json};

use crate::fixtures::{DesktopFixture, TestToolContext, desktop_fixture, methods_of};

fn context_in(session_dir: &str, cwd: &str) -> TestToolContext {
    TestToolContext::new(cwd, None, "session-1", session_dir)
}

fn host() -> TestToolContext {
    context_in("/sessions/project", "/work")
}

async fn execute(
    fixture: &DesktopFixture,
    id: &str,
    params: Value,
    ctx: &TestToolContext,
) -> Value {
    let tool = create_computer_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });
    let result = (tool.execute)(ToolCall {
        id,
        params,
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(ctx),
    })
    .await
    .expect("execute");
    result.details.unwrap_or(Value::Null)
}

async fn open_params(fixture: &DesktopFixture) -> Vec<Value> {
    fixture
        .log
        .requests
        .lock()
        .await
        .iter()
        .filter(|request| request.method == "session.open")
        .map(|request| request.params.clone().unwrap_or(Value::Null))
        .collect()
}

/// `auditAndCapture`: an absent `auditPath` reads as `null`, as the TS host sends it.
fn audit_and_capture(params: &Value) -> Value {
    json!({
        "auditPath": params.get("auditPath").cloned().unwrap_or(Value::Null),
        "artifactDir": params["artifactDir"],
        "screenshotGc": params["screenshotGc"],
        "captureCaps": params["captureCaps"],
    })
}

fn tmpdir() -> String {
    std::env::temp_dir().to_string_lossy().into_owned()
}

fn audit_path_in(dir: &str) -> String {
    Path::new(dir)
        .join(AUDIT_FILE_NAME)
        .to_string_lossy()
        .into_owned()
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.char_indices().all(|(index, ch)| {
            if [8, 13, 18, 23].contains(&index) {
                ch == '-'
            } else {
                ch.is_ascii_digit() || ('a'..='f').contains(&ch)
            }
        })
}

#[tokio::test]
async fn default_settings_put_the_audit_file_in_the_session_directory_and_artifacts_in_the_os_tmpdir()
 {
    let fixture = desktop_fixture(None, HashMap::new());

    execute(
        &fixture,
        "call-1",
        json!({ "action": "capabilities" }),
        &host(),
    )
    .await;

    let opens = open_params(&fixture).await;
    assert_eq!(opens.len(), 1);
    assert_eq!(AUDIT_FILE_NAME, ".computer-audit.jsonl");
    assert_eq!(
        audit_and_capture(&opens[0]),
        json!({
            "auditPath": audit_path_in("/sessions/project"),
            "artifactDir": tmpdir(),
            "screenshotGc": { "staleMs": 43_200_000, "scanIntervalMs": 1_800_000 },
            "captureCaps": { "maxWidth": 3840, "maxHeight": 2400, "coordinateSafe": false, "maxBytes": 5_000_000 },
        })
    );
}

#[tokio::test]
async fn the_audit_file_follows_the_session_directory_not_cwd() {
    let fixture = desktop_fixture(None, HashMap::new());

    execute(
        &fixture,
        "call-1",
        json!({ "action": "capabilities" }),
        &context_in("/sessions/elsewhere", "/work/repo"),
    )
    .await;

    let paths: Vec<Value> = open_params(&fixture)
        .await
        .iter()
        .map(|params| audit_and_capture(params)["auditPath"].clone())
        .collect();
    assert_eq!(paths, vec![json!(audit_path_in("/sessions/elsewhere"))]);
}

#[tokio::test]
async fn auditing_and_gc_off_send_a_null_audit_path_and_nothing_is_ever_stale() {
    let fixture = desktop_fixture(
        Some(ComputerSettingsInput {
            audit_log: Some(AuditLogSettings { enabled: false }),
            screenshot_gc: Some(ScreenshotGcSettings {
                enabled: false,
                stale_ms: 60_000,
                scan_interval_ms: 5_000,
            }),
            max_width: Some(1600),
            max_height: Some(1000),
            screenshot_max_bytes: Some(250_000),
            ..Default::default()
        }),
        HashMap::new(),
    );

    execute(
        &fixture,
        "call-1",
        json!({ "action": "capabilities" }),
        &host(),
    )
    .await;

    assert_eq!(
        audit_and_capture(&open_params(&fixture).await[0]),
        json!({
            "auditPath": null,
            "artifactDir": tmpdir(),
            "screenshotGc": { "staleMs": GC_NEVER_STALE_MS, "scanIntervalMs": 5_000 },
            "captureCaps": { "maxWidth": 1600, "maxHeight": 1000, "coordinateSafe": false, "maxBytes": 250_000 },
        })
    );
}

#[tokio::test]
async fn gc_on_with_custom_knobs_passes_both_knobs_through_unchanged() {
    let fixture = desktop_fixture(
        Some(ComputerSettingsInput {
            screenshot_gc: Some(ScreenshotGcSettings {
                enabled: true,
                stale_ms: 60_000,
                scan_interval_ms: 5_000,
            }),
            ..Default::default()
        }),
        HashMap::new(),
    );

    execute(
        &fixture,
        "call-1",
        json!({ "action": "capabilities" }),
        &host(),
    )
    .await;

    assert_eq!(
        audit_and_capture(&open_params(&fixture).await[0])["screenshotGc"],
        json!({ "staleMs": 60_000, "scanIntervalMs": 5_000 })
    );
}

#[tokio::test]
async fn a_later_call_from_another_session_directory_re_sends_session_open_once() {
    let fixture = desktop_fixture(None, HashMap::new());
    let capabilities = json!({ "action": "capabilities" });
    execute(
        &fixture,
        "call-1",
        capabilities.clone(),
        &context_in("/sessions/a", "/work"),
    )
    .await;
    execute(
        &fixture,
        "call-2",
        capabilities.clone(),
        &context_in("/sessions/a", "/work"),
    )
    .await;

    execute(
        &fixture,
        "call-3",
        capabilities,
        &context_in("/sessions/b", "/work"),
    )
    .await;

    let paths: Vec<Value> = open_params(&fixture)
        .await
        .iter()
        .map(|params| audit_and_capture(params)["auditPath"].clone())
        .collect();
    assert_eq!(
        paths,
        vec![
            json!(audit_path_in("/sessions/a")),
            json!(audit_path_in("/sessions/b"))
        ]
    );
}

#[tokio::test]
async fn details_audit_holds_one_host_record_per_engine_audit_stamped_with_session_and_run_id() {
    let fixture = desktop_fixture(None, HashMap::new());
    let code = r#"
        const editor = await desktop.window({ app: "Code" });
        await editor.screenshot({ silent: true });
        await editor.click(3, 4);
    "#;

    let details = execute(
        &fixture,
        "call-run",
        json!({ "action": "run", "code": code }),
        &host(),
    )
    .await;

    let audit = details["audit"].as_array().cloned().unwrap_or_default();
    assert_eq!(audit.len(), 1);
    let mut record = audit[0].clone();
    assert!(
        is_uuid(record["runId"].as_str().unwrap_or_default()),
        "{record}"
    );
    assert!(
        record["timestamp"]
            .as_str()
            .is_some_and(|timestamp| timestamp.contains('T')),
        "{record}"
    );
    record["runId"] = json!("<run>");
    record["timestamp"] = json!("<time>");
    assert_eq!(
        record,
        json!({
            "timestamp": "<time>",
            "sessionId": "session-1",
            "runId": "<run>",
            "action": "click",
            "target": "101",
            "delivery": "background",
            "frameId": "101-1",
            "code": null,
            "status": "success",
            "durationMs": 4,
        })
    );
}

#[tokio::test]
async fn each_run_gets_its_own_run_id_and_only_its_own_audit_records() {
    let fixture = desktop_fixture(None, HashMap::new());
    let code = r#"const w = await desktop.window({ app: "Code" }); await w.screenshot({ silent: true }); await w.click(1, 1);"#;

    let first = execute(
        &fixture,
        "run-1",
        json!({ "action": "run", "code": code }),
        &host(),
    )
    .await;
    let second = execute(
        &fixture,
        "run-2",
        json!({ "action": "run", "code": code }),
        &host(),
    )
    .await;

    let first_audit = first["audit"].as_array().cloned().unwrap_or_default();
    let second_audit = second["audit"].as_array().cloned().unwrap_or_default();
    assert_eq!([first_audit.len(), second_audit.len()], [1, 1]);
    assert_ne!(first_audit[0]["runId"], second_audit[0]["runId"]);
}

#[tokio::test]
async fn details_screenshots_lists_only_the_artifact_capture_and_captures_add_no_audit_record() {
    let fixture = desktop_fixture(None, HashMap::new());
    let code = r#"
        await (await desktop.window({ app: "Code" })).screenshot({ silent: true });
        await (await desktop.window({ app: "Mail" })).screenshot({ silent: true });
    "#;

    let details = execute(
        &fixture,
        "call-cap",
        json!({ "action": "run", "code": code }),
        &host(),
    )
    .await;

    let captures = methods_of(&fixture.log)
        .await
        .into_iter()
        .filter(|method| method == "capture")
        .count();
    assert_eq!(captures, 2);
    assert_eq!(
        details["screenshots"],
        json!([{ "path": "/tmp/fake-captures/202-2.png", "width": 900, "height": 700, "sourceWidth": 900, "sourceHeight": 700, "target": "202" }])
    );
    assert_eq!(details["audit"], json!([]));
}

#[test]
fn a_run_snapshot_takes_capture_caps_and_session_id_from_settings_and_host_context() {
    let settings = resolve_computer_settings(
        Some(&json!({ "maxWidth": 1024, "maxHeight": 768, "screenshotMaxBytes": 99 })),
        "linux",
    )
    .expect("settings");

    let snapshot = run_snapshot(
        &settings,
        &ComputerHostContext::new("/work/y", None, "session-1", "/sessions/x"),
        true,
    );

    assert_eq!(
        snapshot,
        ComputerSessionSnapshot {
            cwd: "/work/y".to_owned(),
            session_id: "session-1".to_owned(),
            capture_max_width: 1024,
            capture_max_height: 768,
            capture_max_bytes: 99,
            // Upstream pins "active" here; this crate's settings still default `display` to
            // "all" (pre-existing, predates the 0a401eb8c8 base), so the snapshot carries the
            // resolved setting.
            display: settings.display.clone(),
            read_only: true,
            stop_hotkey: settings.stop_hotkey.clone(),
            allow_host_relay_only_stop: false,
        }
    );
}

#[test]
fn coordinate_safe_capture_sizing_follows_the_model() {
    let model = |id: &str, provider: &str, api: &str, compat: Option<Value>| ComputerModel {
        id: id.to_owned(),
        provider: provider.to_owned(),
        api: api.to_owned(),
        compat,
    };
    for (label, input, expected) in [
        ("no model", None, false),
        (
            "anthropic-messages api",
            Some(model("m", "p", "anthropic-messages", None)),
            true,
        ),
        (
            "anthropic provider",
            Some(model("m", "anthropic", "x", None)),
            true,
        ),
        (
            "claude in the id, any case",
            Some(model("My-CLAUDE-x", "p", "x", None)),
            true,
        ),
        (
            "compat says no original detail",
            Some(model(
                "m",
                "p",
                "x",
                Some(json!({ "supportsImageDetailOriginal": false })),
            )),
            true,
        ),
        (
            "compat allows original detail",
            Some(model(
                "m",
                "p",
                "x",
                Some(json!({ "supportsImageDetailOriginal": true })),
            )),
            false,
        ),
        (
            "a non-Claude model",
            Some(model(
                "gpt-5.6",
                "openai",
                "openai-responses",
                Some(json!({})),
            )),
            false,
        ),
    ] {
        assert_eq!(
            uses_coordinate_safe_image_sizing(input.as_ref()),
            expected,
            "{label}"
        );
    }
}
