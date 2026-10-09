use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use maho_omo_computer_use::engine_probe::{
    probe_computer_use_engine, EngineProbeResult, COMPUTER_USE_DOCTOR_TIMEOUT_MS,
};
use maho_omo_computer_use::engine_status::{
    engine_error_code, EngineState, TrackedDesktopService, TrackedDesktopServiceOptions,
};
use maho_omo_computer_use::permission_status::describe_engine_permissions;
use senpi_desktop_core::protocol::{ABI as ENGINE_ABI, PROTOCOL_VERSION};
use senpi_desktop_protocol::{
    EngineErrorData, ErrorCode, JsonRpcErrorData,
};
use senpi_desktop_service::rpc_client::{DesktopEngineRpcError, DesktopError};
use senpi_desktop_service::service::DesktopServiceOptions;
use tempfile::tempdir;

#[test]
fn test_engine_state_display() {
    assert_eq!(EngineState::NotStarted.to_string(), "not started");
    assert_eq!(EngineState::Ready.to_string(), "ready");
    assert_eq!(
        EngineState::Diagnostic("warn".to_string()).to_string(),
        "warn"
    );
}

#[tokio::test]
async fn test_tracked_desktop_service_error_observation() {
    let observed = Arc::new(AtomicBool::new(false));
    let obs_clone = Arc::clone(&observed);

    let svc = TrackedDesktopService::new(TrackedDesktopServiceOptions {
        service_options: DesktopServiceOptions::default(),
        on_error: Some(Arc::new(move |_code| {
            obs_clone.store(true, Ordering::SeqCst);
        })),
        on_permission_required: None,
    });

    let res = svc.ensure_stop_path("ctrl+c").await;
    assert!(res.is_err());
    assert!(observed.load(Ordering::SeqCst));
}

#[test]
fn test_engine_error_code_mapping() {
    let rpc_err = DesktopError::Rpc(Box::new(DesktopEngineRpcError {
        method: "test".to_string(),
        rpc_code: -32000,
        message: "engine fail".to_string(),
        data: Some(JsonRpcErrorData::Engine(EngineErrorData {
            code: ErrorCode::PermissionDenied,
            hint: None,
            permission: None,
        })),
    }));
    assert_eq!(engine_error_code(&rpc_err), "PermissionDenied");
}

#[tokio::test]
async fn test_probe_computer_use_engine_success() {
    let dir = tempdir().unwrap();
    let engine_script = dir.path().join("fake_engine.sh");
    let script_content = format!(
        r#"#!/bin/sh
cat << 'EOF'
{{"jsonrpc":"2.0","result":{{"abi":"{ENGINE_ABI}","protocolVersion":"{PROTOCOL_VERSION}","engineVersion":"1.0.0","buildSha":"abc1234"}},"id":1}}
{{"jsonrpc":"2.0","result":{{"backend":"test","capture":true,"input":true,"ax":true,"backgroundWindowInput":false,"deliveryModes":[],"capturePermission":"granted","inputPermission":"granted","axPermission":"granted","displayCount":1,"focusGuard":false,"stopPath":"global","screenLocked":false}},"id":2}}
EOF
"#
    );
    fs::write(&engine_script, script_content).unwrap();
    let mut perms = match fs::metadata(&engine_script).map(|m| m.permissions()) {
        Ok(p) => p,
        Err(_) => return,
    };
    perms.set_mode(0o755);
    fs::set_permissions(&engine_script, perms).unwrap();

    let env = BTreeMap::new();
    let res = probe_computer_use_engine(
        &engine_script.to_string_lossy(),
        &env,
        COMPUTER_USE_DOCTOR_TIMEOUT_MS,
    )
    .await;

    match res {
        EngineProbeResult::Ok(caps) => {
            assert_eq!(caps.hello.abi, ENGINE_ABI);
            assert_eq!(caps.capabilities.capture_permission, "granted");
        }
        EngineProbeResult::Err { code, message } => {
            panic!("Expected success probe, got Err({code}: {message})");
        }
    }
}

#[tokio::test]
async fn test_probe_computer_use_engine_timeout_and_descendant_cleanup() {
    let dir = tempdir().unwrap();
    let engine_script = dir.path().join("sleep_engine.sh");
    let marker_file = dir.path().join("descendant_running.txt");

    let script_content = format!(
        r#"#!/bin/sh
(
  echo "running" > "{}"
  sleep 30
) &
sleep 30
"#,
        marker_file.display()
    );
    fs::write(&engine_script, script_content).unwrap();
    let mut perms = fs::metadata(&engine_script).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&engine_script, perms).unwrap();

    let env = BTreeMap::new();
    let res = probe_computer_use_engine(
        &engine_script.to_string_lossy(),
        &env,
        200,
    )
    .await;

    match res {
        EngineProbeResult::Err { code, message } => {
            assert_eq!(code, "timeout");
            assert!(message.contains("timed out"));
        }
        EngineProbeResult::Ok(_) => {
            panic!("Expected timeout error");
        }
    }
}

#[tokio::test]
async fn test_probe_multibyte_stderr_truncation() {
    let dir = tempdir().unwrap();
    let engine_script = dir.path().join("multibyte_stderr.sh");
    let script_content = r#"#!/bin/sh
for i in $(seq 1 2000); do
  printf '안녕' >&2
done
exit 1
"#;
    fs::write(&engine_script, script_content).unwrap();
    let mut perms = fs::metadata(&engine_script).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&engine_script, perms).unwrap();

    let env = BTreeMap::new();
    let res = probe_computer_use_engine(
        &engine_script.to_string_lossy(),
        &env,
        COMPUTER_USE_DOCTOR_TIMEOUT_MS,
    )
    .await;

    match res {
        EngineProbeResult::Err { code, message } => {
            assert_eq!(code, "handshake-failed");
            assert!(!message.is_empty());
        }
        EngineProbeResult::Ok(_) => {
            panic!("Expected non-zero exit to fail");
        }
    }
}

#[tokio::test]
async fn test_describe_engine_permissions_passive() {
    let dir = tempdir().unwrap();
    let engine_script = dir.path().join("senpi-desktop-engine");
    let script_content = format!(
        r#"#!/bin/sh
cat << 'EOF'
{{"jsonrpc":"2.0","result":{{"abi":"{ENGINE_ABI}","protocolVersion":"{PROTOCOL_VERSION}","engineVersion":"1.0.0","buildSha":"abc1234"}},"id":1}}
{{"jsonrpc":"2.0","result":{{"backend":"test","capture":true,"input":false,"ax":true,"backgroundWindowInput":false,"deliveryModes":[],"capturePermission":"granted","inputPermission":"denied","axPermission":"granted","displayCount":1,"focusGuard":false,"stopPath":"global","screenLocked":false}},"id":2}}
EOF
"#
    );
    fs::write(&engine_script, script_content).unwrap();
    let mut perms = fs::metadata(&engine_script).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&engine_script, perms).unwrap();

    let env = BTreeMap::new();
    let opts = maho_omo_computer_use::installed_engine::DesktopEngineLocatorOptions {
        platform: Some("linux".to_string()),
        runtime_dir: Some(dir.path().to_string_lossy().to_string()),
        ..Default::default()
    };

    let desc = describe_engine_permissions(None, &env, &opts).await;
    assert!(desc.contains("capturePermission=granted inputPermission=denied axPermission=granted (passive probe)"));
}
