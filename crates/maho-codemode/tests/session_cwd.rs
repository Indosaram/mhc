use maho_codemode::{
    bridges::output_bridge::OutputExecuteTool,
    extension::{
        session_cwd::{
            CodemodeSessionCwdUnavailableError, assert_session_cwd_available,
            assert_session_cwd_available_sync,
        },
        session_manager::{CodemodeSessionManager, CreateCodemodeSessionManagerOptions},
    },
};
use serde_json::json;
use std::{future::Future, io, path::Path, sync::Arc, time::Duration};

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(15), future)
        .await
        .expect("bounded operation")
}

struct Executor;
impl OutputExecuteTool for Executor {
    fn execute_tool<'a>(
        &'a self,
        _: &'a str,
        _: serde_json::Value,
        _: maho_ext_api::ExecuteToolOptions,
    ) -> maho_ext_api::ExecuteToolFuture<'a> {
        Box::pin(async { Ok(maho_ext_api::AgentToolResult::text("fixture")) })
    }
}

async fn session(cwd: &Path, artifacts: &Path) -> CodemodeSessionManager {
    bounded(CodemodeSessionManager::start(
        CreateCodemodeSessionManagerOptions {
            session_id: uuid::Uuid::new_v4().to_string(),
            cwd: cwd.into(),
            settings: Default::default(),
            local_roots: None,
            artifacts_dir: Some(artifacts.into()),
            session_env: None,
            executor: Arc::new(Executor),
            list_tools: None,
            complete: Arc::new(|_| Box::pin(async { Ok(json!({})) })),
        },
    ))
    .await
    .expect("session")
}

#[tokio::test]
async fn async_and_sync_checks_classify_unavailable_paths() {
    // Given missing paths, a regular file, and a child beneath that file.
    let root = tempfile::tempdir().expect("root");
    let file = root.path().join("file");
    std::fs::write(&file, "fixture").expect("file");
    for (path, kind, reason) in [
        (
            root.path().join("missing"),
            io::ErrorKind::NotFound,
            "ENOENT",
        ),
        (
            file.clone(),
            io::ErrorKind::NotADirectory,
            "not a directory",
        ),
        (file.join("child"), io::ErrorKind::NotADirectory, "ENOTDIR"),
    ] {
        // When either acquisition check stats the path.
        let results = [
            bounded(assert_session_cwd_available(&path)).await,
            assert_session_cwd_available_sync(&path),
        ];
        // Then both preserve the classification and the actionable error's cwd.
        for result in results {
            let error = result.expect_err("unavailable cwd");
            assert_eq!(error.kind(), kind);
            let unavailable = error
                .get_ref()
                .and_then(|error| error.downcast_ref::<CodemodeSessionCwdUnavailableError>())
                .expect("cwd error");
            assert_eq!(unavailable.cwd, path);
            assert_eq!(unavailable.reason, reason);
        }
    }
}

#[tokio::test]
async fn checks_accept_existing_directories() {
    // Given an existing session directory.
    let root = tempfile::tempdir().expect("root");
    // When checking both startup and warm reuse.
    let asynchronous = bounded(assert_session_cwd_available(root.path())).await;
    let synchronous = assert_session_cwd_available_sync(root.path());
    // Then acquisition may proceed.
    asynchronous.expect("startup cwd");
    synchronous.expect("warm cwd");
}

#[tokio::test]
async fn initial_acquisition_refuses_missing_directory_before_startup() {
    // Given a session whose project directory does not exist.
    let root = tempfile::tempdir().expect("root");
    let cwd = root.path().join("missing");
    let session = session(&cwd, root.path()).await;
    // When acquiring the first kernel.
    let result = bounded(session.get_javascript_kernel()).await;
    // Then the cwd validator, not process startup, reports the failure.
    let error = result.err().expect("unavailable cwd");
    assert_eq!(
        error,
        io::Error::new(
            io::ErrorKind::NotFound,
            CodemodeSessionCwdUnavailableError {
                cwd,
                reason: "ENOENT"
            }
        )
        .to_string()
    );
    bounded(session.dispose()).await.expect("dispose");
}

#[cfg(unix)]
#[tokio::test]
async fn warm_acquisition_refuses_removed_cwd_and_retains_kernel_for_reuse() {
    // Given a live kernel whose session path is removed after startup.
    let root = tempfile::tempdir().expect("root");
    let cwd = root.path().join("project");
    std::fs::create_dir(&cwd).expect("project");
    let session = session(&cwd, root.path()).await;
    let kernel = bounded(session.get_javascript_kernel())
        .await
        .expect("kernel");
    let moved = root.path().join("moved");
    std::fs::rename(&cwd, &moved).expect("move project");
    // When acquiring the cached kernel at the missing session path.
    let result = bounded(session.get_javascript_kernel()).await;
    // Then reuse is refused without losing ownership of the live kernel.
    assert!(result.is_err());
    std::fs::rename(&moved, &cwd).expect("restore project");
    let warm = bounded(session.get_javascript_kernel())
        .await
        .expect("warm kernel");
    assert!(Arc::ptr_eq(&kernel, &warm));
    bounded(session.dispose()).await.expect("dispose");
}

#[cfg(unix)]
#[tokio::test]
async fn warm_acquisition_refuses_cwd_replaced_by_file() {
    // Given a live kernel whose project path now names a regular file.
    let root = tempfile::tempdir().expect("root");
    let cwd = root.path().join("project");
    std::fs::create_dir(&cwd).expect("project");
    let session = session(&cwd, root.path()).await;
    bounded(session.get_javascript_kernel())
        .await
        .expect("kernel");
    std::fs::rename(&cwd, root.path().join("moved")).expect("move project");
    std::fs::write(&cwd, "fixture").expect("replacement file");
    // When requesting warm reuse.
    let result = bounded(session.get_javascript_kernel()).await;
    // Then a non-directory session path is refused.
    assert!(result.is_err());
    bounded(session.dispose()).await.expect("dispose");
}
