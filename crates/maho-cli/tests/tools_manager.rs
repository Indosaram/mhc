use maho_cli::utils::tools_manager::*;
#[test]
fn release_redirect_parses_relative_and_absolute_tags() {
    assert_eq!(
        release_version_from_redirect("sharkdp/fd", 302, Some("/sharkdp/fd/releases/tag/v10.4.2"))
            .unwrap(),
        "10.4.2"
    );
    assert_eq!(
        release_version_from_redirect(
            "BurntSushi/ripgrep",
            302,
            Some("https://github.com/BurntSushi/ripgrep/releases/tag/15.2.0")
        )
        .unwrap(),
        "15.2.0"
    );
    assert!(release_version_from_redirect("sharkdp/fd", 404, None).is_err());
    assert!(
        release_version_from_redirect("sharkdp/fd", 302, Some("https://github.com/login")).is_err()
    );
}
#[test]
fn local_tool_wins_and_assets_follow_platform_architecture() {
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("fd");
    std::fs::write(&binary, b"").unwrap();
    assert_eq!(
        get_tool_path_in(Tool::Fd, directory.path(), &Default::default(), "linux"),
        Some(binary.to_string_lossy().into_owned())
    );
    assert_eq!(
        asset_name(Tool::Fd, "10.3.0", "darwin", "arm64").as_deref(),
        Some("fd-v10.3.0-aarch64-apple-darwin.tar.gz")
    );
    assert!(asset_name(Tool::Rg, "15", "android", "arm64").is_none());
}
#[tokio::test]
async fn archive_installs_nested_binary_and_removes_owned_artifacts() {
    let directory = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    std::fs::create_dir(source.path().join("nested")).unwrap();
    std::fs::write(source.path().join("nested/fd"), b"binary").unwrap();
    let archive = directory.path().join("fd-test.tar.gz");
    assert!(
        std::process::Command::new("tar")
            .args(["czf"])
            .arg(&archive)
            .arg("-C")
            .arg(source.path())
            .arg("nested")
            .status()
            .unwrap()
            .success()
    );
    let binary = install_archive(&archive, directory.path(), "fd", "fd-test.tar.gz", "linux")
        .await
        .unwrap();
    assert_eq!(std::fs::read(binary).unwrap(), b"binary");
    assert!(!archive.exists());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}
#[tokio::test]
async fn offline_tool_lookup_emits_warning_without_network() {
    let directory = tempfile::tempdir().unwrap();
    let env = std::collections::BTreeMap::from([("MAHO_OFFLINE".to_owned(), "yes".to_owned())]);
    let mut statuses = Vec::new();
    assert!(
        ensure_tool_in(Tool::Fd, directory.path(), &env, "linux", "x64", |status| {
            statuses.push(status)
        })
        .await
        .is_none()
    );
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].kind, "warning");
}

// senpi `test/tools-manager.test.ts` (pin 903b8336), one case per upstream `it`.
#[test]
fn get_latest_version_resolves_the_version_from_the_release_page_redirect() {
    assert_eq!(
        release_version_from_redirect(
            "sharkdp/fd",
            302,
            Some("https://github.com/sharkdp/fd/releases/tag/v10.4.2")
        )
        .unwrap(),
        "10.4.2"
    );
}
#[test]
fn get_latest_version_keeps_tags_without_a_v_prefix_intact() {
    assert_eq!(
        release_version_from_redirect(
            "BurntSushi/ripgrep",
            302,
            Some("https://github.com/BurntSushi/ripgrep/releases/tag/15.2.0")
        )
        .unwrap(),
        "15.2.0"
    );
}
#[test]
fn get_latest_version_resolves_relative_redirect_targets() {
    assert_eq!(
        release_version_from_redirect("sharkdp/fd", 302, Some("/sharkdp/fd/releases/tag/v10.4.2"))
            .unwrap(),
        "10.4.2"
    );
}
#[test]
fn get_latest_version_fails_clearly_when_the_endpoint_does_not_redirect() {
    assert_eq!(
        release_version_from_redirect("sharkdp/fd", 404, None).unwrap_err(),
        "Failed to resolve latest sharkdp/fd release: HTTP 404 without redirect"
    );
}
#[test]
fn get_latest_version_fails_clearly_when_the_redirect_does_not_point_at_a_release_tag() {
    assert_eq!(
        release_version_from_redirect("sharkdp/fd", 302, Some("https://github.com/login"))
            .unwrap_err(),
        "Failed to resolve latest sharkdp/fd release: unexpected redirect to https://github.com/login"
    );
}
#[tokio::test]
async fn ensure_tool_reports_status_through_a_callback_without_writing_to_the_console() {
    let directory = tempfile::tempdir().unwrap();
    let env = std::collections::BTreeMap::from([
        ("PATH".to_owned(), String::new()),
        ("PI_OFFLINE".to_owned(), "1".to_owned()),
    ]);
    let mut statuses = Vec::new();
    assert!(
        ensure_tool_in(Tool::Fd, directory.path(), &env, "linux", "x64", |status| {
            statuses.push((status.kind, status.message))
        })
        .await
        .is_none()
    );
    assert_eq!(
        statuses,
        [(
            "warning",
            "fd not found. Offline mode enabled, skipping download.".to_owned()
        )]
    );
}

