#[cfg(not(windows))]
mod unix_process_tests {
    use super::super::harness::{Fixture, pid_gone_within};
    use maho_machine_ops::{
        AbortSignal, ExecRequest, ExecStream, HandleLifecycle, PolicyHooks, SpawnRequest,
        WaitReason, WaitSpec,
    };
    use std::sync::{Arc, Mutex};
    use tokio::sync::oneshot;

    const MIB: usize = 1024 * 1024;
    const BACKGROUND_SLEEP: &str = "sleep 30 & echo $!; wait";

    #[tokio::test]
    async fn exec_exit_0_and_nonzero_and_scope_token() {
        let f = Fixture::create(true).await;

        let res = f
            .ops
            .process()
            .exec(
                ExecRequest {
                    command: "echo hi; echo err >&2".to_owned(),
                    cwd: Some(f.root.clone()),
                    ..Default::default()
                },
                &f.ctx(),
            )
            .await
            .expect("exec hi");
        assert_eq!(res.exit_code, Some(0));
        assert_eq!(String::from_utf8_lossy(&res.stdout), "hi\n");
        assert_eq!(String::from_utf8_lossy(&res.stderr), "err\n");
        assert!(!res.timed_out);

        let nonzero = f
            .ops
            .process()
            .exec(
                ExecRequest {
                    command: "exit 3".to_owned(),
                    ..Default::default()
                },
                &f.ctx(),
            )
            .await
            .expect("exit 3");
        assert_eq!(nonzero.exit_code, Some(3));

        let scopes_dir = f.state_dir.join("machine").join("scopes");
        let token_res = f
            .ops
            .process()
            .exec(
                ExecRequest {
                    command: format!("cat \"{}/$SENPI_MACHINE_SCOPE.json\"", scopes_dir.display()),
                    ..Default::default()
                },
                &f.ctx(),
            )
            .await
            .expect("read scope file");
        assert_eq!(token_res.exit_code, Some(0));
        let parsed: serde_json::Value =
            serde_json::from_slice(&token_res.stdout).expect("parse scope json");
        assert_eq!(parsed["token"].as_str(), token_res.scope_token.as_deref());
    }

    #[tokio::test]
    async fn exec_timeout_kills_group() {
        let f = Fixture::create(true).await;
        let (pid_tx, pid_rx) = oneshot::channel::<u32>();
        let pid_tx = Arc::new(Mutex::new(Some(pid_tx)));
        let acc = Arc::new(Mutex::new(String::new()));
        let on_output = {
            let pid_tx = Arc::clone(&pid_tx);
            let acc = Arc::clone(&acc);
            Arc::new(move |chunk: &[u8], stream: ExecStream| {
                if stream == ExecStream::Stdout {
                    let mut s = acc.lock().expect("lock");
                    s.push_str(&String::from_utf8_lossy(chunk));
                    if let Some(first_line) = s.split('\n').next()
                        && let Ok(pid) = first_line.trim().parse::<u32>()
                        && s.contains('\n')
                        && let Some(tx) = pid_tx.lock().expect("lock").take()
                    {
                        let _ = tx.send(pid);
                    }
                }
                Ok(())
            })
        };
        let res = f
            .ops
            .process()
            .exec(
                ExecRequest {
                    command: BACKGROUND_SLEEP.to_owned(),
                    timeout_ms: Some(Some(500)),
                    on_output: Some(on_output),
                    ..Default::default()
                },
                &f.ctx(),
            )
            .await
            .expect("timed out exec");
        assert!(res.timed_out);
        let child_pid = pid_rx.await.expect("child pid reported");
        assert!(
            pid_gone_within(child_pid, 5000).await,
            "background child survived timeout"
        );
    }

