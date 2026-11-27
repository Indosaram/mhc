//! Port of `packages/senpi-codemode/test/kill-guard-worker-capture.test.ts`.
//! Verifies `worker-shell-capture.js` + `kill-guard-bridge.js` capture originals without re-entering
//! wrapped `Bun.spawnSync` and guard `new Bun.$.Shell()` instances.

#[cfg(unix)]
mod unix {
    use maho_codemode::bridge::http_server::{BridgeServerOptions, start_bridge_server};
    use maho_codemode::kernels::shared::runtime_asset::require_codemode_runtime_asset;
    use maho_core::kill_guard::{
        KillGuardRule, decide_kill_guard_request, request::KillGuardRequestOptions, verdict::refuse,
    };
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

    fn node_available() -> bool {
        Command::new("node")
            .arg("--version")
            .status()
            .is_ok_and(|s| s.success())
    }

    #[tokio::test]
    async fn worker_shell_capture_guards_without_reentry_and_wraps_bun_shell_constructor() {
        if !node_available() {
            return;
        }
        let agent_pid = i64::from(std::process::id());
        let bridge = start_bridge_server(BridgeServerOptions {
            token: None,
            body_limit_bytes: None,
            on_call: Arc::new(|_| Box::pin(async { Ok(serde_json::json!(null)) })),
            on_emit: Arc::new(|_, _| Box::pin(async { Ok(()) })),
            on_completion: Arc::new(|_| Box::pin(async { Ok(serde_json::json!({})) })),
            on_kill_guard_check: Some(Arc::new(move |body| {
                let options = KillGuardRequestOptions {
                    agent_pid,
                    agent_ppid: None,
                    platform: Some("linux".into()),
                    view: None,
                };
                Box::pin(async move {
                    let verdict = tokio::task::spawn_blocking(move || {
                        decide_kill_guard_request(&body, &options)
                    })
                    .await
                    .unwrap_or_else(|e| refuse(KillGuardRule::R5, &e.to_string(), None));
                    serde_json::to_value(&verdict)
                        .map_err(|e| serde_json::json!({"message": e.to_string()}))
                })
            })),
        })
        .await
        .expect("start bridge");

        let capture_path =
            require_codemode_runtime_asset(None, Path::new("kernels/js/worker-shell-capture.js"))
                .expect("worker-shell-capture asset");
        let bridge_path =
            require_codemode_runtime_asset(None, Path::new("kernels/js/kill-guard-bridge.js"))
                .expect("kill-guard-bridge asset");
        let capture_url = url::Url::from_file_path(&capture_path)
            .expect("url")
            .to_string();
        let mut bridge_url = url::Url::from_file_path(&bridge_path).expect("url");
        bridge_url.set_query(Some(&format!("port={}", bridge.port)));

        let script = format!(
            r#"
import assert from "node:assert/strict";

const originalCalls = [];
const ran = [];
const escape = (v) => v;
function Shell() {{
  const instance = (strings) => {{
    ran.push(strings.join(""));
    return Promise.resolve();
  }};
  return Object.defineProperty(instance, "name", {{ value: "Shell", enumerable: true }});
}}
const $ = Object.defineProperty(
  Object.assign(() => Promise.resolve(), {{ escape }}),
  "Shell",
  {{ value: Shell, enumerable: true }},
);
const spawnSync = (argv) => {{
  originalCalls.push(String(argv[0]));
  return {{ success: true, exitCode: 0, stdout: new Uint8Array(), stderr: new Uint8Array() }};
}};
Object.defineProperty(globalThis, "Bun", {{
  configurable: true,
  value: {{ $, spawn: () => ({{}}), spawnSync }},
}});

const capture = await import({capture_url:?});
const judge = await import({bridge_url_str:?});
const restore = capture.installShellCapture({{
  isActive: () => false,
  emitText: () => {{}},
  killGuard: {{ judge, agentPid: process.pid }},
}});
try {{
  const wrapped = globalThis.Bun.spawnSync;
  let wrapperEntries = 0;
  globalThis.Bun.spawnSync = (argv) => {{
    wrapperEntries += 1;
    return wrapped(argv);
  }};

  globalThis.Bun.spawnSync(["pkill", "-x", "senpi-kill-guard-none"]);
  assert.equal(wrapperEntries, 1);
  assert.deepEqual(originalCalls, ["pkill"]);

  assert.throws(() => globalThis.Bun.spawnSync(["pkill", "-f", ".*"]), /\[senpi:kill-guard\]/);
  assert.deepEqual(originalCalls, ["pkill"]);

  const shell = new globalThis.Bun.$.Shell();
  assert.throws(() => shell`pkill -f 'a||b'`, /\[senpi:kill-guard\]/);
  await shell`echo fine`;
  assert.deepEqual(ran, ["echo fine"]);
}} finally {{
  restore();
  Reflect.deleteProperty(globalThis, "Bun");
}}
"#,
            capture_url = capture_url,
            bridge_url_str = bridge_url.as_str(),
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let test_js = dir.path().join("capture.mjs");
        std::fs::write(&test_js, script).expect("write test script");

        let output = tokio::process::Command::new("node")
            .arg(&test_js)
            .output()
            .await
            .expect("run node capture test");
        bridge.close().await;

        assert!(
            output.status.success(),
            "node failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
