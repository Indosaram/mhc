use std::collections::HashMap;
use std::sync::Arc;

use maho_ai::utils::abort::AbortController;
use senpi_desktop_protocol::{ComputerDisplay, ComputerRunOk, ComputerSessionSnapshot};
use senpi_desktop_service::rpc_client::DesktopError;
use senpi_desktop_service::service::{DesktopService, DesktopServiceOptions};
use serde_json::json;

use senpi_desktop_run::context::{ComputerRunErrorReason, DesktopRunError};
use senpi_desktop_run::runtime::{run_computer_code, ComputerRunHost, ComputerRunRequest, ExecuteTool};

mod common;
use common::SpawnLog;

const RUN_TIMEOUT_MS: u64 = 5_000;

#[tokio::test]
async fn window_find_returns_all_typed_element_handles_with_wire_refs() {
    let (service, _) = open_desktop(HashMap::new()).await;
    let result = run_code(service.clone(), "const window = await desktop.window('101'); const elements = await window.find({role: 'button'}); return elements.map(e => e.ref);", true, None, None, None)
        .await.expect("element query");
    assert_eq!(result.return_value, Some(json!(["e1"])));
    service.close().await.expect("close");
}

#[tokio::test]
async fn returning_from_a_run_cancels_its_unawaited_host_work() {
    let (service, _) = open_desktop(HashMap::new()).await;
    let (cancelled_tx, cancelled_rx) = tokio::sync::oneshot::channel();
    let cancelled = Arc::new(std::sync::Mutex::new(Some(cancelled_tx)));
    let execute: ExecuteTool = Arc::new(move |name, _, signal| {
        let cancelled = cancelled.clone();
        Box::pin(async move {
            if name == "ready" { return Ok(serde_json::Value::Null); }
            signal.cancelled().await;
            cancelled.lock().expect("cancel lock").take().expect("one blocked call").send(()).expect("receiver");
            Err("ended".into())
        })
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(5),
        run_code(service.clone(), "tool.blocked().catch(() => {}); await tool.ready(); return 7;", false, None, None, Some(execute)))
        .await.expect("run finishes").expect("result");
    tokio::time::timeout(std::time::Duration::from_secs(5), cancelled_rx).await.expect("leaked call cancelled").expect("signal");
    assert_eq!(result.return_value, Some(json!(7)));
    service.close().await.expect("close");
}

#[tokio::test]
async fn assertion_and_invalid_wait_preserve_their_run_error_reasons() {
    let (service, _) = open_desktop(HashMap::new()).await;
    let assertion = run_code(service.clone(), "assert(false, '');", true, None, None, None).await.expect_err("assertion");
    assert!(matches!(assertion, DesktopRunError::ComputerRun(error) if error.reason == ComputerRunErrorReason::Assertion && error.message.is_empty()));
    let invalid_wait = run_code(service.clone(), "await wait('invalid');", true, None, None, None).await.expect_err("invalid wait");
    assert!(matches!(invalid_wait, DesktopRunError::ComputerRun(error) if error.reason == ComputerRunErrorReason::Wait));
    service.close().await.expect("close");
}

#[tokio::test]
async fn explicit_null_return_is_not_an_undefined_return() {
    let (service, _) = open_desktop(HashMap::new()).await;
    let null = run_code(service.clone(), "return null;", true, None, None, None).await.expect("null result");
    let absent = run_code(service.clone(), "return;", true, None, None, None).await.expect("undefined result");
    assert_eq!(null.return_value, Some(serde_json::Value::Null));
    assert_eq!(absent.return_value, None);
    service.close().await.expect("close");
}

#[tokio::test]
async fn synchronous_user_loop_is_interrupted_by_the_owned_watchdog() {
    let (service, _) = open_desktop(HashMap::new()).await;
    let result = tokio::time::timeout(std::time::Duration::from_secs(5),
        run_code(service.clone(), "for (;;) {}", false, Some(100), None, None))
        .await.expect("watchdog settles synchronous loop");
    assert!(matches!(result, Err(DesktopRunError::ComputerRun(error)) if error.reason == ComputerRunErrorReason::Timeout));
    service.close().await.expect("close");
}

#[tokio::test]
async fn caller_abort_cancels_the_signal_of_an_in_flight_host_tool() {
    let (service, _) = open_desktop(HashMap::new()).await;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started = Arc::new(std::sync::Mutex::new(Some(started_tx)));
    let execute: ExecuteTool = Arc::new(move |_, _, signal| {
        let started = started.clone();
        Box::pin(async move {
            started.lock().expect("start lock").take().expect("one call").send(()).expect("receiver");
            signal.cancelled().await;
            Err("host tool cancelled".into())
        })
    });
    let caller = AbortController::new();
    let signal = caller.signal();
    let running_service = service.clone();
    let running = tokio::spawn(async move {
        run_code(running_service, "await tool.blocked();", false, None, Some(signal), Some(execute)).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), started_rx).await.expect("host call starts").expect("signal");
    caller.abort(None);
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), running).await.expect("run settles").expect("join");
    assert!(matches!(result, Err(DesktopRunError::ComputerRun(error)) if error.reason == ComputerRunErrorReason::Aborted));
    service.close().await.expect("close");
}

