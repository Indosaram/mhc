use super::*;
use crate::abort::AbortController;
use crate::lsp::client::LspDiagnosticsResult;
use crate::lsp::manager::ClientFuture;
use crate::lsp::manager::LspManagerOptions;
use crate::lsp::manager::ManagedLspClient;
use crate::lsp::types::Position;
use crate::lsp::types::Range;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::Notify;

fn diagnostic(message: &str) -> Diagnostic {
    Diagnostic {
        range: Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 0,
                character: 1,
            },
        },
        severity: Some(1),
        code: None,
        source: None,
        message: message.to_string(),
    }
}

fn typescript_server() -> ResolvedServer {
    ResolvedServer {
        id: "typescript".to_string(),
        command: vec!["typescript-language-server".to_string()],
        extensions: vec![".ts".to_string()],
        priority: 1.0,
        env: None,
        initialization: None,
    }
}

type DiagnosticsHandler = Box<dyn Fn(&str) -> Result<Vec<Diagnostic>, LspError> + Send + Sync>;

struct FakeClient {
    active: AtomicUsize,
    max_active: AtomicUsize,
    stop_calls: AtomicUsize,
    start_started: Arc<Notify>,
    start_release: Option<Arc<Notify>>,
    handler: DiagnosticsHandler,
}

impl FakeClient {
    fn new(handler: DiagnosticsHandler) -> Self {
        Self {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            stop_calls: AtomicUsize::new(0),
            start_started: Arc::new(Notify::new()),
            start_release: None,
            handler,
        }
    }
}

impl ManagedLspClient for FakeClient {
    fn start(&self) -> ClientFuture<'_, Result<(), LspError>> {
        Box::pin(async move {
            self.start_started.notify_one();
            if let Some(release) = &self.start_release {
                release.notified().await;
            }
            Ok(())
        })
    }

    fn initialize(&self) -> ClientFuture<'_, Result<(), LspError>> {
        Box::pin(async { Ok(()) })
    }

    fn stop(&self) -> ClientFuture<'_, ()> {
        Box::pin(async move {
            self.stop_calls.fetch_add(1, Ordering::SeqCst);
        })
    }

    fn is_alive(&self) -> bool {
        true
    }

    fn command(&self) -> Vec<String> {
        vec!["fake".to_string()]
    }

    fn diagnostics<'a>(
        &'a self,
        file_path: &'a str,
        _signal: Option<&'a AbortSignal>,
    ) -> ClientFuture<'a, Result<LspDiagnosticsResult, LspError>> {
        Box::pin(async move {
            let now_active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(now_active, Ordering::SeqCst);
            tokio::task::yield_now().await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            (self.handler)(file_path).map(|items| LspDiagnosticsResult {
                items,
                transient_error: None,
            })
        })
    }
}

fn manager_for(client: Arc<FakeClient>) -> Arc<LspManager> {
    LspManager::new(LspManagerOptions {
        client_factory: Some(Arc::new(move |_, _| {
            Ok(client.clone() as Arc<dyn ManagedLspClient>)
        })),
        reaper_interval_ms: Some(60_000),
        ..LspManagerOptions::default()
    })
}

fn fixed_files(files: Vec<String>) -> ListFiles {
    Arc::new(move |_, _, _| files.clone())
}

