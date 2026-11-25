use maho_ext_webfetch::webfetch::{
    browser_skill::{
        BrowserMode, ConnectedBrowser, NOT_CONNECTED_MESSAGE, PageRenderOptions, PageRenderer,
        RenderError, RenderFuture, RenderedPage, RenderedPageOutcome, WebfetchBrowserSkip,
        WebfetchBrowserSkipReason, WebfetchNotConnectedCause,
    },
    signed_in_hosts::{
        SignedInBrowserSettings, is_signed_in_host, parse_signed_in_browser_settings,
        read_signed_in_browser_settings,
    },
    tool::{WebfetchToolOptions, create_webfetch_tool_with},
};
use maho_tools::definition::{ToolCall, ToolContent};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const LOGIN_PAGE: &str = "<html><head><title>Sign in</title></head><body><h1>Sign in</h1><p>Use your account.</p></body></html>";
const SIGNED_IN_PAGE: &str = r#"<html><head><title>Team wiki</title><script>window.__CSRF__="csrf-secret-9"</script></head><body><main><h1>Roadmap</h1>
<p>Only a signed-in member can read this roadmap page, which lists the next three releases and their owners.</p></main></body></html>"#;

async fn serve_html(
    html: &'static str,
) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test server");
    let addr = listener.local_addr().expect("local addr");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let recorded = Arc::clone(&recorded);
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let Ok(n) = socket.read(&mut buf).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                let head = String::from_utf8_lossy(&buf[..n]);
                let target = head
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                recorded.lock().expect("requests").push(target);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
                    html.len()
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    });
    (
        format!("http://localhost:{}/wiki/roadmap", addr.port()),
        requests,
        handle,
    )
}

type MockRenderHandler = Box<dyn Fn(&str) -> Result<(String, String), RenderError> + Send + Sync>;

struct MockRenderer {
    calls: Mutex<Vec<(String, PageRenderOptions)>>,
    handler: MockRenderHandler,
}

impl MockRenderer {
    fn new(
        handler: impl Fn(&str) -> Result<(String, String), RenderError> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            handler: Box::new(handler),
        })
    }

    fn calls(&self) -> Vec<(String, PageRenderOptions)> {
        self.calls.lock().expect("calls").clone()
    }
}

impl PageRenderer for MockRenderer {
    fn name(&self) -> &str {
        "your connected browser (BrowserSkill)"
    }

    fn render<'a>(&'a self, url: &'a str, options: PageRenderOptions) -> RenderFuture<'a> {
        let url_owned = url.to_owned();
        Box::pin(async move {
            self.calls
                .lock()
                .expect("calls")
                .push((url_owned.clone(), options));
            let (final_url, html) = (self.handler)(&url_owned)?;
            Ok(RenderedPageOutcome {
                page: RenderedPage {
                    url: final_url,
                    html,
                    truncated: false,
                },
                browser: ConnectedBrowser {
                    instance_id: "b1".into(),
                    browser_name: "chrome".into(),
                    label: "Work Chrome".into(),
                },
            })
        })
    }
}