#[tokio::test]
async fn malformed_engine_window_result_is_rejected_before_user_code_receives_it() {
    let (service, _) = open_desktop(HashMap::from([("FAKE_ENGINE_MALFORMED_WINDOWS".into(), "1".into())])).await;
    let error = run_code(service.clone(), "return await desktop.windows();", true, None, None, None)
        .await.expect_err("malformed engine result");
    assert!(matches!(error, DesktopRunError::Engine(DesktopError::Service(_))));
    service.close().await.expect("close");
}

#[tokio::test]
async fn window_filters_preserve_snapshot_bounds_and_focused_handle() {
    let (service, _) = open_desktop(HashMap::new()).await;
    let outcome = run_code(service.clone(),
        "const windows = await desktop.windows({title: 'MAIN', app: 'code'}); const handle = await desktop.focusedWindow(); return {ids: windows.map(w => w.id), bounds: handle.bounds};",
        true, None, None, None).await.expect("filtered windows");
    assert_eq!(outcome.return_value, Some(json!({"ids": ["101"], "bounds": {"x": 0, "y": 25, "width": 1200, "height": 800}})));
    service.close().await.expect("close");
}

#[tokio::test]
async fn ambiguous_window_filter_refuses_to_choose_a_window() {
    let (service, _) = open_desktop(HashMap::new()).await;
    let error = run_code(service.clone(), "await desktop.window({});", true, None, None, None)
        .await.expect_err("ambiguous selector");
    assert!(error.to_string().contains("multiple windows match"));
    service.close().await.expect("close");
}

async fn open_desktop(
    env_overrides: HashMap<String, String>,
) -> (Arc<DesktopService>, SpawnLog) {
    let mut env = env_overrides;
    env.insert("FAKE_ENGINE_DESKTOP".into(), "1".into());
    let log = SpawnLog::new(env);
    let tracer = log.notification_tracer(0);
    let service = Arc::new(DesktopService::new(DesktopServiceOptions {
        create_child: Some(log.factory.clone()),
        on_notification: Some(tracer),
    }));
    service.open(Default::default()).await.expect("service open");
    (service, log)
}

fn test_snapshot(read_only: bool) -> ComputerSessionSnapshot {
    ComputerSessionSnapshot {
        cwd: std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        session_id: "session-1".into(),
        capture_max_width: 1280,
        capture_max_height: 800,
        capture_max_bytes: 5_000_000,
        display: "all".into(),
        read_only,
        stop_hotkey: "ctrl+alt+shift+escape".into(),
        allow_host_relay_only_stop: false,
    }
}

async fn run_code(
    service: Arc<DesktopService>,
    code: &str,
    read_only: bool,
    timeout_ms: Option<u64>,
    signal: Option<maho_ai::utils::abort::AbortSignal>,
    execute_tool: Option<ExecuteTool>,
) -> Result<ComputerRunOk, DesktopRunError> {
    run_computer_code(
        ComputerRunRequest {
            code: code.to_string(),
            snapshot: test_snapshot(read_only),
            timeout_ms: timeout_ms.unwrap_or(RUN_TIMEOUT_MS),
            signal,
        },
        ComputerRunHost {
            service,
            execute_tool,
            kernel: None,
        },
    )
    .await
}

