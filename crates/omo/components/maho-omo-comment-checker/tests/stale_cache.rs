#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use comment_checker_core::{
    COMMENT_CHECKER_RELEASE_VERSION, COMMENT_CHECKER_VERSION_MARKER,
};
use maho_ext_api::ComponentLogger;
use maho_omo_comment_checker::downloader::download_senpi_comment_checker_binary;
use maho_omo_comment_checker::resolver::resolve_with_options_cache;
use maho_omo_comment_checker::types::SenpiCommentCheckerDownloadOptions;
use tempfile::tempdir;

const FRESH_BINARY: &str = "#!/bin/sh
echo pinned-release
";
const STALE_BINARY: &str = "#!/bin/sh
echo v0.6.1-left-by-an-older-install
";

#[derive(Default)]
struct TestLogger;
impl ComponentLogger for TestLogger {
    fn info(&self, _: &str, _: Option<&serde_json::Value>) {}
    fn warn(&self, _: &str, _: Option<&serde_json::Value>) {}
    fn error(&self, _: &str, _: Option<&serde_json::Value>) {}
}

async fn serve_release() -> (String, Arc<Mutex<usize>>, tokio::sync::oneshot::Sender<()>) {
    let stage = tempdir().expect("stage");
    let script = stage.path().join("comment-checker");
    fs::write(&script, FRESH_BINARY).expect("write script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");

    let archive = stage.path().join("release.tar.gz");
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
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    );
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(&bytes).await;
                    let _ = socket.shutdown().await;
                }
            }
        }
    });

    (format!("http://127.0.0.1:{port}/release.tar.gz"), requests, tx)
}

fn cache_with_binary(root: &Path, marker: Option<&str>) -> PathBuf {
    let cache_dir = root.join("bin");
    fs::create_dir_all(&cache_dir).expect("test assertion");
    let bin = cache_dir.join("comment-checker");
    fs::write(&bin, STALE_BINARY).expect("test assertion");
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).expect("test assertion");
    if let Some(m) = marker {
        fs::write(
            cache_dir.join(COMMENT_CHECKER_VERSION_MARKER),
            format!("{m}
"),
        )
        .expect("test assertion");
    }
    cache_dir
}

fn resolve_from_cache_only(cache_dir: &Path) -> Option<PathBuf> {
    let env = BTreeMap::new();
    resolve_with_options_cache(
        &env,
        |p| p.exists(),
        || None,
        |_| None,
        "linux",
        Some(cache_dir),
    )
}

#[tokio::test]
async fn given_cached_checker_with_no_version_marker_when_resolving_then_cache_not_trusted() {
    let temp = tempdir().expect("test assertion");
    let cache_dir = cache_with_binary(temp.path(), None);
    assert_eq!(resolve_from_cache_only(&cache_dir), None);
}

#[tokio::test]
async fn given_cached_checker_with_older_marker_when_resolving_then_cache_not_trusted() {
    let temp = tempdir().expect("test assertion");
    let cache_dir = cache_with_binary(temp.path(), Some("0.6.1"));
    assert_eq!(resolve_from_cache_only(&cache_dir), None);
}

#[tokio::test]
async fn given_cached_checker_with_no_version_marker_when_downloading_then_pinned_replaces_and_records()
{
    let temp = tempdir().expect("test assertion");
    let cache_dir = cache_with_binary(temp.path(), None);
    let (url, requests, _shutdown) = serve_release().await;

    let binary = download_senpi_comment_checker_binary(&SenpiCommentCheckerDownloadOptions {
        logger: Some(Arc::new(TestLogger)),
        platform: Some("linux".to_string()),
        arch: Some("x64".to_string()),
        cache_dir: Some(cache_dir.clone()),
        resolve_asset_url: Some(Arc::new(move |_| url.clone())),
    })
    .await;

    assert_eq!(binary, Some(cache_dir.join("comment-checker")));
    assert_eq!(*requests.lock().expect("test assertion"), 1);
    assert_eq!(
        fs::read_to_string(cache_dir.join("comment-checker")).expect("test assertion"),
        FRESH_BINARY
    );
    assert_eq!(
        fs::read_to_string(cache_dir.join(COMMENT_CHECKER_VERSION_MARKER))
            .expect("test assertion")
            .trim(),
        COMMENT_CHECKER_RELEASE_VERSION
    );
    assert_eq!(
        resolve_from_cache_only(&cache_dir),
        Some(cache_dir.join("comment-checker"))
    );
}

#[tokio::test]
async fn given_cached_checker_recorded_at_pinned_release_when_resolving_then_cache_answers() {
    let temp = tempdir().expect("test assertion");
    let cache_dir = cache_with_binary(temp.path(), Some(COMMENT_CHECKER_RELEASE_VERSION));
    assert_eq!(
        resolve_from_cache_only(&cache_dir),
        Some(cache_dir.join("comment-checker"))
    );
}