#[tokio::test]
async fn bounded_concurrency_keeps_file_failures_structured() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = workspace.path().to_string_lossy().into_owned();
    let file = |name: &str| workspace.path().join(name).to_string_lossy().into_owned();
    let files = vec![
        file("a.ts"),
        file("b.ts"),
        file("c.ts"),
        file("d.ts"),
        file("e.ts"),
    ];
    let mut by_file: HashMap<String, Result<Vec<Diagnostic>, String>> = HashMap::new();
    by_file.insert(file("a.ts"), Ok(vec![diagnostic("a")]));
    by_file.insert(file("b.ts"), Err("b failed".to_string()));
    by_file.insert(file("c.ts"), Ok(vec![diagnostic("c")]));
    by_file.insert(file("d.ts"), Ok(vec![diagnostic("d")]));
    by_file.insert(file("e.ts"), Ok(vec![diagnostic("e")]));
    let client = Arc::new(FakeClient::new(Box::new(move |path| {
        match by_file.get(path) {
            Some(Ok(items)) => Ok(items.clone()),
            Some(Err(message)) => Err(LspError::other(message.clone())),
            None => Ok(Vec::new()),
        }
    })));
    let manager = manager_for(client.clone());

    let result = aggregate_diagnostics_for_directory(
        &root,
        ".ts",
        Some(SeverityFilter::All),
        Some(4),
        DirectoryDiagnosticsOptions {
            list_files: Some(fixed_files(files)),
            manager: Some(manager.clone()),
            max_concurrency: Some(2),
            workspace_root: Some(root.clone()),
            server: Some(typescript_server()),
            signal: None,
        },
    )
    .await
    .expect("aggregate");
    manager.stop_all().await;

    assert!(client.max_active.load(Ordering::SeqCst) <= 2);
    assert_eq!(
        result.file_failures,
        vec![DirectoryDiagnosticsFileFailure {
            file: file("b.ts"),
            error: "b failed".to_string(),
        }]
    );
    assert_eq!(result.total_diagnostics, 3);
    assert!(result.output.contains("File processing errors:"));
}

#[tokio::test]
async fn abort_after_first_file_schedules_no_later_file() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = workspace.path().to_string_lossy().into_owned();
    let file = |name: &str| workspace.path().join(name).to_string_lossy().into_owned();
    let files = vec![file("a.ts"), file("b.ts"), file("c.ts")];
    let controller = Arc::new(AbortController::new());
    let visited = Arc::new(Mutex::new(Vec::<String>::new()));
    let abort = controller.clone();
    let seen = visited.clone();
    let client = Arc::new(FakeClient::new(Box::new(move |path| {
        seen.lock().expect("visited").push(path.to_string());
        abort.abort();
        Ok(Vec::new())
    })));
    let manager = manager_for(client);

    let result = aggregate_diagnostics_for_directory(
        &root,
        ".ts",
        Some(SeverityFilter::All),
        Some(3),
        DirectoryDiagnosticsOptions {
            list_files: Some(fixed_files(files)),
            manager: Some(manager.clone()),
            max_concurrency: Some(1),
            workspace_root: Some(root.clone()),
            server: Some(typescript_server()),
            signal: Some(controller.signal()),
        },
    )
    .await
    .expect("aggregate");
    manager.stop_all().await;

    assert_eq!(result.total_diagnostics, 0);
    assert_eq!(*visited.lock().expect("visited"), vec![file("a.ts")]);
}

#[tokio::test]
async fn aborting_cold_acquisition_settles_and_cleans_up_before_startup_releases() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = workspace.path().to_string_lossy().into_owned();
    let controller = AbortController::new();
    let release = Arc::new(Notify::new());
    let mut fake = FakeClient::new(Box::new(|_| Ok(Vec::new())));
    fake.start_release = Some(release.clone());
    let started = fake.start_started.clone();
    let client = Arc::new(fake);
    let manager = manager_for(client.clone());
    let options = DirectoryDiagnosticsOptions {
        list_files: Some(fixed_files(vec![
            workspace.path().join("a.ts").to_string_lossy().into_owned(),
        ])),
        manager: Some(manager.clone()),
        max_concurrency: None,
        workspace_root: Some(root.clone()),
        server: Some(typescript_server()),
        signal: Some(controller.signal()),
    };
    let aggregation = tokio::spawn({
        let root = root.clone();
        async move {
            aggregate_diagnostics_for_directory(
                &root,
                ".ts",
                Some(SeverityFilter::All),
                Some(1),
                options,
            )
            .await
        }
    });

    started.notified().await;
    controller.abort();
    let settled = tokio::time::timeout(Duration::from_millis(100), aggregation).await;
    release.notify_one();

    let result = settled
        .expect("settles before startup release")
        .expect("join");
    assert!(matches!(result, Err(LspError::Aborted)), "{result:?}");
    assert_eq!(result.expect_err("aborted").name(), "AbortError");
    assert_eq!(manager.client_count(), 0);
    assert_eq!(client.stop_calls.load(Ordering::SeqCst), 1);
    manager.stop_all().await;
}
