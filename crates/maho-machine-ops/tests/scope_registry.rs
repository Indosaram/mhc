#[cfg(not(windows))]
mod unix_scope_registry_tests {
    use maho_machine_ops::process::exec::{KillTreeFn, curated_env};
    use maho_machine_ops::process::in_process_holder::create_in_process_holder_link;
    use maho_machine_ops::process::scopes::{SCOPE_ENV, ScopeRegistry, StartTimeLookup};
    use maho_machine_ops::{
        AbortSignal, DEFAULT_LIMITS, EnvMode, ExecRequest, LocalMachineOpsOptions, OpContext,
        OpPrincipal, OpScope, PolicyHooks, PrincipalKind, create_local_machine_ops,
    };
    use std::collections::BTreeMap;
    use std::process::Stdio;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ctx() -> OpContext {
        OpContext {
            scope: OpScope {
                id: "scope-registry-test".to_owned(),
                root: None,
            },
            principal: OpPrincipal {
                kind: PrincipalKind::LocalSession,
                id: "scope-registry-test".to_owned(),
            },
            signal: AbortSignal::default(),
            limits: DEFAULT_LIMITS,
            policy: PolicyHooks::default(),
            audit: None,
            path_lock: None,
        }
    }

    fn process_gone(pid: u32) -> bool {
        use nix::errno::Errno;
        use nix::sys::signal::kill;
        use nix::unistd::Pid;
        let raw = i32::try_from(pid).unwrap_or(i32::MAX);
        matches!(kill(Pid::from_raw(raw), None), Err(Errno::ESRCH))
    }

    async fn env_keys(ops: &dyn maho_machine_ops::MachineOps) -> Vec<String> {
        let mut env = BTreeMap::new();
        if let Ok(path) = std::env::var("PATH") {
            env.insert("PATH".to_owned(), path);
        }
        env.insert("MARKER".to_owned(), "1".to_owned());
        let result = ops
            .process()
            .exec(
                ExecRequest {
                    command: "env".to_owned(),
                    env_mode: EnvMode::Exact,
                    env: Some(env),
                    ..Default::default()
                },
                &ctx(),
            )
            .await
            .expect("env exec");
        assert_eq!(result.exit_code, Some(0));
        let mut keys: Vec<String> = String::from_utf8_lossy(&result.stdout)
            .split('\n')
            .filter_map(|line| line.split_once('=').map(|(k, _)| k.to_owned()))
            .collect();
        keys.sort();
        keys
    }

    #[tokio::test]
    async fn without_state_dir_records_no_token_writes_nothing_and_adds_no_scope_var() {
        let temp = tempfile::tempdir().expect("tempdir");
        let base = tokio::fs::canonicalize(temp.path())
            .await
            .expect("canonicalize");
        let ops = create_local_machine_ops(LocalMachineOpsOptions {
            state_dir: None,
            holder: create_in_process_holder_link(),
            limits: None,
            kill_process_tree: None,
            resolve_fd: None,
        });
        let result = ops
            .process()
            .exec(
                ExecRequest {
                    command: "env".to_owned(),
                    cwd: Some(base.clone()),
                    ..Default::default()
                },
                &ctx(),
            )
            .await
            .expect("exec env");
        assert_eq!(result.exit_code, Some(0));
        assert!(
            !String::from_utf8_lossy(&result.stdout).contains(&format!("{SCOPE_ENV}=")),
            "should not set scope env without stateDir"
        );
        assert!(result.scope_token.is_none());
        let mut rd = tokio::fs::read_dir(&base).await.expect("readdir");
        assert!(rd.next_entry().await.expect("next_entry").is_none());
    }

    #[tokio::test]
    async fn registry_adds_exact_senpi_machine_scope_env_var() {
        let temp = tempfile::tempdir().expect("tempdir");
        let without = env_keys(&*create_local_machine_ops(LocalMachineOpsOptions {
            state_dir: None,
            holder: create_in_process_holder_link(),
            limits: None,
            kill_process_tree: None,
            resolve_fd: None,
        }))
        .await;
        let with_registry = env_keys(&*create_local_machine_ops(LocalMachineOpsOptions {
            state_dir: Some(temp.path().join("state")),
            holder: create_in_process_holder_link(),
            limits: None,
            kill_process_tree: None,
            resolve_fd: None,
        }))
        .await;
        assert_eq!(SCOPE_ENV, "SENPI_MACHINE_SCOPE");
        let mut expected = without;
        expected.push(SCOPE_ENV.to_owned());
        expected.sort();
        assert_eq!(with_registry, expected);
        let _ = curated_env();
    }

    #[tokio::test]
    async fn exec_still_records_token_after_scopes_dir_removed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = temp.path().join("state");
        let ops = create_local_machine_ops(LocalMachineOpsOptions {
            state_dir: Some(state_dir.clone()),
            holder: create_in_process_holder_link(),
            limits: None,
            kill_process_tree: None,
            resolve_fd: None,
        });
        assert_eq!(
            ops.process()
                .exec(
                    ExecRequest {
                        command: "true".to_owned(),
                        ..Default::default()
                    },
                    &ctx(),
                )
                .await
                .expect("true")
                .exit_code,
            Some(0)
        );
        tokio::fs::remove_dir_all(state_dir.join("machine"))
            .await
            .expect("rm machine dir");
        let again = ops
            .process()
            .exec(
                ExecRequest {
                    command: format!(
                        "cat \"{}/$SENPI_MACHINE_SCOPE.json\"",
                        state_dir.join("machine").join("scopes").display()
                    ),
                    ..Default::default()
                },
                &ctx(),
            )
            .await
            .expect("cat scope json");
        assert_eq!(again.exit_code, Some(0));
        let parsed: serde_json::Value =
            serde_json::from_slice(&again.stdout).expect("parse scope json");
        assert_eq!(parsed["token"].as_str(), again.scope_token.as_deref());
    }

    async fn spawn_own_group_leader() -> tokio::process::Child {
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        cmd.spawn().expect("spawn sleep")
    }

    #[tokio::test]
    async fn does_not_signal_group_when_leader_start_time_mismatches() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut child = spawn_own_group_leader().await;
        let pid = child.id().expect("child pid");

        let reads = Arc::new(AtomicUsize::new(0));
        let lookup: StartTimeLookup = {
            let reads = Arc::clone(&reads);
            Arc::new(move |_pid: u32| {
                let n = reads.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    if n == 0 {
                        Some("start-recorded".to_owned())
                    } else {
                        Some("start-of-a-reused-pid".to_owned())
                    }
                })
            })
        };
        let noop_kill: KillTreeFn = Arc::new(|_| {});
        let registry = ScopeRegistry::new(temp.path().join("state"), noop_kill, Some(lookup));
        registry.attach("token-a", pid);
        registry.sweep(&["token-a".to_owned()]).await;

        assert!(!process_gone(pid));
        assert!(child.try_wait().expect("try_wait").is_none());
        assert!(reads.load(Ordering::SeqCst) > 1);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn sweeps_group_whose_leader_start_time_matches() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut child = spawn_own_group_leader().await;
        let pid = child.id().expect("child pid");
        let noop_kill: KillTreeFn = Arc::new(|_| {});
        let registry = ScopeRegistry::new(temp.path().join("state"), noop_kill, None);
        registry.attach("token-b", pid);
        registry.sweep(&["token-b".to_owned()]).await;

        let status = child.wait().await.expect("wait swept child");
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            status.signal(),
            Some(nix::sys::signal::Signal::SIGHUP as i32)
        );
    }
}
