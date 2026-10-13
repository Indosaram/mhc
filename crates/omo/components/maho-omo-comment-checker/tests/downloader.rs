#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use comment_checker_core::{
    COMMENT_CHECKER_RELEASE_VERSION, COMMENT_CHECKER_VERSION_MARKER,
};
use maho_ext_api::ComponentLogger;
use maho_omo_comment_checker::constants::COMMENT_CHECKER_CACHE_DIR_NAME;
use maho_omo_comment_checker::downloader::download_senpi_comment_checker_binary;
use maho_omo_comment_checker::types::SenpiCommentCheckerDownloadOptions;
use tempfile::tempdir;

#[derive(Default)]
struct RecordingLogger {
    entries: Mutex<Vec<(String, String)>>,
}

impl ComponentLogger for RecordingLogger {
    fn info(&self, message: &str, _details: Option<&serde_json::Value>) {
        self.entries
            .lock()
            .expect("test assertion")
            .push(("info".to_string(), message.to_string()));
    }
    fn warn(&self, message: &str, _details: Option<&serde_json::Value>) {
        self.entries
            .lock()
            .expect("test assertion")
            .push(("warn".to_string(), message.to_string()));
    }
    fn error(&self, message: &str, _details: Option<&serde_json::Value>) {
        self.entries
            .lock()
            .expect("test assertion")
            .push(("error".to_string(), message.to_string()));
    }
}

async fn serve_archive(
    status: u16,
) -> (
    String,
    Arc<Mutex<usize>>,
    tokio::sync::oneshot::Sender<()>,
) {
    let stage = tempdir().expect("stage");
    let script = stage.path().join("comment-checker");
    fs::write(&script, "#!/bin/sh
echo fixture-checker
").expect("write script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");

    let archive = stage.path().join("fixture.tar.gz");
    let status_tar = std::process::Command::new("tar")
        .args(["-czf"])
        .arg(&archive)
        .arg("-C")
        .arg(stage.path())
        .arg("comment-checker")
        .status()
        .expect("tar");
    assert!(status_tar.success());

    let bytes = fs::read(&archive).expect("read archive");
    let requests = Arc::new(Mutex::new(0));
    let requests_clone = requests.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut rx => break,
                res = listener.accept() => {
                    let Ok((mut socket, _)) = res else { break };
                    *requests_clone.lock().expect("test assertion") += 1;
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    if status == 200 {
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            bytes.len()
                        );
                        let _ = socket.write_all(header.as_bytes()).await;
                        let _ = socket.write_all(&bytes).await;
                    } else {
                        let header = format!("HTTP/1.1 {status} Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        let _ = socket.write_all(header.as_bytes()).await;
                    }
                    let _ = socket.shutdown().await;
                }
            }
        }
    });

    (format!("http://127.0.0.1:{port}/asset.tar.gz"), requests, tx)
}

#[test]
fn downloader_cache_dir_name_is_oh_my_opencode() {
    assert_eq!(COMMENT_CHECKER_CACHE_DIR_NAME, "oh-my-opencode");
}

#[tokio::test]
async fn given_empty_cache_and_reachable_release_when_downloading_then_binary_lands_executable_and_archive_removed()
{
    let temp = tempdir().expect("temp");
    let cache_dir = temp.path().join("bin");
    let logger = Arc::new(RecordingLogger::default());
    let (url, requests, _shutdown) = serve_archive(200).await;

    let binary_path = download_senpi_comment_checker_binary(&SenpiCommentCheckerDownloadOptions {
        logger: Some(logger.clone()),
        platform: Some("linux".to_string()),
        arch: Some("x64".to_string()),
        cache_dir: Some(cache_dir.clone()),
        resolve_asset_url: Some(Arc::new(move |_| url.clone())),
    })
    .await;

    assert_eq!(binary_path, Some(cache_dir.join("comment-checker")));
    assert!(cache_dir.join("comment-checker").exists());
    let perms = fs::metadata(cache_dir.join("comment-checker"))
        .expect("test assertion")
        .permissions();
    assert_ne!(perms.mode() & 0o111, 0);

    let mut entries: Vec<String> = fs::read_dir(&cache_dir)
        .expect("test assertion")
        .map(|e| e.expect("test assertion").file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        vec![
            "comment-checker".to_string(),
            COMMENT_CHECKER_VERSION_MARKER.to_string()
        ]
    );
    assert_eq!(*requests.lock().expect("test assertion"), 1);
    let levels: Vec<String> = logger
        .entries
        .lock()
        .expect("test assertion")
        .iter()
        .map(|(l, _)| l.clone())
        .collect();
    assert_eq!(levels, vec!["info".to_string()]);
}

