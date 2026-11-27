use maho_ai::utils::abort::AbortController;
use maho_codemode::{
    bridge::protocol::{BridgeConnectionConfig, KernelMemoryThresholds},
    config::{
        feature_settings::{
            JS_ISOLATION_ENVIRONMENT_FLAG, JsIsolation, JsIsolationHost,
            current_js_isolation_host_for, default_js_isolation, resolve_js_isolation,
            resolve_js_isolation_for_host,
        },
        settings::{CodemodeSettings, Environment},
    },
    kernels::js::runtime_name::runtime_name_of,
    kernels::{
        js::{
            context_manager::{JavaScriptKernel, JavaScriptKernelStart},
            process_memory_host::JavaScriptProcessMemoryHost,
            process_worker::decode_process_frame,
            worker_host::JavaScriptKernelMode,
            worker_slot::WorkerSlot,
            worker_startup::WorkerStartupOptions,
        },
        shared::subprocess_contract::KernelRunInput,
    },
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};

fn connection() -> BridgeConnectionConfig {
    BridgeConnectionConfig {
        port: 1,
        token: "process-test".into(),
        local_roots: None,
        artifacts_dir: None,
        parallel_pool_width: Some(2),
    }
}

fn thresholds() -> KernelMemoryThresholds {
    KernelMemoryThresholds {
        gc_watermark_bytes: 50,
        notice_bytes: 100,
        ceiling_bytes: 200,
    }
}

#[test]
fn isolation_environment_overrides_file_only_when_recognized() {
    let settings: CodemodeSettings =
        serde_json::from_value(json!({"isolation":{"js":"process"}})).expect("settings");
    for (value, expected) in [
        ("worker", JsIsolation::Worker),
        ("process", JsIsolation::Process),
        ("PROCESS", JsIsolation::Process),
        (" worker ", JsIsolation::Process),
    ] {
        let env = Environment::from([(JS_ISOLATION_ENVIRONMENT_FLAG.into(), value.into())]);
        let isolation = resolve_js_isolation(&settings, &env);
        assert_eq!(isolation, expected);
    }
    assert_eq!(
        resolve_js_isolation(&CodemodeSettings::default(), &Environment::new()),
        JsIsolation::Worker
    );
    assert!(
        serde_json::from_value::<CodemodeSettings>(json!({"isolation":{"js":"inline"}})).is_err()
    );
}

#[test]
fn process_frame_removes_undefined_fields_without_accepting_raw_json_as_control() {
    let line = format!(
        "token {}",
        json!({"type":"result","cellId":"one","ok":true,"durationMs":0,"valueRepr":{"\u{0000}senpi:undefined:token":1},"memory":{"\u{0000}senpi:undefined:token":1}})
    );
    let result = decode_process_frame(&line, "token");
    assert_eq!(result["type"], "result");
    assert!(result.get("valueRepr").is_none());
    assert!(result.get("memory").is_none());
    assert_eq!(
        decode_process_frame(r#"{"type":"ready"}"#, "token")["type"],
        "text"
    );
}

#[test]
fn footprint_ceiling_does_not_require_gc_and_recycles_only_when_idle() {
    let mut host = JavaScriptProcessMemoryHost::new(thresholds());
    let result =
        host.settled(json!({"type":"result","memory":{"liveBytes":200,"measure":"footprint"}}));
    assert_eq!(result["memory"]["overCeiling"], true);
    assert!(!host.claim_recycle(false));
    assert!(host.claim_recycle(true));
    host.worker_lost();
    let next =
        host.settled(json!({"type":"result","memory":{"liveBytes":10,"measure":"footprint"}}));
    assert_eq!(next["memory"]["recycled"], true);
}

#[test]
fn crash_replacement_is_announced_on_exactly_one_result() {
    let mut host = JavaScriptProcessMemoryHost::new(thresholds());
    host.mark_restarted("exit code 9");
    host.worker_lost();
    let first = host.settled(json!({"type":"result"}));
    let second = host.settled(json!({"type":"result"}));
    assert_eq!(first["kernelState"], "restarted");
    assert!(second.get("kernelState").is_none());
}

#[tokio::test]
async fn process_start_failure_never_downgrades_to_inline() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let connection = connection();
    let abort = AbortController::new();
    let mut slot = WorkerSlot::default();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        slot.ensure_ready(
            WorkerStartupOptions {
                isolation: JsIsolation::Process,
                process_startup_deadline: Duration::from_secs(5),
                cwd,
                session_id: "missing-process",
                parallel_pool_width: 2,
                connection: &connection,
                generation: 0,
                host_tool_names: &[],
                foreign_language_names: &[],
                session_env: None,
                worker_entry: Some(Path::new("/missing/process-entry.js")),
                env_passthrough: None,
                memory: None,
                inherited_env: None,
                kill_guard: None,
            },
            &abort.signal(),
        ),
    )
    .await
    .expect("bounded startup");
    assert!(result.is_err());
    assert_eq!(slot.mode(), JavaScriptKernelMode::Process);
    assert!(!slot.present());
}

async fn cell(kernel: &JavaScriptKernel, id: &str, code: &str) -> Value {
    tokio::time::timeout(
        Duration::from_secs(10),
        kernel.run(
            KernelRunInput {
                cell_id: id.into(),
                code: code.into(),
                timeout_ms: Some(5000),
            },
            |_| {},
        ),
    )
    .await
    .expect("bounded cell")
    .expect("result")
}

