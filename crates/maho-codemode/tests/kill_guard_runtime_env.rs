//! Port of `packages/senpi-codemode/test/kill-guard-runtime-env.test.ts`.
//! Verifies that codemode `create_runtime` merges active kill-guard runtime env into `session_env`
//! unless `terminal.killGuard: "off"` is configured in user settings.

#[cfg(unix)]
mod unix {
    use maho_codemode::extension::runtime_factory::{
        RuntimeExecuteTool, RuntimeHostOptions, RuntimePreparationOptions,
        create_runtime_with_factory, prepare_runtime,
    };
    use maho_codemode::interpreters::detect::InterpreterDetector;
    use maho_codemode::tool::types::{EvalLanguage, EvalRuntimeInfo};
    use maho_core::kill_guard::{
        ActivateKillGuardRuntimeOptions, KillGuardHolder, activate_kill_guard_runtime,
        kill_guard_protected_pids, release_kill_guard_runtime,
    };
    use maho_ext_api::{
        EventBus, ExtensionApi, ExtensionRuntime, ExtensionSessionProfile, LoadedExtension,
        SourceInfo,
    };
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    struct HolderGuard(Option<KillGuardHolder>);

    impl HolderGuard {
        fn activate(agent_dir: &Path) -> (Self, maho_core::kill_guard::KillGuardRuntime) {
            let holder = KillGuardHolder::new();
            let runtime = activate_kill_guard_runtime(ActivateKillGuardRuntimeOptions {
                holder,
                agent_dir,
                protected_pids: kill_guard_protected_pids,
                platform: None,
            })
            .expect("activate ok")
            .expect("runtime present on unix");
            (Self(Some(holder)), runtime)
        }
    }

    impl Drop for HolderGuard {
        fn drop(&mut self) {
            if let Some(holder) = self.0.take() {
                release_kill_guard_runtime(holder);
            }
        }
    }

    async fn capture_session_env(agent_dir: &Path) -> (Option<HashMap<String, String>>, bool) {
        let cwd = tempfile::tempdir().expect("cwd tempdir");
        let home = agent_dir.to_path_buf();
        let api = Arc::new(ExtensionApi::new(
            LoadedExtension::new(
                "codemode-kg",
                cwd.path().to_path_buf(),
                SourceInfo::default(),
            ),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        ));
        let mut detector = InterpreterDetector::new("24.0.0".into(), false);
        let prepared = prepare_runtime(
            RuntimePreparationOptions {
                cwd: cwd.path(),
                home_dir: &home,
                agent_dir: Some(agent_dir),
                environment: &HashMap::new(),
                event: &serde_json::json!({"sessionId": "kill-guard-env"}),
                session_file: None,
                js_runtime: EvalRuntimeInfo {
                    name: "bun".into(),
                    version: "1.4.0".into(),
                    path: Some("/usr/bin/bun".into()),
                    isolation: None,
                },
            },
            &mut detector,
        )
        .await
        .expect("prepare runtime");

        let captured = Arc::new(Mutex::new(None));
        let captured_kg = Arc::new(Mutex::new(false));
        let captured_clone = Arc::clone(&captured);
        let captured_kg_clone = Arc::clone(&captured_kg);

        let executor = Arc::new(RuntimeExecuteTool {
            api,
            active_tools: vec!["eval".into()],
        });
        let session = create_runtime_with_factory(
            prepared,
            RuntimeHostOptions {
                executor,
                active_tools: vec!["eval".into()],
                list_tools: None,
                complete: Arc::new(|_| Box::pin(async { Ok(serde_json::json!({"text": ""})) })),
                session_env: HashMap::new(),
            },
            move |options| {
                *captured_clone.lock().expect("lock") = options.session_env.clone();
                *captured_kg_clone.lock().expect("lock") = options.kill_guard;
                maho_codemode::extension::session_manager::CodemodeSessionManager::start(options)
            },
        )
        .await
        .expect("create runtime");

        let _ = session.manager.dispose().await;
        let env = captured.lock().expect("lock").clone();
        let kg = *captured_kg.lock().expect("lock");
        let _ = EvalLanguage::Js;
        (env, kg)
    }

    #[tokio::test]
    async fn kernel_session_env_carries_runtime_layer_unless_user_setting_off() {
        // 1. Active runtime while guard is on -> PATH/BASH_ENV/SENPI_KILL_GUARD_* exported
        let dir_on = tempfile::tempdir().expect("tempdir");
        let (_guard, runtime) = HolderGuard::activate(dir_on.path());
        let (env_on, kg_on) = capture_session_env(dir_on.path()).await;
        assert!(kg_on);
        let env_on = env_on.expect("session_env present");
        let first_path = env_on
            .get("PATH")
            .and_then(|p| p.split(':').next())
            .unwrap_or("");
        assert_eq!(first_path, runtime.dir.to_string_lossy());
        assert_eq!(
            env_on.get("BASH_ENV").map(String::as_str),
            Some(runtime.bash_env.to_string_lossy().as_ref())
        );
        assert_eq!(
            env_on.get("SENPI_KILL_GUARD_EXEC").map(String::as_str),
            Some(runtime.exec.as_str())
        );
        assert_eq!(
            env_on.get("SENPI_KILL_GUARD_AGENT_PID").map(String::as_str),
            Some(std::process::id().to_string().as_str())
        );
        assert!(env_on.contains_key("SENPI_KILL_GUARD_PROTECTED"));
        drop(_guard);

        // 2. terminal.killGuard: "off" in user settings keeps runtime layer out even when holder is active
        let dir_off = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir_off.path().join("settings.json"),
            r#"{"terminal":{"killGuard":"off"}}"#,
        )
        .expect("write settings");
        let (_guard_off, runtime_off) = HolderGuard::activate(dir_off.path());
        let (env_off, kg_off) = capture_session_env(dir_off.path()).await;
        assert!(!kg_off);
        let env_off = env_off.expect("session_env present");
        assert!(!env_off.contains_key("BASH_ENV"));
        assert!(!env_off.contains_key("SENPI_KILL_GUARD_EXEC"));
        let path_entries: Vec<&str> = env_off
            .get("PATH")
            .map(|p| p.split(':').collect())
            .unwrap_or_default();
        assert!(!path_entries.contains(&runtime_off.dir.to_string_lossy().as_ref()));
        drop(_guard_off);

        // 3. No holder active -> adds nothing
        let dir_none = tempfile::tempdir().expect("tempdir");
        let (env_none, kg_none) = capture_session_env(dir_none.path()).await;
        assert!(kg_none);
        let env_none = env_none.expect("session_env present");
        assert!(!env_none.contains_key("BASH_ENV"));
        assert!(!env_none.contains_key("SENPI_KILL_GUARD_EXEC"));
    }
}