#[tokio::test]
async fn given_cached_binary_recorded_at_pinned_release_when_downloading_again_then_cache_answers_without_request()
{
    let temp = tempdir().expect("temp");
    let cache_dir = temp.path().join("bin");
    fs::create_dir_all(&cache_dir).expect("test assertion");
    let bin = cache_dir.join("comment-checker");
    fs::write(&bin, "#!/bin/sh
").expect("test assertion");
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).expect("test assertion");
    fs::write(
        cache_dir.join(COMMENT_CHECKER_VERSION_MARKER),
        format!("{COMMENT_CHECKER_RELEASE_VERSION}
"),
    )
    .expect("test assertion");

    let logger = Arc::new(RecordingLogger::default());
    let (url, requests, _shutdown) = serve_archive(200).await;

    let binary_path = download_senpi_comment_checker_binary(&SenpiCommentCheckerDownloadOptions {
        logger: Some(logger.clone()),
        platform: Some("linux".to_string()),
        arch: Some("x64".to_string()),
        cache_dir: Some(cache_dir.clone()),
        resolve_asset_url: Some(Arc::new(move |_| url.clone())),
    })
    .await;

    assert_eq!(binary_path, Some(bin));
    assert_eq!(*requests.lock().expect("test assertion"), 0);
    assert!(logger.entries.lock().expect("test assertion").is_empty());
}

#[tokio::test]
async fn given_release_endpoint_fails_when_downloading_then_returns_none_with_warning_and_no_partial_files()
{
    let temp = tempdir().expect("temp");
    let cache_dir = temp.path().join("bin");
    let logger = Arc::new(RecordingLogger::default());
    let (url, _requests, _shutdown) = serve_archive(503).await;

    let binary_path = download_senpi_comment_checker_binary(&SenpiCommentCheckerDownloadOptions {
        logger: Some(logger.clone()),
        platform: Some("linux".to_string()),
        arch: Some("x64".to_string()),
        cache_dir: Some(cache_dir.clone()),
        resolve_asset_url: Some(Arc::new(move |_| url.clone())),
    })
    .await;

    assert!(binary_path.is_none());
    let entries = logger.entries.lock().expect("test assertion").clone();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0, "info");
    assert_eq!(entries[1].0, "warn");
    assert!(entries[1].1.contains("download failed"));

    let remaining: Vec<_> = if cache_dir.exists() {
        fs::read_dir(&cache_dir).expect("test assertion").collect()
    } else {
        vec![]
    };
    assert!(remaining.is_empty());
}

#[tokio::test]
async fn given_unsupported_platform_when_downloading_then_returns_none_without_contacting_network() {
    let temp = tempdir().expect("temp");
    let cache_dir = temp.path().join("bin");
    let logger = Arc::new(RecordingLogger::default());
    let (url, requests, _shutdown) = serve_archive(200).await;

    let binary_path = download_senpi_comment_checker_binary(&SenpiCommentCheckerDownloadOptions {
        logger: Some(logger.clone()),
        platform: Some("freebsd".to_string()),
        arch: Some("x64".to_string()),
        cache_dir: Some(cache_dir),
        resolve_asset_url: Some(Arc::new(move |_| url.clone())),
    })
    .await;

    assert!(binary_path.is_none());
    assert_eq!(*requests.lock().expect("test assertion"), 0);
    let entries = logger.entries.lock().expect("test assertion").clone();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, "warn");
}
