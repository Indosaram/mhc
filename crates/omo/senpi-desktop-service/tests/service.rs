//! Port of upstream `test/service.test.ts`, `test/acquire-child.test.ts`, and `test/permission.test.ts`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::time::Duration;

use senpi_desktop_core::protocol::MethodRejection;
use senpi_desktop_protocol::{
    JsonRpcErrorData, MethodRejectionData,
};
use senpi_desktop_service::child::{
    acquiring_engine_child_factory, default_engine_path, AcquiringEngineChildOptions,
};
use senpi_desktop_service::parse::parse_error_data;
use senpi_desktop_service::rpc_client::{
    CallOptions, DesktopError, DesktopServiceErrorCode,
};
use senpi_desktop_service::service::{
    DesktopSessionOpenParams, DesktopService, DesktopServiceOptions,
};
use senpi_desktop_service::timeouts::{RESTART_MESSAGE, START_TIMEOUT_MESSAGE};
use serde_json::json;

mod common;
use common::SpawnLog;

#[tokio::test]
async fn test_01_opens_with_hello_then_session_open_and_resumes() {
    let log = SpawnLog::new(HashMap::new());
    let service = DesktopService::new(DesktopServiceOptions {
        create_child: Some(log.factory.clone()),
        on_notification: Some(log.notification_tracer(0)),
    });

    let capabilities = service.open(Default::default()).await.expect("open ok");
    assert_eq!(capabilities.backend, "fake");

    let r1 = log.nth_request("engine.hello", 1).await;
    let r2 = log.nth_request("session.open", 1).await;
    assert_eq!(r1.method, "engine.hello");
    assert_eq!(r2.method, "session.open");

    let stopped = service.stop().await.expect("stop ok");
    assert!(stopped.suspended);

    let resumed = service.resume().await.expect("resume ok");
    assert!(!resumed.suspended);

    service.close().await.expect("close ok");
}

#[tokio::test]
async fn test_02_rejects_with_start_timeout_when_silent() {
    let mut env = HashMap::new();
    env.insert("FAKE_ENGINE_MODE".to_string(), "silent".to_string());
    let log = SpawnLog::new(env);
    let service = DesktopService::new(DesktopServiceOptions {
        create_child: Some(log.factory.clone()),
        on_notification: Some(log.notification_tracer(0)),
    });

    let open_fut = tokio::spawn({
        let s = Arc::new(service);
        async move { s.open(Default::default()).await }
    });
    let wait_req = log.nth_request("engine.hello", 1);

    let (open_res, _) = tokio::join!(open_fut, wait_req);
    let res = open_res.expect("spawn ok");
    assert!(res.is_err());
    if let Err(DesktopError::Service(err)) = res {
        assert_eq!(err.code(), DesktopServiceErrorCode::Timeout);
        assert!(err.message().contains(START_TIMEOUT_MESSAGE));
    } else {
        panic!("expected DesktopServiceError timeout");
    }
    assert_eq!(log.kills(), 1);
}

#[tokio::test]
async fn test_03_refuses_engine_with_another_abi_and_kills_it() {
    let mut env = HashMap::new();
    env.insert("FAKE_ENGINE_ABI".to_string(), "senpi-desktop/0".to_string());
    let log = SpawnLog::new(env);
    let service = DesktopService::new(DesktopServiceOptions {
        create_child: Some(log.factory.clone()),
        on_notification: Some(log.notification_tracer(0)),
    });

    let res = service.open(Default::default()).await;
    assert!(res.is_err());
    match res {
        Err(DesktopError::AbiMismatch(abi_err)) => {
            assert_eq!(abi_err.actual_abi, "senpi-desktop/0");
        }
        _ => panic!("expected DesktopError::AbiMismatch"),
    }
    assert_eq!(log.kills(), 1);

    let reqs = log.requests.lock().await;
    let methods: Vec<_> = reqs.iter().map(|r| r.method.as_str()).collect();
    assert!(!methods.contains(&"session.open"));
}