async fn fetch_as(
    settings: SignedInBrowserSettings,
    renderer: Arc<dyn PageRenderer>,
    url: &str,
    format: &str,
    timeout: Option<u64>,
) -> (Value, String) {
    let tool = create_webfetch_tool_with(WebfetchToolOptions {
        renderer: Some(renderer),
        signed_in_settings: Some(Arc::new(move || settings.clone())),
        browser_enabled: None,
    });
    let mut params = json!({ "url": url, "format": format });
    if let Some(t) = timeout {
        params["timeout"] = json!(t);
    }
    let result = (tool.execute)(ToolCall {
        id: "tool",
        params,
        signal: Default::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("webfetch execute");
    let text = result
        .content
        .iter()
        .filter_map(|part| match part {
            ToolContent::Text {
                text,
                audience: None,
            } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    (result.details.expect("details"), text)
}

#[test]
fn parses_and_normalizes_signed_in_browser_settings() {
    assert_eq!(
        parse_signed_in_browser_settings(&json!({})),
        SignedInBrowserSettings {
            hosts: vec![],
            browser: None
        }
    );
    assert_eq!(
        parse_signed_in_browser_settings(&json!({
            "webfetch": { "signedInBrowser": { "hosts": "a.test" } }
        })),
        SignedInBrowserSettings {
            hosts: vec![],
            browser: None
        }
    );
    assert_eq!(
        parse_signed_in_browser_settings(&json!({
            "webfetch": {
                "signedInBrowser": {
                    "hosts": [
                        " Wiki.Example.test ",
                        "*.corp.test",
                        "https://x.test/",
                        "a.test:8080",
                        "user@a.test",
                        "b.test.",
                        "*.com",
                        "wiki.例え.jp",
                        3
                    ],
                    "browser": " Work Chrome "
                }
            }
        })),
        SignedInBrowserSettings {
            hosts: vec!["wiki.example.test".into(), "*.corp.test".into()],
            browser: Some("Work Chrome".into()),
        }
    );

    for (host, kept) in [
        ("*.co.uk", false),
        ("*.com.tw", false),
        ("*.co.th", false),
        ("*.or.jp", false),
        ("*.github.io", false),
        ("*.blogspot.com", false),
        ("*.amazonaws.com", false),
        ("*.ngrok.io", false),
        ("127.0.0.1", false),
        ("2130706433", false),
        ("0x7f000001", false),
        ("*.ibm.de", true),
        ("*.corp.example.co.uk", true),
        ("*.team.github.io", true),
    ] {
        let parsed = parse_signed_in_browser_settings(&json!({
            "webfetch": { "signedInBrowser": { "hosts": [host] } }
        }));
        assert_eq!(
            parsed.hosts,
            if kept { vec![host.to_owned()] } else { vec![] },
            "host={host}"
        );
    }
}

#[test]
fn matches_signed_in_hosts_safely() {
    let hosts = vec![
        "wiki.example.test".to_owned(),
        "*.corp.test".to_owned(),
        "127.0.0.1".to_owned(),
    ];
    for (url, expected) in [
        ("https://wiki.example.test/page", true),
        ("https://wiki.example.test:8443/page", true),
        ("https://user:pass@wiki.example.test/page", true),
        ("https://evil.test/?wiki.example.test", false),
        ("https://wiki.example.test.evil.test/", false),
        ("https://xwiki.example.test/", false),
        ("https://wiki.example.test./", false),
        ("https://docs.corp.test/a", true),
        ("https://a.b.corp.test/a", true),
        ("https://corp.test/a", false),
        ("https://[::1]/", false),
        ("http://127.0.0.1/", false),
        ("http://2130706433/", false),
        ("file:///etc/hosts", false),
    ] {
        assert_eq!(is_signed_in_host(url, &hosts), expected, "url={url}");
    }
}

#[test]
fn reads_only_global_file_without_writing_or_reading_project_settings() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("settings.json");
    let body = serde_json::to_string(&json!({
        "retiredKeyFromOldRelease": true,
        "webfetch": { "signedInBrowser": { "hosts": ["wiki.example.test"] } }
    }))
    .expect("json");
    std::fs::write(&file, &body).expect("write settings");
    let before = std::fs::metadata(&file)
        .expect("meta")
        .modified()
        .expect("mtime");

    let settings = read_signed_in_browser_settings(Some(dir.path()));
    assert_eq!(
        settings,
        SignedInBrowserSettings {
            hosts: vec!["wiki.example.test".into()],
            browser: None
        }
    );
    assert_eq!(std::fs::read_to_string(&file).expect("read"), body);
    assert_eq!(
        std::fs::metadata(&file)
            .expect("meta")
            .modified()
            .expect("mtime"),
        before
    );

    // Project settings under agent_dir/repo/.senpi/settings.json are ignored.
    let agent_only = tempfile::tempdir().expect("tempdir");
    let project = agent_only.path().join("repo").join(".senpi");
    std::fs::create_dir_all(&project).expect("mkdir");
    std::fs::write(
        project.join("settings.json"),
        r#"{"webfetch":{"signedInBrowser":{"hosts":["wiki.example.test"]}}}"#,
    )
    .expect("write project");
    assert_eq!(
        read_signed_in_browser_settings(Some(agent_only.path())),
        SignedInBrowserSettings::default()
    );
}

#[tokio::test]
async fn gives_signed_in_read_only_part_of_call_timeout() {
    let (url, _reqs, server) = serve_html(LOGIN_PAGE).await;
    let renderer = MockRenderer::new(|_| {
        Err(RenderError::Skip(WebfetchBrowserSkip {
            reason: WebfetchBrowserSkipReason::IntegrationNotConnected,
            cause: None,
            message: "no browser".into(),
            detail: None,
        }))
    });

    let _ = fetch_as(
        SignedInBrowserSettings {
            hosts: vec!["localhost".into()],
            browser: None,
        },
        renderer.clone(),
        &url,
        "markdown",
        Some(10),
    )
    .await;

    let calls = renderer.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].1.timeout_ms < 10_000);
    assert!(calls[0].1.timeout_ms > 0);
    server.abort();
}

#[tokio::test]
async fn never_contacts_browser_for_unlisted_host() {
    let (url, _reqs, server) = serve_html(LOGIN_PAGE).await;
    let renderer = MockRenderer::new(|u| Ok((u.to_owned(), SIGNED_IN_PAGE.to_owned())));

    let (details, text) = fetch_as(
        SignedInBrowserSettings::default(),
        renderer.clone(),
        &url,
        "markdown",
        None,
    )
    .await;

    assert!(renderer.calls().is_empty());
    assert!(text.contains("Use your account."));
    assert_eq!(details["route"], "http");
    server.abort();
}

#[tokio::test]
async fn reads_listed_host_in_signed_in_session_with_pinned_browser_and_strips_scripts() {
    let (url, reqs, server) = serve_html(LOGIN_PAGE).await;
    let renderer = MockRenderer::new(|u| Ok((u.to_owned(), SIGNED_IN_PAGE.to_owned())));
    let settings = SignedInBrowserSettings {
        hosts: vec!["localhost".into()],
        browser: Some("Work Chrome".into()),
    };

    let (details, text) =
        fetch_as(settings.clone(), renderer.clone(), &url, "markdown", None).await;
    let calls = renderer.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, url);
    assert_eq!(calls[0].1.mode, BrowserMode::SignedIn);
    assert_eq!(calls[0].1.browser.as_deref(), Some("Work Chrome"));
    assert!(reqs.lock().expect("reqs").is_empty());
    assert!(text.contains("Only a signed-in member can read this roadmap page"));
    assert!(!text.contains("csrf-secret-9"));
    assert_eq!(details["route"], "signed-in-browser");
    assert_eq!(details["finalUrl"], url);
    assert_eq!(details["converted"], true);
    assert_eq!(details["rendered"], true);
    assert_eq!(
        details["browser"],
        json!({ "instanceId": "b1", "browserName": "chrome", "label": "Work Chrome" })
    );

    let (_html_details, html_text) = fetch_as(settings, renderer, &url, "html", None).await;
    assert!(!html_text.contains("csrf-secret-9"));
    assert!(html_text.contains("Roadmap"));
    server.abort();
}

