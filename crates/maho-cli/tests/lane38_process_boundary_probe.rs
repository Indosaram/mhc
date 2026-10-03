#[test]
fn role_consumption_does_not_leak_to_ordinary_descendants() {
    const PROBE: &str = "MAHO_PARENT_ROLE_PROBE";
    if std::env::var_os(PROBE).is_some() {
        let mut env = std::env::vars().collect();
        assert!(maho_cli::experimental::process::consume_internal_process_role(&mut env).expect("valid role").is_some());
        assert!(maho_cli::experimental::process::consume_internal_process_role(&mut env).expect("consumed role").is_none());
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        let result = runtime.block_on(maho_core::exec::exec_command("/bin/sh",
            &["-c".into(), "printf '%s' \"${__PI_INTERNAL_SPAWN-unset}\"".into()], ".", &Default::default()));
        let config = runtime.block_on(maho_core::resolve_config_command::run_config_command(
            "!printf '%s' \"${__PI_INTERNAL_SPAWN-unset}\"", &[]));
        assert_eq!(result.code, 0);
        assert_eq!(config.as_deref(), Some("unset"), "config helpers must not inherit the role");
        assert_eq!(result.stdout, "unset", "consumed role must not reach ordinary subprocesses");
        assert_eq!(std::env::var("__PI_INTERNAL_SPAWN").as_deref(), Ok("coordinator"), "child filtering must not mutate the parent environment");
        let explicit = runtime.block_on(maho_core::exec::exec_command("/bin/sh",
            &["-c".into(), "printf '%s:%s' \"${__PI_INTERNAL_SPAWN-unset}\" \"$MAHO_ROLE_FIXTURE\"".into()], ".",
            &maho_core::exec::ExecOptions { env: Some(std::collections::HashMap::from([
                ("__PI_INTERNAL_SPAWN".into(), "server".into()), ("MAHO_ROLE_FIXTURE".into(), "kept".into()),
            ])), ..Default::default() }));
        assert_eq!(explicit.code, 0);
        assert_eq!(explicit.stdout, "unset:kept");
        let config_value = runtime.block_on(maho_core::resolve_config_value::run_config_command(
            "!printf '%s' \"${__PI_INTERNAL_SPAWN-unset}\"", None));
        assert_eq!(config_value.as_deref(), Some("unset"), "config value helpers must not inherit role");
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "role_consumption_does_not_leak_to_ordinary_descendants", "--nocapture"])
        .env(PROBE, "1").env("__PI_INTERNAL_SPAWN", "coordinator")
        .output().expect("isolated role process");
    assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}

#[test]
fn intentional_internal_spawns_keep_only_the_requested_role() {
    use maho_cli::experimental::process::{InternalProcessRole, spawn_internal_process};
    const PROBE: &str = "MAHO_INTENTIONAL_ROLE_PROBE";
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    if let Ok(expected) = std::env::var(PROBE) {
        assert_eq!(std::env::var("__PI_INTERNAL_SPAWN").expect("role"), expected);
        let output = runtime.block_on(maho_core::exec::exec_command("/bin/sh",
            &["-c".into(), "printf '%s' \"${__PI_INTERNAL_SPAWN-unset}\"".into()], ".", &Default::default()));
        assert_eq!(output.code, 0);
        assert_eq!(output.stdout, "unset");
        return;
    }
    for (role, expected) in [(InternalProcessRole::Coordinator, "coordinator"),
        (InternalProcessRole::Server, "server"), (InternalProcessRole::SessionWorker, "session-worker")] {
        let mut env: std::collections::BTreeMap<_, _> = std::env::vars().collect();
        env.insert(PROBE.into(), expected.into());
        env.insert("__PI_INTERNAL_SPAWN".into(), "invalid-inherited-role".into());
        let original = env.clone();
        let status = runtime.block_on(async {
            let mut child = spawn_internal_process(role,
                &["--exact".into(), "intentional_internal_spawns_keep_only_the_requested_role".into()],
                &std::env::current_dir().expect("cwd"), &env).expect("internal child");
            match tokio::time::timeout(std::time::Duration::from_secs(10), child.wait()).await {
                Ok(status) => status.expect("reaped child"),
                Err(error) => {
                    maho_cli::experimental::process::terminate_internal_process(&mut child).await.expect("cleanup timed-out child");
                    panic!("internal child did not exit: {error}");
                }
            }
        });
        assert!(status.success(), "{expected}: {status}");
        assert_eq!(env, original);
    }
}