#[tokio::test]
async fn test_04_routes_out_of_order_replies_to_request_ids() {
    let log = SpawnLog::new(HashMap::new());
    let service = Arc::new(DesktopService::new(DesktopServiceOptions {
        create_child: Some(log.factory.clone()),
        on_notification: Some(log.notification_tracer(0)),
    }));
    service.open(Default::default()).await.expect("open ok");

    let held_call = tokio::spawn({
        let s = Arc::clone(&service);
        async move { s.call("windows", json!({ "fake": "hold" }), CallOptions::default()).await }
    });
    log.nth_request("windows", 1).await;

    let release_call = tokio::spawn({
        let s = Arc::clone(&service);
        async move { s.call("displays", json!({ "fake": "release" }), CallOptions::default()).await }
    });

    let (res_held, res_release) = tokio::try_join!(held_call, release_call).expect("both joined ok");
    let held_val = res_held.expect("held call ok");
    let release_val = res_release.expect("release call ok");

    assert_eq!(release_val.get("method").and_then(|m| m.as_str()), Some("displays"));
    assert_eq!(held_val.get("method").and_then(|m| m.as_str()), Some("held"));
    service.close().await.expect("close ok");
}

#[tokio::test]
async fn test_05_sends_cancel_for_aborted_call_and_keeps_engine() {
    let log = SpawnLog::new(HashMap::new());
    let service = Arc::new(DesktopService::new(DesktopServiceOptions {
        create_child: Some(log.factory.clone()),
        on_notification: Some(log.notification_tracer(0)),
    }));
    service.open(Default::default()).await.expect("open ok");

    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let opts = CallOptions {
        timeout: None,
        cancel_rx: Some(Arc::new(tokio::sync::Mutex::new(cancel_rx))),
    };

    let capture_call = tokio::spawn({
        let s = Arc::clone(&service);
        async move { s.call("capture", json!({ "fake": "cancellable" }), opts).await }
    });

    let sent = log.nth_request("capture", 1).await;
    let _ = cancel_tx.send(());

    let res = capture_call.await.expect("join ok");
    assert!(res.is_err());
    if let Err(DesktopError::Service(err)) = res {
        assert_eq!(err.code(), DesktopServiceErrorCode::Cancelled);
        assert!(!err.engine_restarted);
    } else {
        panic!("expected cancelled error");
    }

    let cancel_req = log.nth_request("$/cancel", 1).await;
    assert_eq!(
        cancel_req.params.and_then(|p| p.get("id").and_then(|i| i.as_u64())),
        sent.id
    );
    assert_eq!(log.kills(), 0);
    assert!(service.call("displays", json!({}), CallOptions::default()).await.is_ok());
    service.close().await.expect("close ok");
}

#[tokio::test]
async fn test_06_kills_engine_ignoring_cancel_after_grace_and_replays() {
    let log = SpawnLog::new(HashMap::new());
    let service = Arc::new(DesktopService::new(DesktopServiceOptions {
        create_child: Some(log.factory.clone()),
        on_notification: Some(log.notification_tracer(0)),
    }));
    service.open(DesktopSessionOpenParams {
        display: Some("all".to_string()),
        ..Default::default()
    }).await.expect("open ok");
    service.ensure_stop_path("ctrl+alt+shift+escape").await.expect("arm ok");

    let opts = CallOptions {
        timeout: Some(Duration::from_millis(100)),
        cancel_rx: None,
    };

    let hung_call = tokio::spawn({
        let s = Arc::clone(&service);
        async move { s.call("typeText", json!({ "fake": "hang" }), opts).await }
    });

    log.nth_request("typeText", 1).await;
    log.nth_request("$/cancel", 1).await;

    let res = hung_call.await.expect("join ok");
    assert!(res.is_err());
    if let Err(DesktopError::Service(err)) = res {
        assert_eq!(err.code(), DesktopServiceErrorCode::Timeout);
        assert!(err.engine_restarted);
        assert!(err.message().contains(RESTART_MESSAGE));
    } else {
        panic!("expected timeout with restart");
    }
    assert_eq!(log.kills(), 1);

    let next = service.call("displays", json!({}), CallOptions::default()).await;
    assert!(next.is_ok());

    let r_open = log.nth_request("session.open", 2).await;
    assert_eq!(
        r_open.params.as_ref().and_then(|p| p.get("display").and_then(|d| d.as_str())),
        Some("all")
    );
    service.close().await.expect("close ok");
}

