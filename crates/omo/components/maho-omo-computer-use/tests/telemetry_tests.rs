use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use maho_omo_computer_use::telemetry::{
    create_computer_use_telemetry,
    ComputerUseTelemetryObservation,
    ComputerUseTelemetryObservers,
    SharedComputerUseTelemetryObservers,
};
use senpi_desktop_protocol::DesktopCapabilities;
use serde_json::json;

#[test]
fn test_given_raw_runtime_values_when_activation_recorded_then_closed_platform_and_backend() {
    let observers = Arc::new(SharedComputerUseTelemetryObservers::new());
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let obs_clone = Arc::clone(&observed);

    let _sub = observers.subscribe(Arc::new(move |obs| {
        obs_clone.lock().unwrap().push(obs);
    }));

    let telem = create_computer_use_telemetry("unknown-os", Some(observers));
    telem.activation("sess-raw", true, "command_on", &json!("unknown-backend"));

    let list = observed.lock().unwrap();
    assert_eq!(list.len(), 1);
    if let ComputerUseTelemetryObservation::Activation { runtime, .. } = &list[0] {
        assert_eq!(runtime.platform, "other");
        assert_eq!(runtime.backend, "other");
    } else {
        panic!("expected activation");
    }
}

#[test]
fn test_given_unavailable_os_capabilities_when_inspected_then_capture_input_ax_emit() {
    let observers = Arc::new(SharedComputerUseTelemetryObservers::new());
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let obs_clone = Arc::clone(&observed);

    let _sub = observers.subscribe(Arc::new(move |obs| {
        obs_clone.lock().unwrap().push(obs);
    }));

    let telem = create_computer_use_telemetry("darwin", Some(observers));
    let caps = DesktopCapabilities {
        backend: "macos".to_string(),
        display_server: None,
        capture: false,
        input: false,
        ax: false,
        background_window_input: false,
        delivery_modes: vec![],
        capture_permission: "denied".to_string(),
        input_permission: "denied".to_string(),
        ax_permission: "denied".to_string(),
        display_count: 1,
        focus_guard: false,
        stop_path: "none".to_string(),
        stop_reason: None,
        integrity_level: None,
        screen_locked: false,
    };

    telem.os_permissions("sess-os-perm", &caps);

    let list = observed.lock().unwrap();
    assert_eq!(list.len(), 3);
}

#[test]
fn test_given_every_engine_error_code_when_failures_recorded_then_codes_stay_enumerated() {
    let observers = Arc::new(SharedComputerUseTelemetryObservers::new());
    let count = Arc::new(AtomicUsize::new(0));
    let count_clone = Arc::clone(&count);

    let _sub = observers.subscribe(Arc::new(move |obs| {
        if let ComputerUseTelemetryObservation::EngineError { .. } = obs {
            count_clone.fetch_add(1, Ordering::SeqCst);
        }
    }));

    let telem = create_computer_use_telemetry("linux", Some(observers));
    for code in maho_omo_computer_use::telemetry::COMPUTER_USE_ENGINE_ERROR_CODES {
        telem.engine_error("sess-errors", code, &json!("x11"));
    }

    assert_eq!(
        count.load(Ordering::SeqCst),
        maho_omo_computer_use::telemetry::COMPUTER_USE_ENGINE_ERROR_CODES.len()
    );
}

#[test]
fn test_given_denied_exec_result_when_host_reports_then_only_tier_emitted() {
    let observers = Arc::new(SharedComputerUseTelemetryObservers::new());
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let obs_clone = Arc::clone(&observed);

    let _sub = observers.subscribe(Arc::new(move |obs| {
        obs_clone.lock().unwrap().push(obs);
    }));

    let telem = create_computer_use_telemetry("darwin", Some(observers));
    let start_payload = json!({
        "type": "tool_execution_start",
        "toolCallId": "call-exec-1",
        "toolName": "computer",
        "args": { "action": "run", "code": "echo hello" }
    });
    assert!(telem.tool_execution_started(&start_payload));

    let end_payload = json!({
        "type": "tool_execution_end",
        "toolCallId": "call-exec-1",
        "toolName": "computer",
        "isError": true,
        "result": {
            "content": [
                { "type": "text", "text": "The user rejected permission to use this specific tool call." }
            ]
        }
    });

    telem.permission_tier_denied(&end_payload, "sess-exec", &json!("macos"));

    let list = observed.lock().unwrap();
    assert_eq!(list.len(), 1);
    if let ComputerUseTelemetryObservation::PermissionDenied { scope, permission, .. } = &list[0] {
        assert_eq!(scope, "tier");
        assert_eq!(permission, "exec");
    } else {
        panic!("expected tier denial");
    }
}

