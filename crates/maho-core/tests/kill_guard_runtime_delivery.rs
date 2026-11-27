//! Port of `packages/coding-agent/test/suite/kill-guard-runtime-delivery.test.ts`.
//! Covers holder reference stack, Windows no-op, user vs project settings, `!cmd` user bash vs agent
//! bash tool environment, and `maho-tools` overlay registration.

use maho_core::kill_guard::{
    ActivateKillGuardRuntimeOptions, KillGuardHolder, KillGuardSetting,
    activate_kill_guard_runtime, active_kill_guard_environment, kill_guard_protected_pids,
    read_kill_guard_setting, release_kill_guard_runtime,
};
use maho_core::sdk::{CreateAgentSessionOptions, create_agent_session};
use maho_core::session_manager::SessionManager;
use maho_core::utils::shell::{get_shell_env, get_user_shell_env};
use maho_tools::bash::{
    BashExecOptions, BashExit, BashOperations, BashToolOptions, create_bash_tool_definition,
};
use maho_tools::definition::{AbortSignal, ToolCall, ToolFuture};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

struct HolderGuard(KillGuardHolder);

impl HolderGuard {
    fn new(agent_dir: &Path) -> Self {
        let holder = KillGuardHolder::new();
        activate_kill_guard_runtime(ActivateKillGuardRuntimeOptions {
            holder,
            agent_dir,
            protected_pids: kill_guard_protected_pids,
            platform: None,
        })
        .expect("activate runtime");
        Self(holder)
    }
}

impl Drop for HolderGuard {
    fn drop(&mut self) {
        release_kill_guard_runtime(self.0);
    }
}

fn shim_root(agent_dir: &Path) -> String {
    agent_dir.join("kill-guard").to_string_lossy().into_owned()
}

fn first_path_entry(env: &BTreeMap<String, String>) -> &str {
    env.get("PATH")
        .and_then(|value| value.split(if cfg!(windows) { ';' } else { ':' }).next())
        .unwrap_or("")
}

struct RecordingOps {
    seen_envs: Mutex<Vec<Option<BTreeMap<String, String>>>>,
}

impl RecordingOps {
    fn new() -> Self {
        Self {
            seen_envs: Mutex::new(Vec::new()),
        }
    }
}

impl BashOperations for RecordingOps {
    fn exec<'a>(
        &'a self,
        _command: &'a str,
        _cwd: &'a Path,
        options: BashExecOptions,
    ) -> ToolFuture<'a, BashExit> {
        self.seen_envs.lock().expect("lock").push(options.env);
        Box::pin(async { Ok(BashExit { exit_code: Some(0) }) })
    }
}