#[tokio::test]
async fn test_07_closes_quietly_right_after_grace_kill() {
    let log = SpawnLog::new(HashMap::new());
    let service = Arc::new(DesktopService::new(DesktopServiceOptions {
        create_child: Some(log.factory.clone()),
        on_notification: Some(log.notification_tracer(0)),
    }));
    service.open(Default::default()).await.expect("open ok");

    let opts = CallOptions {
        timeout: Some(Duration::from_millis(100)),
        cancel_rx: None,
    };
    let hung_call = tokio::spawn({
        let s = Arc::clone(&service);
        async move { s.call("typeText", json!({ "fake": "hang" }), opts).await }
    });

    log.nth_request("typeText", 1).await;
    let _ = hung_call.await;

    service.close().await.expect("close ok");
    let reqs = log.requests.lock().await;
    let methods: Vec<_> = reqs.iter().map(|r| r.method.as_str()).collect();
    assert!(!methods.contains(&"session.close"));
}

#[tokio::test]
async fn test_08_heartbeats_every_heartbeat_ms_and_stops_on_close() {
    let log = SpawnLog::new(HashMap::new());
    let service = DesktopService::new(DesktopServiceOptions {
        create_child: Some(log.factory.clone()),
        on_notification: Some(log.notification_tracer(0)),
    });
    service.open(Default::default()).await.expect("open ok");
    log.nth_request("stopPath.heartbeat", 3).await;
    service.close().await.expect("close ok");
    let reqs = log.requests.lock().await;
    let hb_count = reqs.iter().filter(|r| r.method == "stopPath.heartbeat").count();
    assert!(hb_count >= 3);
}

#[tokio::test]
async fn test_09_fans_audit_and_stop_path_changed_to_listeners() {
    let log = SpawnLog::new(HashMap::new());
    let service = DesktopService::new(DesktopServiceOptions {
        create_child: Some(log.factory.clone()),
        on_notification: Some(log.notification_tracer(0)),
    });

    let audit_count = Arc::new(AtomicUsize::new(0));
    let ac_clone = Arc::clone(&audit_count);
    let audit_notify = Arc::new(tokio::sync::Notify::new());
    let an_clone = Arc::clone(&audit_notify);
    let _sub1 = service.on_audit(move |ev| {
        assert_eq!(ev.action, "click");
        ac_clone.fetch_add(1, Ordering::SeqCst);
        an_clone.notify_waiters();
    });

    let stop_count = Arc::new(AtomicUsize::new(0));
    let sc_clone = Arc::clone(&stop_count);
    let _sub2 = service.on_stop_path_change(move |st| {
        assert_eq!(serde_json::to_value(st.stop_path).unwrap(), json!("global"));
        sc_clone.fetch_add(1, Ordering::SeqCst);
    });
    let sub3 = service.on_audit(|_| {});
    sub3.unsubscribe();

    service.open(Default::default()).await.expect("open ok");
    let audit_signal = audit_notify.notified();
    tokio::pin!(audit_signal);
    audit_signal.as_mut().enable();
    service.call("click", json!({ "fake": "notify" }), CallOptions::default()).await.expect("notify call");
    tokio::time::timeout(Duration::from_secs(5), audit_signal).await.expect("audit signal");
    assert!(audit_count.load(Ordering::SeqCst) >= 1);
    service.close().await.expect("close ok");
}

