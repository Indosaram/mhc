use super::*;

struct StartingActor {
    commands: mpsc::UnboundedSender<Command>,
    actor: tokio::task::JoinHandle<()>,
    run: oneshot::Receiver<Result<Value, String>>,
    _directory: tempfile::TempDir,
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("startup control completed before deadline")
}

impl StartingActor {
    async fn new() -> Self {
        let directory = tempfile::tempdir().expect("isolated cwd");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("startup event listener");
        std::fs::write(
            directory.path().join("startup-port"),
            listener
                .local_addr()
                .expect("listener address")
                .port()
                .to_string(),
        )
        .expect("startup event address");
        let options = WorkerOptions {
            isolation: Default::default(),
            cached_memory: Default::default(),
            cwd: directory.path().into(),
            session_id: "startup-controls".into(),
            width: 1,
            environment: None,
            connection: BridgeConnectionConfig {
                port: 1,
                token: "test".into(),
                local_roots: None,
                artifacts_dir: None,
                parallel_pool_width: None,
            },
            names: Arc::new(|| Ok((vec![], vec![]))),
            memory: crate::bridge::protocol::KernelMemoryThresholds {
                gc_watermark_bytes: 1_073_741_824,
                notice_bytes: 1_610_612_736,
                ceiling_bytes: 2_147_483_648,
            },
            worker_entry: Some(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/startup-worker.js"),
            ),
        };
        let (commands, receiver) = mpsc::unbounded_channel();
        let tools = Arc::new(super::super::kernel_tools_host::KernelToolHostPump::new(
            Arc::new(|_| {}),
            Arc::new(|| false),
        ));
        let actor = tokio::spawn(run_actor(
            options,
            WorkerSlot::default(),
            receiver,
            Default::default(),
            Default::default(),
            tools,
            maho_ai::utils::abort::AbortController::new().signal(),
        ));
        let (response, run) = oneshot::channel();
        assert!(
            commands
                .send(Command::Run(
                    KernelRunInput {
                        cell_id: "waiting".into(),
                        code: "1".into(),
                        timeout_ms: None,
                    },
                    None,
                    None,
                    Box::new(|| Ok("1".into())),
                    None,
                    response,
                ))
                .is_ok()
        );
        bounded(listener.accept())
            .await
            .expect("worker entered init without ready");
        Self {
            commands,
            actor,
            run,
            _directory: directory,
        }
    }

    async fn close(self) {
        let (response, closed) = oneshot::channel();
        assert!(self.commands.send(Command::Close(response)).is_ok());
        bounded(closed)
            .await
            .expect("close reply")
            .expect("worker retired");
        bounded(self.actor).await.expect("actor stopped");
    }
}

#[tokio::test]
async fn queued_cancel_completes_while_worker_never_reports_ready() {
    let mut fixture = StartingActor::new().await;
    let (response, cancelled) = oneshot::channel();

    assert!(
        fixture
            .commands
            .send(Command::Cancel("waiting".into(), "cancel".into(), response))
            .is_ok()
    );

    assert!(bounded(cancelled).await.expect("cancel reply"));
    assert_eq!(
        bounded(&mut fixture.run)
            .await
            .expect("run reply")
            .expect("stopped cell")["ok"],
        false
    );
    fixture.close().await;
}

#[tokio::test]
async fn interrupt_retires_startup_and_reports_lost_state() {
    let mut fixture = StartingActor::new().await;
    let (response, interrupted) = oneshot::channel();

    assert!(
        fixture
            .commands
            .send(Command::Interrupt("stop".into(), None, response))
            .is_ok()
    );

    assert_eq!(
        bounded(interrupted)
            .await
            .expect("interrupt reply")
            .expect("restart"),
        (false, None)
    );
    assert_eq!(
        bounded(&mut fixture.run)
            .await
            .expect("run reply")
            .expect("stopped cell")["ok"],
        false
    );
    fixture.close().await;
}

#[tokio::test]
async fn reset_replaces_worker_that_never_reports_ready() {
    let mut fixture = StartingActor::new().await;
    let (response, reset) = oneshot::channel();

    assert!(fixture.commands.send(Command::Reset(response)).is_ok());

    bounded(reset)
        .await
        .expect("reset reply")
        .expect("replacement ready");
    assert_eq!(
        bounded(&mut fixture.run)
            .await
            .expect("run reply")
            .expect("stopped cell")["ok"],
        false
    );
    fixture.close().await;
}

#[tokio::test]
async fn reset_bring_up_keeps_command_admission_responsive() {
    let fixture = StartingActor::new().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reset startup listener");
    std::fs::write(
        fixture._directory.path().join("startup-port"),
        listener
            .local_addr()
            .expect("listener address")
            .port()
            .to_string(),
    )
    .expect("reset startup address");
    std::fs::remove_file(fixture._directory.path().join("startup-attempt"))
        .expect("next worker also wedges");
    let (response, reset) = oneshot::channel();
    assert!(fixture.commands.send(Command::Reset(response)).is_ok());
    bounded(listener.accept())
        .await
        .expect("reset replacement entered init");
    let (response, cancelled) = oneshot::channel();

    assert!(
        fixture
            .commands
            .send(Command::Cancel("absent".into(), "cancel".into(), response))
            .is_ok()
    );

    assert!(
        !bounded(cancelled)
            .await
            .expect("command admitted during reset")
    );
    fixture.close().await;
    assert!(
        bounded(reset)
            .await
            .expect("pending reset settled on close")
            .is_err()
    );
}
