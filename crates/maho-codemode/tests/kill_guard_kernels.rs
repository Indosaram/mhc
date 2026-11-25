//! Port of `packages/senpi-codemode/test/kill-guard-kernels.test.ts` (JavaScript kernels).
//! Executes real Bun cells inside `CodemodeSessionManager` with `kill_guard: true` wired to the
//! HTTP `/kill_guard_check` endpoint and verifies broad commands and protected signals are refused.

#[cfg(unix)]
mod unix {
    use maho_codemode::bridges::output_bridge::OutputExecuteTool;
    use maho_codemode::config::feature_settings::{IsolationSettings, JsIsolation};
    use maho_codemode::config::settings::CodemodeSettings;
    use maho_codemode::extension::session_manager::{
        CodemodeSessionManager, CreateCodemodeSessionManagerOptions,
    };
    use maho_codemode::tool::types::{EvalKernelRunInput, EvalLanguage};
    use maho_ext_api::{ExecuteToolOptions, ExecuteToolResult};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

    const REPORTER: &str = r#"pkill -f 'paid" 2>/dev/null || echo 1' ; true"#;
    const TAG: &str = "[senpi:kill-guard]";

    struct NoopExecutor;
    impl OutputExecuteTool for NoopExecutor {
        fn is_tool_available(&self, _: &str) -> Option<bool> {
            Some(false)
        }
        fn execute_tool<'a>(
            &'a self,
            tool: &'a str,
            _: serde_json::Value,
            _: ExecuteToolOptions,
        ) -> maho_ext_api::ExecuteToolFuture<'a> {
            Box::pin(async move {
                Err(maho_ext_api::ExecuteToolError {
                    code: maho_ext_api::ExecuteToolErrorCode::Blocked,
                    tool_name: tool.to_owned(),
                    message: "no tools".into(),
                    active_tools: Vec::new(),
                })
            })
        }
    }

    fn bun_available() -> bool {
        Command::new("bun")
            .arg("--version")
            .status()
            .is_ok_and(|s| s.success())
    }

    fn error_message(value: &serde_json::Value) -> String {
        value
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned()
    }

    #[tokio::test]
    async fn js_kernel_refuses_broad_commands_and_protected_signals() {
        if !bun_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let fake_bin = temp.path().join("bin");
        std::fs::create_dir_all(&fake_bin).expect("mkdir fake bin");
        let ran_log = temp.path().join("ran.log");
        for name in ["pkill", "kill", "killall", "sudo"] {
            let p = fake_bin.join(name);
            std::fs::write(
                &p,
                format!("#!/bin/sh\necho \"{name} $*\" >> '{}'\n", ran_log.display()),
            )
            .expect("write fake bin");
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake bin");
        }
        let path_with_fakes = format!(
            "{}:{}",
            fake_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let cwd = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();

        for isolation in [JsIsolation::Worker, JsIsolation::Process] {
            let settings = CodemodeSettings {
                parallel_pool_width: 1.0,
                isolation: IsolationSettings {
                    js: Some(isolation),
                },
                ..Default::default()
            };
            let session_env =
                std::collections::HashMap::from([("PATH".into(), path_with_fakes.clone())]);

            let manager = CodemodeSessionManager::start(CreateCodemodeSessionManagerOptions {
                session_id: format!("kg-kernel-{isolation:?}"),
                owner_session_id: None,
                cwd: cwd.clone(),
                settings,
                local_roots: None,
                artifacts_dir: Some(temp.path().join(format!("artifacts-{isolation:?}"))),
                session_env: Some(session_env),
                inherited_env: None,
                kill_guard: true,
                executor: Arc::new(NoopExecutor),
                list_tools: None,
                complete: Arc::new(|_| Box::pin(async { Ok(serde_json::json!({"text": ""})) })),
            })
            .await
            .expect("start codemode session manager");

            let kernel = manager
                .get_javascript_kernel()
                .await
                .expect("get js kernel");
            let eval = |cell_id: &str, code: String| {
                let kernel = Arc::clone(&kernel);
                let cell_id = cell_id.to_owned();
                async move {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(20),
                        kernel.run_eval(EvalKernelRunInput {
                            cell_id,
                            code,
                            timeout_ms: Some(15_000),
                            ..Default::default()
                        }),
                    )
                    .await
                    .expect("cell timeout")
                    .expect("cell transport ok")
                }
            };

            let reporter_json = serde_json::to_string(REPORTER).expect("json");
            let shell = eval(
                "c1",
                format!(
                    "process.getBuiltinModule(\"node:child_process\").execSync({reporter_json}); return \"ran\";"
                ),
            )
            .await;
            assert!(
                error_message(&shell).contains(TAG),
                "{isolation:?} execSync expected TAG, got {shell}"
            );

            // Agent PID itself is protected (R2)
            let agent_pid = std::process::id();
            let sig = eval(
                "c2",
                format!("process.kill({agent_pid}, \"SIGTERM\"); return \"sent\";"),
            )
            .await;
            assert!(
                error_message(&sig).contains(TAG),
                "{isolation:?} process.kill expected TAG, got {sig}"
            );

            // Signal 0 probe on self succeeds
            let probe = eval(
                "c3",
                format!("process.kill({agent_pid}, 0); return \"probed\";"),
            )
            .await;
            assert_eq!(probe["ok"], true, "{isolation:?} probe failed: {probe}");

            // Bun.$ and Bun.spawnSync checks (worker & inline run under bun)
            let reporter_tpl = REPORTER.replace('`', "\\`");
            let bun_sh = eval(
                "c4",
                format!("await Bun.$`{reporter_tpl}`; return \"ran\";"),
            )
            .await;
            assert!(
                error_message(&bun_sh).contains(TAG),
                "{isolation:?} Bun.$ expected TAG, got {bun_sh}"
            );

            let bun_interp = eval(
                "c5",
                "const p = \"a||b\"; await Bun.$`pkill -f ${p}`; return \"ran\";".into(),
            )
            .await;
            assert!(
                error_message(&bun_interp).contains(TAG),
                "{isolation:?} interpolated Bun.$ expected TAG, got {bun_interp}"
            );

            let _ = eval(
                "c6",
                "globalThis.late = new Promise((resolve) => setTimeout(() => { try { Bun.$`pkill -f \".*\"`; resolve(\"ran\"); } catch (error) { resolve(String(error)); } }, 0)); return \"armed\";".into(),
            )
            .await;
            let late_res = eval("c7", "return await globalThis.late;".into()).await;
            assert!(
                late_res["valueRepr"].as_str().unwrap_or("").contains(TAG),
                "{isolation:?} late timer expected TAG, got {late_res}"
            );

            let bun_spawn = eval(
                "c8",
                "Bun.spawnSync([\"pkill\", \"-f\", \"a||b\"]); return \"ran\";".into(),
            )
            .await;
            assert!(
                error_message(&bun_spawn).contains(TAG),
                "{isolation:?} Bun.spawnSync expected TAG, got {bun_spawn}"
            );

            let bun_ok = eval(
                "c9",
                "await Bun.$`echo fine`.quiet(); return \"ok\";".into(),
            )
            .await;
            assert_eq!(bun_ok["ok"], true);
            assert_eq!(bun_ok["valueRepr"], "\"ok\"");

            let custom_shell_bad = eval(
                "c10",
                "const guarded = new Bun.$.Shell(); await guarded`pkill -f \".*\"`; return \"ran\";"
                    .into(),
            )
            .await;
            assert!(
                error_message(&custom_shell_bad).contains(TAG),
                "{isolation:?} new Shell() broad expected TAG, got {custom_shell_bad}"
            );

            let custom_shell_ok = eval(
                "c11",
                "const plain = new Bun.$.Shell(); return (await plain`echo fine`.text()).trim();"
                    .into(),
            )
            .await;
            assert_eq!(custom_shell_ok["ok"], true);
            assert_eq!(custom_shell_ok["valueRepr"], "\"fine\"");

            assert!(
                std::fs::read_to_string(&ran_log)
                    .unwrap_or_default()
                    .is_empty(),
                "no fake binary may have executed"
            );

            let _ = manager.dispose().await;
            let _ = EvalLanguage::Js;
            let _: fn() -> Option<ExecuteToolResult> = || None;
        }
    }
}