#[test]
fn settings_read_respects_only_user_settings_not_project_settings() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().join("project");
    let agent_dir = dir.path().join("agent");
    std::fs::create_dir_all(cwd.join(".senpi")).expect("mkdir .senpi");
    std::fs::create_dir_all(&agent_dir).expect("mkdir agent");

    assert_eq!(
        read_kill_guard_setting(&cwd, Some(&agent_dir)),
        KillGuardSetting::On
    );

    // Project attempting to disable is ignored
    std::fs::write(
        cwd.join(".senpi").join("settings.json"),
        r#"{"terminal":{"killGuard":"off"}}"#,
    )
    .expect("write project settings");
    assert_eq!(
        read_kill_guard_setting(&cwd, Some(&agent_dir)),
        KillGuardSetting::On
    );

    // User disabling is honored
    std::fs::write(
        agent_dir.join("settings.json"),
        r#"{"terminal":{"killGuard":"off"}}"#,
    )
    .expect("write user settings");
    assert_eq!(
        read_kill_guard_setting(&cwd, Some(&agent_dir)),
        KillGuardSetting::Off
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn runtime_delivery_holder_stack_and_user_vs_agent_bash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let first = dir.path().join("agent-first");
    let second = dir.path().join("agent-second");
    std::fs::create_dir_all(&first).expect("mkdir first");
    std::fs::create_dir_all(&second).expect("mkdir second");

    // 1. Holder stack preserves remaining layer on release and stops when last holder releases
    let holder_a = KillGuardHolder::new();
    let holder_b = KillGuardHolder::new();
    struct Cleanup(Vec<KillGuardHolder>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for h in self.0.drain(..) {
                release_kill_guard_runtime(h);
            }
        }
    }
    let _cleanup = Cleanup(vec![holder_a, holder_b]);

    activate_kill_guard_runtime(ActivateKillGuardRuntimeOptions {
        holder: holder_a,
        agent_dir: &first,
        protected_pids: kill_guard_protected_pids,
        platform: None,
    })
    .expect("activate a");
    activate_kill_guard_runtime(ActivateKillGuardRuntimeOptions {
        holder: holder_b,
        agent_dir: &second,
        protected_pids: kill_guard_protected_pids,
        platform: None,
    })
    .expect("activate b");
    assert!(first_path_entry(&get_shell_env()).starts_with(&shim_root(&second)));

    release_kill_guard_runtime(holder_b);
    assert!(first_path_entry(&get_shell_env()).starts_with(&shim_root(&first)));

    activate_kill_guard_runtime(ActivateKillGuardRuntimeOptions {
        holder: holder_b,
        agent_dir: &second,
        protected_pids: kill_guard_protected_pids,
        platform: None,
    })
    .expect("re-activate b");
    release_kill_guard_runtime(holder_a);
    assert!(first_path_entry(&get_shell_env()).starts_with(&shim_root(&second)));

    // maho-tools overlay hook is also registered by activate_kill_guard_runtime
    let overlay = maho_tools::bash_executor::with_kill_guard_env(BTreeMap::from([(
        "PATH".to_owned(),
        "/usr/bin:/bin".to_owned(),
    )]));
    assert!(first_path_entry(&overlay).starts_with(&shim_root(&second)));

    release_kill_guard_runtime(holder_b);
    assert_eq!(
        get_shell_env().get("BASH_ENV").cloned(),
        std::env::var("BASH_ENV").ok()
    );
    let _ = active_kill_guard_environment(&BTreeMap::new());

    // Windows no-op
    let windows = activate_kill_guard_runtime(ActivateKillGuardRuntimeOptions {
        holder: holder_a,
        agent_dir: &first,
        protected_pids: kill_guard_protected_pids,
        platform: Some("win32"),
    })
    .expect("win32 ok");
    assert!(windows.is_none());
    assert!(
        !get_shell_env()
            .get("PATH")
            .unwrap_or(&String::new())
            .contains(&shim_root(&first))
    );

    // 2. User `!cmd` omits runtime layer while agent bash carries it
    let _guard = HolderGuard::new(&first);
    let cwd = dir.path().join("cwd");
    std::fs::create_dir_all(&cwd).expect("mkdir cwd");
    let model = serde_json::from_value(json!({
        "id": "fixture", "name": "fixture", "api": "faux", "provider": "faux",
        "baseUrl": "", "reasoning": false, "input": [], "contextWindow": 128000, "maxTokens": 4096,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}
    }))
    .expect("model");
    let session = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        agent_dir: Some(first.to_string_lossy().into_owned()),
        model: Some(model),
        session_manager: Some(SessionManager::in_memory(
            &cwd.to_string_lossy(),
            None,
            None,
        )),
        tools: Some(Vec::new()),
        ..Default::default()
    })
    .await
    .expect("session")
    .session;

    let ops = Arc::new(RecordingOps::new());
    session
        .execute_bash("true", None, true, None, Some(ops.clone()))
        .await
        .expect("user bash with operations");
    let bash_tool = create_bash_tool_definition(
        PathBuf::from(&cwd),
        BashToolOptions {
            operations: Some(ops.clone()),
            ..Default::default()
        },
    );
    (bash_tool.execute)(ToolCall {
        id: "kill-guard-agent-bash",
        params: json!({ "command": "true" }),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("agent bash with operations");

    let recorded = ops.seen_envs.lock().expect("lock").clone();
    assert_eq!(recorded.len(), 2);
    assert!(
        recorded[0].is_none(),
        "user operations override receives None"
    );
    let agent_env = recorded[1].as_ref().expect("agent bash receives env");
    assert!(first_path_entry(agent_env).starts_with(&shim_root(&first)));
    assert_eq!(
        agent_env.get("BASH_ENV").map(String::as_str),
        Some(
            Path::new(first_path_entry(agent_env))
                .join("bash_env.sh")
                .to_string_lossy()
                .as_ref()
        )
    );
    assert!(agent_env.contains_key("SENPI_KILL_GUARD_EXEC"));

    let user_env = get_user_shell_env();
    assert!(
        !user_env
            .get("PATH")
            .unwrap_or(&String::new())
            .contains(&shim_root(&first))
    );
    assert_eq!(
        user_env.get("BASH_ENV").cloned(),
        std::env::var("BASH_ENV").ok()
    );

    // 3. Default operations for user `!cmd` runs without this session's runtime layer (senpi#5371)
    assert!(first_path_entry(&get_shell_env()).starts_with(&shim_root(&first)));
    let res = session
        .execute_bash(
            r#"printf 'PATH0=%s\nBASH_ENV=%s\nEXEC=%s\nAGENT_PID=%s\nPROTECTED=%s\nPROMPT=%s\n' "${PATH%%:*}" "${BASH_ENV:-}" "${SENPI_KILL_GUARD_EXEC:-}" "${SENPI_KILL_GUARD_AGENT_PID:-}" "${SENPI_KILL_GUARD_PROTECTED:-}" "${PROMPT_COMMAND:-}""#,
            None,
            true,
            None,
            None,
        )
        .await
        .expect("default user bash");
    assert_eq!(res.exit_code, Some(0));
    let val = |prefix: &str| -> String {
        res.output
            .lines()
            .find_map(|line| line.strip_prefix(prefix))
            .unwrap_or("")
            .to_owned()
    };
    assert!(!val("PATH0=").starts_with(&shim_root(&first)));
    assert_eq!(
        val("BASH_ENV="),
        std::env::var("BASH_ENV").unwrap_or_default()
    );
    assert_eq!(
        val("EXEC="),
        std::env::var("SENPI_KILL_GUARD_EXEC").unwrap_or_default()
    );
    assert_eq!(
        val("AGENT_PID="),
        std::env::var("SENPI_KILL_GUARD_AGENT_PID").unwrap_or_default()
    );
    assert_eq!(
        val("PROTECTED="),
        std::env::var("SENPI_KILL_GUARD_PROTECTED").unwrap_or_default()
    );
    assert_eq!(
        val("PROMPT="),
        std::env::var("PROMPT_COMMAND").unwrap_or_default()
    );
    assert!(!res.output.contains(&shim_root(&first)));

    session.dispose().await;
}
