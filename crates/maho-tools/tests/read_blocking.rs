use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use maho_tools::{
    definition::{AbortSignal, ToolCall, ToolError, ToolFuture},
    read::{
        ReadExecutionPolicy, ReadOperations, ReadToolOptions, create_read_tool_definition,
        flags_are_dataless,
    },
};
use serde_json::json;
use tokio::sync::{Semaphore, mpsc};

struct GatedReadOperations {
    entered: mpsc::UnboundedSender<()>,
    completed: mpsc::UnboundedSender<()>,
    release: Arc<Semaphore>,
    active: Arc<AtomicUsize>,
    maximum_active: Arc<AtomicUsize>,
}

#[test]
fn dataless_classifier_only_matches_the_darwin_dataless_bit() {
    assert!(flags_are_dataless(0x4000_0000));
    assert!(flags_are_dataless(0x4000_0020));
    assert!(!flags_are_dataless(0));
    assert!(!flags_are_dataless(0x0000_0020));
}

impl ReadOperations for GatedReadOperations {
    fn read_file<'a>(&'a self, _path: &'a Path) -> ToolFuture<'a, Vec<u8>> {
        Box::pin(async move {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum_active.fetch_max(active, Ordering::SeqCst);
            self.entered
                .send(())
                .map_err(|error| ToolError::Message(error.to_string()))?;
            let permit = self
                .release
                .acquire()
                .await
                .map_err(|error| ToolError::Message(error.to_string()))?;
            permit.forget();
            self.active.fetch_sub(1, Ordering::SeqCst);
            self.completed
                .send(())
                .map_err(|error| ToolError::Message(error.to_string()))?;
            Ok(b"ready".to_vec())
        })
    }

    fn access<'a>(&'a self, _path: &'a Path) -> ToolFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

struct Fixture {
    operations: Arc<GatedReadOperations>,
    entered: mpsc::UnboundedReceiver<()>,
    completed: mpsc::UnboundedReceiver<()>,
    release: Arc<Semaphore>,
    maximum_active: Arc<AtomicUsize>,
}

impl Fixture {
    fn new() -> Self {
        let (entered_send, entered) = mpsc::unbounded_channel();
        let (completed_send, completed) = mpsc::unbounded_channel();
        let release = Arc::new(Semaphore::new(0));
        let maximum_active = Arc::new(AtomicUsize::new(0));
        Self {
            operations: Arc::new(GatedReadOperations {
                entered: entered_send,
                completed: completed_send,
                release: Arc::clone(&release),
                active: Arc::new(AtomicUsize::new(0)),
                maximum_active: Arc::clone(&maximum_active),
            }),
            entered,
            completed,
            release,
            maximum_active,
        }
    }
}

fn execute(
    operations: Arc<dyn ReadOperations>,
    policy: ReadExecutionPolicy,
    signal: AbortSignal,
) -> tokio::task::JoinHandle<Result<maho_tools::definition::ToolResult, ToolError>> {
    let tool = create_read_tool_definition(
        std::env::temp_dir(),
        ReadToolOptions {
            operations: Some(operations),
            execution_policy: Some(policy),
            folder: None,
            ..Default::default()
        },
    );
    tokio::spawn(async move {
        (tool.execute)(ToolCall {
            id: "blocked-read",
            params: json!({"path":"fixture.txt"}),
            signal,
            on_update: None,
            context: None,
        })
        .await
    })
}

#[tokio::test(start_paused = true)]
async fn read_deadline_returns_before_the_underlying_operation_finishes() {
    let mut fixture = Fixture::new();
    let policy = ReadExecutionPolicy::new(Duration::from_secs(30), 1);

    let read = execute(fixture.operations.clone(), policy, AbortSignal::default());
    fixture.entered.recv().await.expect("read enters");
    tokio::time::advance(Duration::from_secs(30)).await;

    let error = read
        .await
        .expect("read task joins")
        .expect_err("read reaches its deadline");
    assert!(error.to_string().contains("timed out"), "{error}");
    assert!(
        fixture.completed.try_recv().is_err(),
        "underlying read must still be blocked"
    );

    fixture.release.add_permits(1);
    fixture
        .completed
        .recv()
        .await
        .expect("underlying read settles after release");
}

#[tokio::test]
async fn read_cancellation_returns_before_the_underlying_operation_finishes() {
    let mut fixture = Fixture::new();
    let policy = ReadExecutionPolicy::new(Duration::from_secs(60), 1);
    let signal = AbortSignal::default();

    let read = execute(fixture.operations.clone(), policy, signal.clone());
    fixture.entered.recv().await.expect("read enters");
    signal.abort();

    assert!(matches!(
        read.await.expect("read task joins"),
        Err(ToolError::Aborted)
    ));
    assert!(
        fixture.completed.try_recv().is_err(),
        "underlying read must still be blocked"
    );

    fixture.release.add_permits(1);
    fixture
        .completed
        .recv()
        .await
        .expect("underlying read settles after release");
}

#[tokio::test(start_paused = true)]
async fn timed_out_read_keeps_its_capacity_until_the_underlying_operation_finishes() {
    let mut fixture = Fixture::new();
    let policy = ReadExecutionPolicy::new(Duration::from_secs(10), 1);

    let first = execute(
        fixture.operations.clone(),
        policy.clone(),
        AbortSignal::default(),
    );
    fixture.entered.recv().await.expect("first read enters");
    tokio::time::advance(Duration::from_secs(10)).await;
    first
        .await
        .expect("first task joins")
        .expect_err("first read times out");

    let second = execute(
        fixture.operations.clone(),
        policy.with_deadline(Duration::from_secs(3600)),
        AbortSignal::default(),
    );
    tokio::task::yield_now().await;
    assert_eq!(fixture.maximum_active.load(Ordering::SeqCst), 1);
    assert!(
        fixture.entered.try_recv().is_err(),
        "second read must wait for capacity"
    );

    fixture.release.add_permits(1);
    fixture
        .entered
        .recv()
        .await
        .expect("second read enters after first settles");
    assert_eq!(fixture.maximum_active.load(Ordering::SeqCst), 1);
    fixture.release.add_permits(1);
    second
        .await
        .expect("second task joins")
        .expect("second read succeeds");
}