#[tokio::test]
async fn direct_process_preserves_state_and_replaces_crashed_child() {
    let kernel = tokio::time::timeout(
        Duration::from_secs(10),
        JavaScriptKernel::start_with_memory(
            Path::new(env!("CARGO_MANIFEST_DIR")),
            "process-e2e",
            JavaScriptKernelStart {
                isolation: JsIsolation::Process,
                parallel_pool_width: 2,
                session_env: None,
                connection: connection(),
                names: Arc::new(|| Ok((vec![], vec![]))),
                shutdown: AbortController::new(),
                memory: KernelMemoryThresholds {
                    gc_watermark_bytes: u64::MAX,
                    notice_bytes: u64::MAX,
                    ceiling_bytes: u64::MAX,
                },
                env_passthrough: None,
                inherited_env: None,
                kill_guard: None,
            },
        ),
    )
    .await
    .expect("bounded start")
    .expect("process kernel");
    let identity = cell(&kernel, "identity", "globalThis.processMarker = 41; [process.pid, (await import('node:worker_threads')).isMainThread]").await;
    let identity: Value =
        serde_json::from_str(identity["valueRepr"].as_str().expect("identity")).expect("json");
    assert_eq!(identity[0], kernel.pid().expect("child pid"));
    assert_eq!(identity[1], true);
    let persisted = cell(&kernel, "persisted", "processMarker + 1").await;
    assert_eq!(persisted["valueRepr"], "42");
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    assert_eq!(persisted["memory"]["measure"], "footprint");
    let crashed = cell(&kernel, "crash", "process.exit(9)").await;
    assert_eq!(crashed["ok"], false);
    assert_eq!(crashed["error"]["name"], "JavaScriptWorkerExitedError");
    assert_eq!(
        crashed["error"]["message"],
        "JavaScript worker exited with code 9"
    );
    let replaced = cell(&kernel, "replaced", "typeof processMarker").await;
    assert_eq!(replaced["valueRepr"], "\"undefined\"");
    assert_eq!(replaced["kernelState"], "restarted");
    let next = cell(&kernel, "next", "1+1").await;
    assert!(next.get("kernelState").is_none());
    kernel.close().await.expect("close");
}

#[test]
fn runtime_name_of_identifies_bun_and_node_binaries() {
    assert_eq!(runtime_name_of("/usr/local/bin/bun"), Some("bun"));
    assert_eq!(runtime_name_of("C:\\bun\\bun.exe"), Some("bun"));
    assert_eq!(runtime_name_of("/opt/node/bin/node"), Some("node"));
    assert_eq!(runtime_name_of("node.CMD"), Some("node"));
    assert_eq!(runtime_name_of("/opt/senpi/bin/senpi"), None);
}

#[test]
fn host_default_isolation_selects_process_for_posix_bun_hosts() {
    for (executable, platform, expected) in [
        (Some("bun"), "darwin", JsIsolation::Process),
        (Some("bun"), "linux", JsIsolation::Process),
        (Some("bun"), "win32", JsIsolation::Worker),
        (Some("node"), "darwin", JsIsolation::Worker),
        (Some("node"), "linux", JsIsolation::Worker),
        (None, "darwin", JsIsolation::Worker),
        (None, "linux", JsIsolation::Worker),
    ] {
        let host = JsIsolationHost {
            executable,
            platform,
        };
        assert_eq!(default_js_isolation(&host), expected);
        assert_eq!(
            resolve_js_isolation_for_host(&CodemodeSettings::default(), &Environment::new(), &host),
            expected
        );
    }
    let posix_bun = current_js_isolation_host_for("/usr/local/bin/bun", "linux");
    let worker_file: CodemodeSettings =
        serde_json::from_value(json!({"isolation":{"js":"worker"}})).expect("worker settings");
    assert_eq!(
        resolve_js_isolation_for_host(&worker_file, &Environment::new(), &posix_bun),
        JsIsolation::Worker
    );
    let env_worker = Environment::from([(JS_ISOLATION_ENVIRONMENT_FLAG.into(), "worker".into())]);
    assert_eq!(
        resolve_js_isolation_for_host(&CodemodeSettings::default(), &env_worker, &posix_bun),
        JsIsolation::Worker
    );
}

#[test]
fn windows_bun_host_defaults_to_worker_unless_overridden() {
    let win_bun = current_js_isolation_host_for("C:\\bun\\bun.exe", "win32");
    assert_eq!(default_js_isolation(&win_bun), JsIsolation::Worker);
    let env_process = Environment::from([(JS_ISOLATION_ENVIRONMENT_FLAG.into(), "process".into())]);
    assert_eq!(
        resolve_js_isolation_for_host(&CodemodeSettings::default(), &env_process, &win_bun),
        JsIsolation::Process
    );
}

#[test]
fn env_isolation_flag_overrides_file_setting() {
    let posix_bun = current_js_isolation_host_for("/usr/local/bin/bun", "linux");
    let process_file: CodemodeSettings =
        serde_json::from_value(json!({"isolation":{"js":"process"}})).expect("process settings");
    let env_worker = Environment::from([(JS_ISOLATION_ENVIRONMENT_FLAG.into(), "worker".into())]);
    assert_eq!(
        resolve_js_isolation_for_host(&process_file, &env_worker, &posix_bun),
        JsIsolation::Worker
    );
}