struct FakeFetch<F>(F);
impl<F: Fn(ToolFetchRequest) -> Result<ToolFetchResponse, ToolFetchError> + Send + Sync> ToolFetch
    for FakeFetch<F>
{
    fn fetch(
        &self,
        request: ToolFetchRequest,
    ) -> futures::future::BoxFuture<'_, Result<ToolFetchResponse, ToolFetchError>> {
        let result = (self.0)(request);
        Box::pin(async move { result })
    }
}

struct DropFlag(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tokio::test]
async fn get_latest_version_discards_the_redirect_response_body() {
    use futures::StreamExt;
    let discarded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let requests = std::sync::Mutex::new(Vec::new());
    let fetch = FakeFetch(
        |request: ToolFetchRequest| -> Result<ToolFetchResponse, ToolFetchError> {
            requests
                .lock()
                .unwrap()
                .push((request.url, request.follow_redirects));
            let flag = DropFlag(discarded.clone());
            let body = futures::stream::iter([Ok(b"<html></html>".to_vec())])
                .map(move |chunk| {
                    let _flag = &flag;
                    chunk
                })
                .boxed();
            Ok(ToolFetchResponse {
                status: 302,
                location: Some("https://github.com/sharkdp/fd/releases/tag/v10.4.2".to_owned()),
                body,
            })
        },
    );
    assert_eq!(
        get_latest_version_with(&fetch, "sharkdp/fd").await.unwrap(),
        "10.4.2"
    );
    assert!(discarded.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        *requests.lock().unwrap(),
        [(
            "https://github.com/sharkdp/fd/releases/latest".to_owned(),
            false
        )]
    );
}

#[derive(Debug)]
struct FetchFailed(std::io::Error);
impl std::fmt::Display for FetchFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("fetch failed")
    }
}
impl std::error::Error for FetchFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

#[tokio::test]
async fn ensure_tool_surfaces_the_error_cause_chain_when_a_download_fails() {
    let directory = tempfile::tempdir().unwrap();
    let env = std::collections::BTreeMap::from([("PATH".to_owned(), String::new())]);
    let fetch = FakeFetch(
        |_: ToolFetchRequest| -> Result<ToolFetchResponse, ToolFetchError> {
            Err(Box::new(FetchFailed(std::io::Error::other(
                "connect ETIMEDOUT 140.82.113.3:443",
            ))) as ToolFetchError)
        },
    );
    let mut statuses = Vec::new();
    assert!(
        ensure_tool_with(
            &fetch,
            Tool::Fd,
            directory.path(),
            &env,
            "linux",
            "x64",
            |status| statuses.push((status.kind, status.message))
        )
        .await
        .is_none()
    );
    assert_eq!(
        statuses,
        [
            ("info", "fd not found. Downloading...".to_owned()),
            (
                "warning",
                "Failed to download fd: fetch failed: connect ETIMEDOUT 140.82.113.3:443"
                    .to_owned()
            ),
        ]
    );
}