    #[tokio::test]
    async fn exec_caps_stdout_at_8_mib() {
        let f = Fixture::create(true).await;
        let res = f
            .ops
            .process()
            .exec(
                ExecRequest {
                    command: format!("head -c {} /dev/zero", 9 * MIB),
                    ..Default::default()
                },
                &f.ctx(),
            )
            .await
            .expect("9 MiB output");
        assert_eq!(res.exit_code, Some(0));
        assert!(res.stdout_truncated);
        assert_eq!(res.stdout.len(), 8 * MIB);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn exec_sets_oom_score_adj_1000_on_linux() {
        let f = Fixture::create(true).await;
        let signal = AbortSignal::default();
        let (score_tx, score_rx) = oneshot::channel::<String>();
        let score_tx = Arc::new(Mutex::new(Some(score_tx)));
        let on_spawn = {
            let score_tx = Arc::clone(&score_tx);
            Arc::new(move |pid: u32| {
                if let Ok(val) = std::fs::read_to_string(format!("/proc/{pid}/oom_score_adj"))
                    && let Some(tx) = score_tx.lock().expect("lock").take()
                {
                    let _ = tx.send(val.trim().to_owned());
                }
            })
        };
        let ops = Arc::clone(&f.ops);
        let ctx = f.ctx_with(PolicyHooks::default(), Some(signal.clone()));
        let handle = tokio::spawn(async move {
            ops.process()
                .exec(
                    ExecRequest {
                        command: "sleep 30".to_owned(),
                        on_spawn: Some(on_spawn),
                        ..Default::default()
                    },
                    &ctx,
                )
                .await
        });
        let val = tokio::time::timeout(std::time::Duration::from_secs(10), score_rx)
            .await
            .expect("oom_score_adj read timeout")
            .expect("oom_score_adj recv");
        signal.abort();
        let _ = handle.await.expect("join");
        assert_eq!(val, "1000");
    }

    #[tokio::test]
    async fn exec_abort_kills_group() {
        let f = Fixture::create(true).await;
        let signal = AbortSignal::default();
        let (pid_tx, pid_rx) = oneshot::channel::<u32>();
        let pid_tx = Arc::new(Mutex::new(Some(pid_tx)));
        let acc = Arc::new(Mutex::new(String::new()));
        let on_output = {
            let pid_tx = Arc::clone(&pid_tx);
            let acc = Arc::clone(&acc);
            Arc::new(move |chunk: &[u8], stream: ExecStream| {
                if stream == ExecStream::Stdout {
                    let mut s = acc.lock().expect("lock");
                    s.push_str(&String::from_utf8_lossy(chunk));
                    if let Some(first_line) = s.split('\n').next()
                        && let Ok(pid) = first_line.trim().parse::<u32>()
                        && s.contains('\n')
                        && let Some(tx) = pid_tx.lock().expect("lock").take()
                    {
                        let _ = tx.send(pid);
                    }
                }
                Ok(())
            })
        };
        let ops = Arc::clone(&f.ops);
        let ctx = f.ctx_with(PolicyHooks::default(), Some(signal.clone()));
        let running = tokio::spawn(async move {
            ops.process()
                .exec(
                    ExecRequest {
                        command: BACKGROUND_SLEEP.to_owned(),
                        on_output: Some(on_output),
                        ..Default::default()
                    },
                    &ctx,
                )
                .await
        });
        let child_pid = tokio::time::timeout(std::time::Duration::from_secs(10), pid_rx)
            .await
            .expect("wait bg pid")
            .expect("bg pid");
        signal.abort();
        let res = running.await.expect("join").expect("exec aborted");
        assert!(res.aborted);
        assert!(
            pid_gone_within(child_pid, 5000).await,
            "background child survived abort"
        );
    }

    #[tokio::test]
    async fn exec_50_concurrent_start_without_queueing() {
        let f = Fixture::create(true).await;
        let go = f.base.join("go");
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (all_tx, all_rx) = oneshot::channel::<()>();
        let all_tx = Arc::new(Mutex::new(Some(all_tx)));
        let mut joins = Vec::new();
        for _ in 0..50 {
            let ops = Arc::clone(&f.ops);
            let ctx = f.ctx();
            let started = Arc::clone(&started);
            let all_tx = Arc::clone(&all_tx);
            let go_path = go.clone();
            joins.push(tokio::spawn(async move {
                let on_spawn = Arc::new(move |_pid: u32| {
                    if started.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == 50
                        && let Some(tx) = all_tx.lock().expect("lock").take()
                    {
                        let _ = tx.send(());
                    }
                });
                ops.process()
                    .exec(
                        ExecRequest {
                            command: format!(
                                "while [ ! -f \"{}\" ]; do sleep 0.05; done; echo done",
                                go_path.display()
                            ),
                            on_spawn: Some(on_spawn),
                            ..Default::default()
                        },
                        &ctx,
                    )
                    .await
            }));
        }
        tokio::time::timeout(std::time::Duration::from_secs(20), all_rx)
            .await
            .expect("all 50 execs spawned")
            .expect("all_tx");
        tokio::fs::write(&go, "").await.expect("write go file");
        let mut ok_count = 0usize;
        for j in joins {
            let res = j.await.expect("join").expect("exec result");
            if res.exit_code == Some(0) {
                ok_count += 1;
            }
        }
        assert_eq!(ok_count, 50);
    }

    #[tokio::test]
    async fn sweep_ends_token_members() {
        let f = Fixture::create(true).await;
        let res = f
            .ops
            .process()
            .exec(
                ExecRequest {
                    command: "sleep 30 >/dev/null 2>&1 & echo $!".to_owned(),
                    ..Default::default()
                },
                &f.ctx(),
            )
            .await
            .expect("exec background");
        let survivor: u32 = String::from_utf8_lossy(&res.stdout)
            .trim()
            .parse()
            .expect("survivor pid");
        assert!(!pid_gone_within(survivor, 0).await);
        let token = res.scope_token.expect("scope token");
        f.ops.process().sweep(&[token]).await.expect("sweep");
        assert!(pid_gone_within(survivor, 3000).await);
    }

    #[tokio::test]
    async fn spawn_write_wait_kill() {
        let f = Fixture::create(true).await;
        let handle = f
            .ops
            .process()
            .spawn(
                SpawnRequest {
                    command: "/bin/sh".to_owned(),
                    args: vec![
                        "-c".to_owned(),
                        "read line; echo got:$line; sleep 30".to_owned(),
                    ],
                    ..Default::default()
                },
                &f.ctx(),
            )
            .await
            .expect("spawn pty");
        assert!(handle.pid() > 0);
        assert!(f.ops.process().handle(handle.id()).is_some());
        assert_eq!(f.ops.process().list(Some("conformance-device")).len(), 1);
        let effect = handle.write(b"abc\n", None).await.expect("write stdin");
        assert_eq!(effect.accepted_bytes, 4);
        let waited = handle
            .wait(WaitSpec {
                output: Some("got:abc".to_owned()),
                timeout_ms: 10_000,
            })
            .await
            .expect("wait matched");
        assert_eq!(waited.reason, WaitReason::Matched);
        assert_eq!(waited.status.state, HandleLifecycle::Running);
        handle.kill(None).await.expect("kill handle");
        assert_eq!(handle.status().state, HandleLifecycle::Killed);
        assert!(pid_gone_within(handle.pid(), 5000).await);
        let after = handle
            .wait(WaitSpec {
                output: None,
                timeout_ms: 1000,
            })
            .await
            .expect("wait exited");
        assert_eq!(after.reason, WaitReason::Exited);
    }
}