#[test]
fn test_given_engine_spawn_eacces_when_failed_result_mentions_permission_not_mislabeled() {
    let observers = Arc::new(SharedComputerUseTelemetryObservers::new());
    let count = Arc::new(AtomicUsize::new(0));
    let count_clone = Arc::clone(&count);

    let _sub = observers.subscribe(Arc::new(move |_| {
        count_clone.fetch_add(1, Ordering::SeqCst);
    }));

    let telem = create_computer_use_telemetry("linux", Some(observers));

    let end_payload = json!({
        "type": "tool_execution_end",
        "toolCallId": "call-spawn-fail",
        "toolName": "computer",
        "isError": true,
        "result": {
            "content": [
                { "type": "text", "text": "spawn EACCES /bin/senpi-desktop-engine: permission denied" }
            ]
        }
    });

    telem.permission_tier_denied(&end_payload, "sess-spawn", &json!("x11"));

    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn test_given_mixed_preflights_when_earlier_read_denied_then_call_id_preserves_tier() {
    let observers = Arc::new(SharedComputerUseTelemetryObservers::new());
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let obs_clone = Arc::clone(&observed);

    let _sub = observers.subscribe(Arc::new(move |obs| {
        obs_clone.lock().unwrap().push(obs);
    }));

    let telem = create_computer_use_telemetry("darwin", Some(observers));

    telem.tool_execution_started(&json!({
        "type": "tool_execution_start",
        "toolCallId": "call-preflight-read",
        "toolName": "computer",
        "args": { "action": "capabilities" }
    }));

    telem.tool_execution_started(&json!({
        "type": "tool_execution_start",
        "toolCallId": "call-preflight-exec",
        "toolName": "computer",
        "args": { "action": "run", "code": "ls" }
    }));

    telem.permission_tier_denied(&json!({
        "type": "tool_execution_end",
        "toolCallId": "call-preflight-read",
        "toolName": "computer",
        "isError": true,
        "result": {
            "content": [{ "type": "text", "text": "The user rejected permission to use this specific tool call." }]
        }
    }), "sess-order", &json!("macos"));

    let list = observed.lock().unwrap();
    assert_eq!(list.len(), 1);
    if let ComputerUseTelemetryObservation::PermissionDenied { permission, .. } = &list[0] {
        assert_eq!(permission, "read");
    } else {
        panic!("expected read tier denial");
    }
}

#[test]
fn test_given_duplicate_denied_notifications_when_both_settle_then_another_call_not_consumed() {
    let observers = Arc::new(SharedComputerUseTelemetryObservers::new());
    let count = Arc::new(AtomicUsize::new(0));
    let count_clone = Arc::clone(&count);

    let _sub = observers.subscribe(Arc::new(move |_| {
        count_clone.fetch_add(1, Ordering::SeqCst);
    }));

    let telem = create_computer_use_telemetry("darwin", Some(observers));

    telem.tool_execution_started(&json!({
        "type": "tool_execution_start",
        "toolCallId": "call-dup-1",
        "toolName": "computer",
        "args": { "action": "screenshot" }
    }));

    let denial = json!({
        "type": "tool_execution_end",
        "toolCallId": "call-dup-1",
        "toolName": "computer",
        "isError": true,
        "result": {
            "content": [{ "type": "text", "text": "The user rejected permission to use this specific tool call." }]
        }
    });

    telem.permission_tier_denied(&denial, "sess-dup", &json!("macos"));
    telem.permission_tier_denied(&denial, "sess-dup", &json!("macos"));

    assert_eq!(count.load(Ordering::SeqCst), 1);
}