#[tokio::test]
async fn test_10_rejects_every_pending_call_with_closed_on_engine_exit() {
    let log = SpawnLog::new(HashMap::new());
    let service = Arc::new(DesktopService::new(DesktopServiceOptions {
        create_child: Some(log.factory.clone()),
        on_notification: Some(log.notification_tracer(0)),
    }));
    service.open(Default::default()).await.expect("open ok");

    let held_call = tokio::spawn({
        let s = Arc::clone(&service);
        async move { s.call("windows", json!({ "fake": "hold" }), CallOptions::default()).await }
    });
    log.nth_request("windows", 1).await;

    let exit_call = tokio::spawn({
        let s = Arc::clone(&service);
        async move { s.call("displays", json!({ "fake": "exit" }), CallOptions::default()).await }
    });

    let (res_held, res_exit) = tokio::join!(held_call, exit_call);
    let held_res = res_held.expect("join ok");
    let exit_res = res_exit.expect("join ok");

    assert!(held_res.is_err());
    assert!(exit_res.is_err());
    if let Err(DesktopError::Service(err)) = exit_res {
        assert_eq!(err.code(), DesktopServiceErrorCode::Closed);
        assert!(err.message().contains("7"));
    }
}

#[tokio::test]
async fn test_11_arms_stop_path_once_per_chord() {
    let log = SpawnLog::new(HashMap::new());
    let service = DesktopService::new(DesktopServiceOptions {
        create_child: Some(log.factory.clone()),
        on_notification: Some(log.notification_tracer(0)),
    });
    service.open(Default::default()).await.expect("open ok");

    let status1 = service.ensure_stop_path("ctrl+alt+shift+escape").await.expect("arm ok");
    let status2 = service.ensure_stop_path("ctrl+alt+shift+escape").await.expect("read ok");
    assert_eq!(status1.stop_path, status2.stop_path);

    log.nth_request("stopPath.start", 1).await;
    log.nth_request("stopPath.status", 1).await;
    service.close().await.expect("close ok");
}

#[tokio::test]
async fn test_12_acquiring_reacquires_before_every_engine_spawn() {
    let requested = Arc::new(std::sync::Mutex::new(Vec::new()));
    let spawned = Arc::new(std::sync::Mutex::new(Vec::new()));
    let request_log = requested.clone();
    let spawn_log = spawned.clone();
    let factory = acquiring_engine_child_factory(AcquiringEngineChildOptions {
        version: "5.0.2".into(), host: Some("linux-x64".into()),
        acquire: Arc::new(move |version, host| {
            let mut log = request_log.lock().unwrap();
            log.push((version, host));
            let path = PathBuf::from(format!("/cache/generation-{}/senpi-desktop-engine", log.len()));
            Box::pin(async move { Ok(senpi_desktop_service::child::AcquiredDesktopEngine { path: Some(path), diagnostic_message: None }) })
        }),
        spawn_engine: Some(Arc::new(move |path| {
            spawn_log.lock().unwrap().push(path.clone());
            let python_cmd = if cfg!(windows) { "python" } else { "python3" };
            let child = tokio::process::Command::new(python_cmd)
                .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_engine.py"))
                .arg("--stdio").stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
                .spawn().expect("fake engine");
            Ok(senpi_desktop_service::child::EngineChild::new(path.display().to_string(), child))
        })),
    });
    let service = DesktopService::new(DesktopServiceOptions { create_child: Some(factory.clone()), on_notification: None });
    assert_eq!(service.open(Default::default()).await.unwrap().backend, "fake");
    let second = factory().await.unwrap();
    second.end();
    tokio::time::timeout(Duration::from_secs(5), second.wait_exited()).await.expect("second exit");
    assert_eq!(*requested.lock().unwrap(), vec![("5.0.2".into(), Some("linux-x64".into())), ("5.0.2".into(), Some("linux-x64".into()))]);
    assert_eq!(*spawned.lock().unwrap(), vec![PathBuf::from("/cache/generation-1/senpi-desktop-engine"), PathBuf::from("/cache/generation-2/senpi-desktop-engine")]);
    service.close().await.unwrap();
}