#[tokio::test]
async fn test_settles_a_run_that_returns_without_awaiting_any_host_call() {
    let (service, _log) = open_desktop(HashMap::new()).await;

    let res1 = run_code(service.clone(), "return 42;", false, Some(5000), None, None).await.unwrap();
    assert_eq!(res1.return_value, Some(json!(42)));

    let res2 = run_code(
        service.clone(),
        "if (true) return { early: true }; await desktop.windows();",
        false,
        Some(5000),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(res2.return_value, Some(json!({ "early": true })));

    let res3 = run_code(
        service.clone(),
        "await Promise.resolve(); return 'x';",
        false,
        Some(5000),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(res3.return_value, Some(json!("x")));

    service.close().await.unwrap();
}

#[tokio::test]
async fn test_returns_the_codes_value_and_displays_inline_capture_before_later_console_output() {
    let (service, _log) = open_desktop(HashMap::new()).await;
    let code = r#"
        const shot = await desktop.screenshot();
        const editor = await desktop.window({ app: "code" });
        console.log("found", editor.title);
        return { width: shot.width, frameId: shot.frameId, window: editor.id };
    "#;

    let result = run_code(service.clone(), code, false, None, None, None).await.unwrap();
    assert_eq!(
        result.return_value,
        Some(json!({ "width": 1280, "frameId": "desktop-1", "window": "101" }))
    );

    assert_eq!(result.displays.len(), 3);
    match &result.displays[1] {
        ComputerDisplay::Image { mime_type, .. } => assert_eq!(mime_type, "image/png"),
        _ => panic!("expected image display"),
    }
    match &result.displays[2] {
        ComputerDisplay::Text { text } => assert!(text.contains("found main.ts")),
        _ => panic!("expected text display"),
    }

    service.close().await.unwrap();
}

#[tokio::test]
async fn test_shows_an_artifact_only_capture_as_text_with_its_path_and_records_the_artifact() {
    let (service, _log) = open_desktop(HashMap::new()).await;
    let code = r#"return (await (await desktop.window("202")).screenshot()).path;"#;

    let result = run_code(service.clone(), code, false, None, None, None).await.unwrap();
    let expected_path = "/tmp/fake-captures/202-1.png";
    assert_eq!(result.return_value, Some(json!(expected_path)));

    assert!(result.displays.iter().all(|d| matches!(d, ComputerDisplay::Text { .. })));
    assert!(result.displays.iter().any(|d| match d {
        ComputerDisplay::Text { text } => text.contains(expected_path),
        _ => false,
    }));

    assert_eq!(result.screenshots.len(), 1);
    assert_eq!(result.screenshots[0].path, expected_path);
    assert_eq!(result.screenshots[0].target, "202");

    service.close().await.unwrap();
}

#[tokio::test]
async fn test_rejects_click_in_a_read_only_run_before_it_reaches_the_engine() {
    let (service, log) = open_desktop(HashMap::new()).await;

    let err = run_code(service.clone(), "await desktop.click(1, 1)", true, None, None, None)
        .await
        .unwrap_err();

    match err {
        DesktopRunError::ComputerRun(cr) => {
            assert_eq!(cr.reason, ComputerRunErrorReason::ReadOnly);
            assert!(cr.message.contains("read-only run: 'click' requires read_only: false"));
        }
        other => panic!("expected ReadOnly ComputerRunError, got: {other:?}"),
    }

    let reqs = log.requests.lock().await;
    assert!(!reqs.iter().any(|r| r.method == "click"));

    service.close().await.unwrap();
}

#[tokio::test]
async fn test_allows_screenshot_in_a_read_only_run() {
    let (service, log) = open_desktop(HashMap::new()).await;

    let result = run_code(
        service.clone(),
        "return (await desktop.screenshot({ silent: true })).frameId;",
        true,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.return_value, Some(json!("desktop-1")));
    let capture = log.nth_request("capture", 1).await;
    assert_eq!(capture.params.expect("capture params")["caps"], json!({"maxWidth": 1280, "maxHeight": 800, "maxBytes": 5_000_000}));

    let reqs = log.requests.lock().await;
    assert!(reqs.iter().any(|r| r.method == "capture"));

    service.close().await.unwrap();
}

#[tokio::test]
async fn test_surfaces_the_engines_invalid_coordinate_frame_when_input_precedes_first_capture() {
    let (service, _log) = open_desktop(HashMap::new()).await;

    let err = run_code(service.clone(), "await desktop.click(5, 5)", false, None, None, None)
        .await
        .unwrap_err();

    match err {
        DesktopRunError::Engine(DesktopError::Rpc(rpc)) => {
            assert_eq!(rpc.rpc_code, -32007);
            assert!(matches!(rpc.data, Some(senpi_desktop_protocol::JsonRpcErrorData::Engine(ref data)) if data.code == senpi_desktop_core::error::ErrorCode::InvalidCoordinateFrame));
        }
        other => panic!("expected InvalidCoordinateFrame Rpc error, got: {other:?}"),
    }

    service.close().await.unwrap();
}

#[tokio::test]
async fn test_drives_a_window_handle_by_its_id_and_reports_the_engines_audit_events_for_the_run() {
    let (service, log) = open_desktop(HashMap::new()).await;
    let code = r#"
        const editor = await desktop.window({ app: "Code" });
        await editor.screenshot({ silent: true });
        await editor.click(3, 4, { delivery: "foreground" });
    "#;

    let result = run_code(service.clone(), code, false, None, None, None).await.unwrap();

    let reqs = log.requests.lock().await;
    let click = reqs.iter().find(|r| r.method == "click").unwrap();
    assert_eq!(click.params.as_ref().unwrap()["target"], "101");
    assert_eq!(click.params.as_ref().unwrap()["x"], 3);
    assert_eq!(click.params.as_ref().unwrap()["y"], 4);

    assert!(result.audit.iter().any(|a| a.action == "click" && a.target == "101" && a.status == senpi_desktop_protocol::AuditStatus::Success));

    service.close().await.unwrap();
}

#[tokio::test]
async fn caught_engine_error_does_not_replace_later_user_error() {
    let (service, _log) = open_desktop(HashMap::new()).await;
    let result = run_code(service.clone(), "try { await desktop.click(5, 5); } catch {} throw new Error('user failure');", false, None, None, None).await;
    service.close().await.unwrap();
    match result.unwrap_err() {
        DesktopRunError::ComputerRun(error) => assert_eq!(error.message,"user failure"),
        other => panic!("caught engine failure replaced user error: {other:?}"),
    }
}

#[tokio::test]
async fn read_only_run_refuses_clipboard_write_before_engine_dispatch() {
    let (service,log)=open_desktop(HashMap::new()).await;
    let result=run_code(service.clone(),"await desktop.clipboard.write('private');",true,None,None,None).await;
    service.close().await.unwrap();
    match result.unwrap_err() {
        DesktopRunError::ComputerRun(error)=>assert_eq!(error.reason,ComputerRunErrorReason::ReadOnly),
        other=>panic!("expected read-only refusal, got {other:?}"),
    }
    assert!(!log.requests.lock().await.iter().any(|request|request.method=="clipboard.write"));
}

#[tokio::test]
async fn test_reaches_the_host_execute_tool_from_tool_name_inside_the_run() {
    let (service, _log) = open_desktop(HashMap::new()).await;
    let execute_tool: ExecuteTool = Arc::new(|name, params, _sig| {
        Box::pin(async move {
            assert_eq!(name, "read");
            assert_eq!(params["path"], "notes.txt");
            Ok(json!({ "content": [{ "type": "text", "text": "file body" }] }))
        })
    });

    let code = "return (await tool.read({ path: 'notes.txt' })).content[0].text;";
    let result = run_code(service.clone(), code, false, None, None, Some(execute_tool)).await.unwrap();

    assert_eq!(result.return_value, Some(json!("file body")));

    service.close().await.unwrap();
}

#[tokio::test]
async fn test_rejects_an_aborted_run_and_sends_cancel_for_its_in_flight_engine_call() {
    let mut env = HashMap::new();
    env.insert("FAKE_ENGINE_STALL".into(), "windows".into());
    let (service, log) = open_desktop(env).await;

    let controller = AbortController::new();
    let sig = controller.signal();

    let svc = service.clone();
    let handle = tokio::spawn(async move {
        run_code(svc, "await desktop.windows()", false, None, Some(sig), None).await
    });

    let sent = log.nth_request("windows", 1).await;
    controller.abort(None);

    let err = handle.await.unwrap().unwrap_err();
    match err {
        DesktopRunError::ComputerRun(cr) => assert_eq!(cr.reason, ComputerRunErrorReason::Aborted),
        other => panic!("expected Aborted ComputerRunError, got: {other:?}"),
    }

    let cancel = log.nth_request("$/cancel", 1).await;
    assert_eq!(cancel.params.as_ref().unwrap()["id"], sent.id.unwrap());

    service.close().await.unwrap();
}

#[tokio::test]
async fn test_times_out_a_run_that_loops_forever_at_timeout_ms_and_cancels_in_flight_call() {
    let mut env = HashMap::new();
    env.insert("FAKE_ENGINE_STALL".into(), "windows".into());
    let (service, log) = open_desktop(env).await;

    let svc = service.clone();
    let handle = tokio::spawn(async move {
        run_code(
            svc,
            "for (;;) { await desktop.displays(); await desktop.windows(); }",
            false,
            Some(200),
            None,
            None,
        )
        .await
    });

    let sent = log.nth_request("windows", 1).await;
    let err = handle.await.unwrap().unwrap_err();
    match err {
        DesktopRunError::ComputerRun(cr) => assert_eq!(cr.reason, ComputerRunErrorReason::Timeout),
        other => panic!("expected Timeout ComputerRunError, got: {other:?}"),
    }

    let cancel = log.nth_request("$/cancel", 1).await;
    assert_eq!(cancel.params.as_ref().unwrap()["id"], sent.id.unwrap());

    service.close().await.unwrap();
}