#[tokio::test]
async fn falls_back_to_anonymous_read_with_typed_not_connected_reason() {
    let (url, reqs, server) = serve_html(LOGIN_PAGE).await;
    let renderer = MockRenderer::new(|_| {
        Err(RenderError::Skip(WebfetchBrowserSkip {
            reason: WebfetchBrowserSkipReason::IntegrationNotConnected,
            cause: Some(WebfetchNotConnectedCause::NoDaemon),
            message: NOT_CONNECTED_MESSAGE.into(),
            detail: Some("no readable daemon.json".into()),
        }))
    });

    let (details, text) = fetch_as(
        SignedInBrowserSettings {
            hosts: vec!["localhost".into()],
            browser: None,
        },
        renderer,
        &url,
        "markdown",
        None,
    )
    .await;

    assert_eq!(*reqs.lock().expect("reqs"), vec!["/wiki/roadmap"]);
    assert!(text.contains("Use your account."));
    assert_eq!(details["route"], "http");
    assert_eq!(
        details["signedInSkipped"],
        json!({
            "reason": "integration_not_connected",
            "cause": "no_daemon",
            "message": "browser integration not connected; render skipped",
            "detail": "no readable daemon.json"
        })
    );
    server.abort();
}

#[tokio::test]
async fn discards_signed_in_read_that_ended_on_unlisted_host() {
    let (url, reqs, server) = serve_html(LOGIN_PAGE).await;
    let renderer = MockRenderer::new(|_| {
        Ok((
            "https://mail.other.test/inbox".into(),
            SIGNED_IN_PAGE.to_owned(),
        ))
    });

    let (details, text) = fetch_as(
        SignedInBrowserSettings {
            hosts: vec!["localhost".into()],
            browser: None,
        },
        renderer,
        &url,
        "markdown",
        None,
    )
    .await;

    assert!(!text.contains("roadmap page"));
    assert_eq!(*reqs.lock().expect("reqs"), vec!["/wiki/roadmap"]);
    assert_eq!(details["route"], "http");
    assert_eq!(details["signedInSkipped"]["reason"], "host_not_allowed");
    assert!(
        details["signedInSkipped"]["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("mail.other.test")
    );
    server.abort();
}