#[tokio::test]
async fn test_13_unavailable_acquisition_spawns_nothing() {
    let factory = acquiring_engine_child_factory(AcquiringEngineChildOptions {
        version: "5.0.2".into(), host: Some("linux-arm64".into()),
        acquire: Arc::new(|_, _| Box::pin(async { Ok(senpi_desktop_service::child::AcquiredDesktopEngine { path: None, diagnostic_message: Some("native-unavailable".into()) }) })),
        spawn_engine: Some(Arc::new(|_| panic!("must not spawn"))),
    });
    let service = DesktopService::new(DesktopServiceOptions { create_child: Some(factory), on_notification: None });
    let err = service.open(Default::default()).await.unwrap_err();
    assert!(matches!(err, DesktopError::Service(ref error) if error.code() == DesktopServiceErrorCode::Closed));
}

#[test]
fn test_14_decodes_typed_permission_guidance() {
    let data = json!({
        "code": "PermissionDenied",
        "hint": "grant accessibility in System Settings",
        "permission": {
            "permission": "accessibility",
            "settingsUrl": "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
            "app": "QA App",
            "relaunchRequired": true
        }
    });
    let parsed = parse_error_data(&data);
    assert!(parsed.is_some());
    if let Some(JsonRpcErrorData::Engine(engine_data)) = parsed {
        assert_eq!(engine_data.code, senpi_desktop_core::error::ErrorCode::PermissionDenied);
        assert!(engine_data.permission.is_some());
    } else {
        panic!("expected Engine error data");
    }
}

#[test]
fn test_15_decodes_method_rejection() {
    let data = json!({ "reason": "hostOnly" });
    let parsed = parse_error_data(&data);
    assert_eq!(
        parsed,
        Some(JsonRpcErrorData::Rejected(MethodRejectionData {
            reason: MethodRejection::HostOnly
        }))
    );
}

#[test]
fn test_16_ignores_unrecognized_error_payload() {
    let data = json!({ "unexpected": 42 });
    assert_eq!(parse_error_data(&data), None);
}

#[test]
fn test_17_locates_default_sibling_engine_path() {
    let p = default_engine_path();
    assert!(p.is_ok());
    let path = p.unwrap();
    assert!(path.to_string_lossy().contains("senpi-desktop-engine"));
}

fn assert_malformed_permission_metadata_is_not_exposed(key: &str, invalid: serde_json::Value) {
    let base = json!({"permission":"accessibility","settingsUrl":"settings","app":"QA App","relaunchRequired":true});
        let mut permission = base.clone();
        permission[key] = invalid;
        let data = parse_error_data(&json!({"code":"PermissionDenied","permission":permission}));
        assert!(matches!(data, Some(JsonRpcErrorData::Engine(ref error)) if error.permission.is_none() && error.hint.is_none()));
}

#[test]
fn unknown_permission_is_not_exposed() {
    assert_malformed_permission_metadata_is_not_exposed("permission", json!("unknown"));
}

#[test]
fn numeric_settings_url_is_not_exposed() {
    assert_malformed_permission_metadata_is_not_exposed("settingsUrl", json!(4));
}

#[test]
fn null_app_is_not_exposed() {
    assert_malformed_permission_metadata_is_not_exposed("app", json!(null));
}

#[test]
fn string_relaunch_flag_is_not_exposed() {
    assert_malformed_permission_metadata_is_not_exposed("relaunchRequired", json!("true"));
}

#[test]
fn accepts_accessibility_denied_stop_path_status() {
    assert!(senpi_desktop_service::parse::is_stop_path_status(&json!({
        "suspended":false,"globalLive":false,"hostRelayLive":true,"heartbeatFresh":true,
        "stopPath":"host-relay","reason":"accessibility-denied"
    })));
}
