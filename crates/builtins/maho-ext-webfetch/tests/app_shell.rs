use maho_ext_webfetch::webfetch::content::looks_like_app_shell;

const APP_SHELL: &str = r#"<!doctype html><html><head><title>Board</title>
<script type="module" src="/assets/index.js"></script></head>
<body><noscript>You need to enable JavaScript to run this app.</noscript><div id="root"></div></body></html>"#;

const RENDERED: &str = r#"<!doctype html><html><head><title>Board</title><script>window.__TOKEN__="secret-token-123"</script></head><body><div id="root">
<main><h1>Team board</h1><p>Three cards are waiting for review, and the release column is empty this week.</p>
<p>Drag a card to the done column once its pull request has merged and the release notes are written.</p></main>
</div></body></html>"#;

#[test]
fn detects_app_shells_accurately() {
    let article = format!(
        r#"<!doctype html><html><head><title>Notes</title><script src="/analytics.js"></script></head>
<body><article><h1>Release notes</h1><p>{}</p></article></body></html>"#,
        "This release fixes the reader so long articles keep every paragraph. ".repeat(6)
    );
    for (name, html, expected) in [
        ("empty mount point with a module script", APP_SHELL, true),
        (
            "empty body with an inline bootstrap script",
            "<html><body><script>start()</script></body></html>",
            true,
        ),
        (
            "article page that also loads a script",
            article.as_str(),
            false,
        ),
        (
            "short static page with no script",
            "<html><body><h1>Example Domain</h1><p>For examples.</p></body></html>",
            false,
        ),
        (
            "only a JSON-LD script",
            r#"<html><body><script type="application/ld+json">{}</script></body></html>"#,
            false,
        ),
    ] {
        assert_eq!(
            looks_like_app_shell(html, "https://example.test/"),
            expected,
            "{name}"
        );
    }
}

#[cfg(unix)]
mod unix_daemon_tests {
    use super::*;
    use maho_ext_webfetch::webfetch::{
        browser_skill::{
            BrowserMode, BrowserSkillOptions, NOT_CONNECTED_MESSAGE, PageRenderOptions,
            PageRenderer, RenderError, WebfetchBrowserSkipReason, WebfetchNotConnectedCause,
            create_browser_skill_renderer,
        },
        signed_in_hosts::SignedInBrowserSettings,
        tool::{WebfetchToolOptions, create_webfetch_tool_with},
    };
    use maho_tools::definition::{AbortSignal, ToolCall, ToolContent};
    use serde_json::{Value, json};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{TcpListener, UnixListener};

    const FIVE_MIB: usize = 5 * 1024 * 1024;

    #[derive(Clone, Debug)]
    struct DaemonCall {
        method: String,
        params: Value,
    }

    enum Reply {
        Json(Value),
        Never,
    }

    type HandlerFuture = Pin<Box<dyn Future<Output = Reply> + Send>>;
    type DaemonHandler = Arc<dyn Fn(DaemonCall) -> HandlerFuture + Send + Sync>;

    struct FakeDaemon {
        _dir: tempfile::TempDir,
        home: std::path::PathBuf,
        calls: Arc<Mutex<Vec<DaemonCall>>>,
        connections: Arc<AtomicUsize>,
        live_sessions: Arc<AtomicUsize>,
        max_live_sessions: Arc<AtomicUsize>,
        handle: tokio::task::JoinHandle<()>,
    }

    impl Drop for FakeDaemon {
        fn drop(&mut self) {
            self.handle.abort();
        }
    }

    impl FakeDaemon {
        async fn start(handler: DaemonHandler) -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let home = dir.path().to_path_buf();
            let sock_path = home.join("d.sock");
            let listener = UnixListener::bind(&sock_path).expect("bind unix socket");
            std::fs::write(
                home.join("daemon.json"),
                json!({ "sock_path": sock_path }).to_string(),
            )
            .expect("write daemon.json");

            let calls = Arc::new(Mutex::new(Vec::new()));
            let connections = Arc::new(AtomicUsize::new(0));
            let live_sessions = Arc::new(AtomicUsize::new(0));
            let max_live_sessions = Arc::new(AtomicUsize::new(0));

            let calls_bg = Arc::clone(&calls);
            let conns_bg = Arc::clone(&connections);
            let live_bg = Arc::clone(&live_sessions);
            let max_bg = Arc::clone(&max_live_sessions);

            let handle = tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        return;
                    };
                    conns_bg.fetch_add(1, Ordering::SeqCst);
                    let handler = Arc::clone(&handler);
                    let calls = Arc::clone(&calls_bg);
                    let live = Arc::clone(&live_bg);
                    let max = Arc::clone(&max_bg);
                    tokio::spawn(async move {
                        let (read_half, mut write_half) = stream.into_split();
                        let mut reader = BufReader::new(read_half);
                        let mut line = String::new();
                        let Ok(n) = reader.read_line(&mut line).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        let Ok(frame) = serde_json::from_str::<Value>(line.trim_end()) else {
                            return;
                        };
                        let id = frame["id"].as_str().unwrap_or_default().to_owned();
                        let method = frame["method"].as_str().unwrap_or_default().to_owned();
                        let params = frame["params"].clone();
                        let call = DaemonCall {
                            method: method.clone(),
                            params,
                        };
                        calls.lock().expect("calls").push(call.clone());
                        if method == "session.start" {
                            let cur = live.fetch_add(1, Ordering::SeqCst) + 1;
                            max.fetch_max(cur, Ordering::SeqCst);
                        }
                        if method == "session.stop" {
                            live.fetch_sub(1, Ordering::SeqCst);
                        }
                        match handler(call).await {
                            Reply::Never => {
                                // Keep socket open until peer disconnects.
                                let mut sink = [0u8; 64];
                                while matches!(reader.read(&mut sink).await, Ok(m) if m > 0) {}
                            }
                            Reply::Json(mut reply) => {
                                if reply.get("id").is_none() {
                                    reply["id"] = json!(id);
                                }
                                let _ = write_half.write_all(format!("{reply}\n").as_bytes()).await;
                            }
                        }
                    });
                }
            });

            Self {
                _dir: dir,
                home,
                calls,
                connections,
                live_sessions,
                max_live_sessions,
                handle,
            }
        }

        fn calls(&self) -> Vec<DaemonCall> {
            self.calls.lock().expect("calls").clone()
        }
    }

    fn browser_json() -> Value {
        json!({
            "instance_id": "b1",
            "browser_name": "chrome",
            "label": "Home Chrome",
            "unresponsive": false,
            "connected_at_ms": 100,
            "extension_protocol_version": "1.4"
        })
    }

    fn status_reply(browsers: Vec<Value>, protocol: &str) -> Value {
        json!({
            "result": {
                "daemon_version": "0.4.0",
                "protocol_version": protocol,
                "browsers": browsers,
                "sessions": []
            }
        })
    }

    fn connected_browser(
        page_url: String,
        overrides: impl Fn(&DaemonCall) -> Option<HandlerFuture> + Send + Sync + 'static,
    ) -> DaemonHandler {
        let overrides = Arc::new(overrides);
        Arc::new(move |call| {
            if let Some(fut) = overrides(&call) {
                return fut;
            }
            let page_url = page_url.clone();
            Box::pin(async move {
                match call.method.as_str() {
                    "system.status" => Reply::Json(status_reply(vec![browser_json()], "1.4")),
                    "session.start" => Reply::Json(json!({ "result": { "session_id": "s1" } })),
                    "tool.navigate" => Reply::Json(json!({
                        "result": { "tab_id": 1, "url": page_url, "final_url": page_url, "reached": "networkidle" }
                    })),
                    "tool.get_html" => Reply::Json(json!({
                        "result": { "html": RENDERED, "truncated": false, "byte_size": RENDERED.len(), "tab_id": 1 }
                    })),
                    "tool.tab_list" => Reply::Json(json!({
                        "result": { "tabs": [{ "tab_id": 1, "url": page_url, "scope": "agent" }] }
                    })),
                    _ => Reply::Json(json!({ "result": {} })),
                }
            })
        })
    }

    fn default_renderer(home: &std::path::Path) -> Arc<dyn PageRenderer> {
        create_browser_skill_renderer(BrowserSkillOptions {
            home: Some(home.to_path_buf()),
            settle_poll_ms: Some(0),
            status_timeout_ms: Some(200),
            ..Default::default()
        })
    }

    async fn serve_html(html: String) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind tcp");
        let addr = listener.local_addr().expect("addr");
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let html = html.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let Ok(n) = socket.read(&mut buf).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
                        html.len()
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                });
            }
        });
        (format!("http://127.0.0.1:{}/app", addr.port()), handle)
    }

    async fn fetch_with(
        tool: &maho_tools::definition::ToolDefinition,
        url: &str,
        format: &str,
        signal: Option<AbortSignal>,
        timeout: Option<u64>,
    ) -> Result<(Value, String, Vec<String>), maho_tools::definition::ToolError> {
        let result = (tool.execute)(ToolCall {
            id: "tool",
            params: json!({
                "url": url,
                "format": format,
                "timeout": timeout.unwrap_or(5),
            }),
            signal: signal.unwrap_or_default(),
            on_update: None,
            context: None,
        })
        .await?;
        let details = result.details.expect("details");
        let user_text = result
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
        let model_notes = result
            .content
            .iter()
            .filter_map(|part| match part {
                ToolContent::Text {
                    text,
                    audience: Some(_),
                } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        Ok((details, user_text, model_notes))
    }

    #[tokio::test]
    async fn renders_in_incognito_session_with_exact_verb_set_and_strips_scripts() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let daemon = FakeDaemon::start(connected_browser(url.clone(), |_| None)).await;
        let tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(&daemon.home)),
            ..Default::default()
        });

        let (details, user_text, model_notes) = fetch_with(&tool, &url, "markdown", None, None)
            .await
            .expect("fetch");
        assert!(user_text.contains("Three cards are waiting for review"));
        assert!(!user_text.contains("secret-token-123"));
        assert_eq!(details["rendered"], true);
        assert_eq!(details["browser"]["instanceId"], "b1");
        assert_eq!(details["browser"]["label"], "Home Chrome");
        assert!(
            model_notes
                .join("\n")
                .contains("incognito window without your cookies")
        );

        let calls = daemon.calls();
        let methods: Vec<&str> = calls.iter().map(|c| c.method.as_str()).collect();
        assert_eq!(
            methods,
            [
                "system.status",
                "session.start",
                "tool.navigate",
                "tool.tab_list",
                "tool.get_html",
                "tool.tab_list",
                "tool.tab_list",
                "tool.get_html",
                "tool.tab_list",
                "session.stop",
            ]
        );
        let start_params = &calls[1].params;
        assert_eq!(start_params["task_name"], "webfetch");
        assert!(
            start_params["controller"]["name"]
                .as_str()
                .unwrap_or_default()
                .starts_with("senpi webfetch ")
        );
        assert_eq!(start_params["browser_instance_id"], "b1");
        assert_eq!(start_params["incognito"], true);
        assert_eq!(start_params["focused"], false);
        assert_eq!(
            calls[3].params,
            json!({ "session_id": "s1", "scope": "agent" })
        );
        assert_eq!(
            calls[4].params,
            json!({ "session_id": "s1", "max_bytes": FIVE_MIB })
        );
        server.abort();
    }

    #[tokio::test]
    async fn reports_exact_not_connected_message_when_no_daemon_json_exists() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let empty = tempfile::tempdir().expect("tempdir");
        let tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(empty.path())),
            ..Default::default()
        });

        let (details, user_text, _) = fetch_with(&tool, &url, "markdown", None, None)
            .await
            .expect("fetch");
        assert_eq!(user_text.trim(), "");
        assert_eq!(details["rendered"], false);
        assert_eq!(
            details["renderSkipped"]["reason"],
            "integration_not_connected"
        );
        assert_eq!(details["renderSkipped"]["cause"], "no_daemon");
        assert_eq!(
            details["renderSkipped"]["message"],
            "browser integration not connected; render skipped"
        );
        assert_eq!(
            NOT_CONNECTED_MESSAGE,
            "browser integration not connected; render skipped"
        );
        server.abort();
    }

    #[tokio::test]
    async fn refuses_invalid_daemon_json_socket_paths_as_no_daemon() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        for (kind, expected_detail) in [("regular", "not a socket"), ("relative", "not absolute")] {
            let home = tempfile::tempdir().expect("tempdir");
            let sock_path = if kind == "regular" {
                let p = home.path().join("plain");
                std::fs::write(&p, "x").expect("write plain");
                p.to_string_lossy().into_owned()
            } else {
                "relative.sock".to_owned()
            };
            std::fs::write(
                home.path().join("daemon.json"),
                json!({ "sock_path": sock_path }).to_string(),
            )
            .expect("write daemon.json");
            let tool = create_webfetch_tool_with(WebfetchToolOptions {
                renderer: Some(default_renderer(home.path())),
                ..Default::default()
            });
            let (details, _, _) = fetch_with(&tool, &url, "markdown", None, None)
                .await
                .expect("fetch");
            assert_eq!(
                details["renderSkipped"]["reason"],
                "integration_not_connected"
            );
            assert_eq!(details["renderSkipped"]["cause"], "no_daemon");
            assert!(
                details["renderSkipped"]["detail"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(expected_detail)
            );
        }
        server.abort();
    }

    #[tokio::test]
    async fn refuses_socket_owned_by_another_user_without_connecting() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let daemon = FakeDaemon::start(connected_browser(url.clone(), |_| None)).await;
        let renderer = create_browser_skill_renderer(BrowserSkillOptions {
            home: Some(daemon.home.clone()),
            settle_poll_ms: Some(0),
            status_timeout_ms: Some(200),
            owner_uid: Some(4_000_000),
            ..Default::default()
        });
        let tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(renderer),
            ..Default::default()
        });

        let (details, _, _) = fetch_with(&tool, &url, "markdown", None, None)
            .await
            .expect("fetch");
        assert_eq!(daemon.connections.load(Ordering::SeqCst), 0);
        assert_eq!(details["renderSkipped"]["cause"], "no_daemon");
        assert_eq!(
            details["renderSkipped"]["detail"],
            "the daemon socket belongs to another user"
        );
        server.abort();
    }

    #[tokio::test]
    async fn maps_status_errors_to_integration_not_connected_causes() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let cases: Vec<(&str, Reply, &str)> = vec![
            ("never", Reply::Never, "daemon_unreachable"),
            (
                "mismatched id",
                Reply::Json(json!({
                    "id": "someone-else",
                    "result": { "daemon_version": "0.4.0", "protocol_version": "1.4", "browsers": [browser_json()], "sessions": [] }
                })),
                "incompatible",
            ),
            (
                "non-object result",
                Reply::Json(json!({ "result": "ok" })),
                "incompatible",
            ),
            (
                "protocol 2.0",
                Reply::Json(status_reply(vec![browser_json()], "2.0")),
                "incompatible",
            ),
            (
                "no browser",
                Reply::Json(status_reply(vec![], "1.4")),
                "no_browser",
            ),
            (
                "only unresponsive",
                Reply::Json(status_reply(
                    vec![{
                        let mut b = browser_json();
                        b["unresponsive"] = json!(true);
                        b
                    }],
                    "1.4",
                )),
                "browser_unresponsive",
            ),
        ];

        for (label, reply, expected_cause) in cases {
            let reply_json = match reply {
                Reply::Never => None,
                Reply::Json(v) => Some(v),
            };
            let daemon = FakeDaemon::start(connected_browser(url.clone(), move |call| {
                if call.method != "system.status" {
                    return None;
                }
                let r = reply_json.clone();
                Some(Box::pin(async move {
                    match r {
                        None => Reply::Never,
                        Some(v) => Reply::Json(v),
                    }
                }))
            }))
            .await;
            let renderer = create_browser_skill_renderer(BrowserSkillOptions {
                home: Some(daemon.home.clone()),
                settle_poll_ms: Some(0),
                status_timeout_ms: Some(50),
                ..Default::default()
            });
            let tool = create_webfetch_tool_with(WebfetchToolOptions {
                renderer: Some(renderer),
                ..Default::default()
            });
            let (details, _, _) = fetch_with(&tool, &url, "markdown", None, None)
                .await
                .expect("fetch");
            assert_eq!(
                details["renderSkipped"]["reason"], "integration_not_connected",
                "{label}"
            );
            assert_eq!(details["renderSkipped"]["cause"], expected_cause, "{label}");
            assert!(
                !daemon.calls().iter().any(|c| c.method == "session.start"),
                "{label}"
            );
        }
        server.abort();
    }

    #[tokio::test]
    async fn picks_responsive_most_recently_connected_browser() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let mut older = browser_json();
        older["instance_id"] = json!("old");
        older["connected_at_ms"] = json!(1);
        let mut newer = browser_json();
        newer["instance_id"] = json!("new");
        newer["connected_at_ms"] = json!(9);
        let mut stale = browser_json();
        stale["instance_id"] = json!("stale");
        stale["connected_at_ms"] = json!(99);
        stale["unresponsive"] = json!(true);

        let browsers = vec![older, stale, newer];
        let daemon = FakeDaemon::start(connected_browser(url.clone(), move |call| {
            if call.method != "system.status" {
                return None;
            }
            let browsers = browsers.clone();
            Some(Box::pin(async move {
                Reply::Json(status_reply(browsers, "1.4"))
            }))
        }))
        .await;
        let tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(&daemon.home)),
            ..Default::default()
        });

        let _ = fetch_with(&tool, &url, "markdown", None, None)
            .await
            .expect("fetch");
        assert_eq!(daemon.calls()[1].params["browser_instance_id"], "new");
        server.abort();
    }

    #[tokio::test]
    async fn skips_extension_below_protocol_1_4_without_starting_session() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let mut old_ext = browser_json();
        old_ext["extension_protocol_version"] = json!("1.3");
        let daemon = FakeDaemon::start(connected_browser(url.clone(), move |call| {
            if call.method != "system.status" {
                return None;
            }
            let b = old_ext.clone();
            Some(Box::pin(async move {
                Reply::Json(status_reply(vec![b], "1.4"))
            }))
        }))
        .await;
        let tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(&daemon.home)),
            ..Default::default()
        });

        let (details, _, _) = fetch_with(&tool, &url, "markdown", None, None)
            .await
            .expect("fetch");
        assert_eq!(details["renderSkipped"]["reason"], "incognito_unsupported");
        let methods: Vec<String> = daemon.calls().into_iter().map(|c| c.method).collect();
        assert_eq!(methods, ["system.status"]);
        server.abort();
    }

    #[tokio::test]
    async fn maps_incognito_session_start_errors() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        for (error_json, expected_reason, expected_msg) in [
            (
                json!({ "code": "unsupported", "message": "no", "data": { "reason": "unsupported_feature" } }),
                "incognito_unsupported",
                "no",
            ),
            (
                json!({ "code": "internal", "message": "Allow in incognito is off" }),
                "incognito_unavailable",
                "Allow in incognito is off",
            ),
        ] {
            let err = error_json.clone();
            let daemon = FakeDaemon::start(connected_browser(url.clone(), move |call| {
                if call.method != "session.start" {
                    return None;
                }
                let err = err.clone();
                Some(Box::pin(
                    async move { Reply::Json(json!({ "error": err })) },
                ))
            }))
            .await;
            let tool = create_webfetch_tool_with(WebfetchToolOptions {
                renderer: Some(default_renderer(&daemon.home)),
                ..Default::default()
            });
            let (details, _, _) = fetch_with(&tool, &url, "markdown", None, None)
                .await
                .expect("fetch");
            assert_eq!(details["renderSkipped"]["reason"], expected_reason);
            assert!(
                details["renderSkipped"]["detail"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(expected_msg)
            );
        }
        server.abort();
    }

    #[tokio::test]
    async fn maps_unresponsive_extension_during_navigate_and_stops_session() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let daemon = FakeDaemon::start(connected_browser(url.clone(), |call| {
            if call.method != "tool.navigate" {
                return None;
            }
            Some(Box::pin(async {
                Reply::Json(json!({
                    "error": { "code": "timeout", "message": "late", "data": { "reason": "extension_unresponsive" } }
                }))
            }))
        }))
        .await;
        let tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(&daemon.home)),
            ..Default::default()
        });

        let (details, _, _) = fetch_with(&tool, &url, "markdown", None, None)
            .await
            .expect("fetch");
        assert_eq!(
            details["renderSkipped"]["reason"],
            "integration_not_connected"
        );
        assert_eq!(details["renderSkipped"]["cause"], "browser_unresponsive");
        assert_eq!(
            daemon.calls().last().map(|c| c.method.as_str()),
            Some("session.stop")
        );
        server.abort();
    }

    #[tokio::test]
    async fn cancels_and_stops_after_deadline_and_returns_static_text() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let daemon = FakeDaemon::start(connected_browser(url.clone(), |call| {
            if call.method != "tool.navigate" {
                return None;
            }
            Some(Box::pin(async { Reply::Never }))
        }))
        .await;
        let tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(&daemon.home)),
            ..Default::default()
        });

        let (details, _, _) = fetch_with(&tool, &url, "markdown", None, Some(4))
            .await
            .expect("fetch");
        assert_eq!(details["renderSkipped"]["reason"], "timeout");
        let calls = daemon.calls();
        let methods: Vec<&str> = calls.iter().map(|c| c.method.as_str()).collect();
        assert!(methods.contains(&"cancel"));
        assert_eq!(methods.last().copied(), Some("session.stop"));
        server.abort();
    }

    #[tokio::test]
    async fn aborts_and_stops_session_when_aborted_mid_render() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let signal = AbortSignal::default();
        let trigger = signal.clone();
        let daemon = FakeDaemon::start(connected_browser(url.clone(), move |call| {
            if call.method != "tool.navigate" {
                return None;
            }
            let trigger = trigger.clone();
            Some(Box::pin(async move {
                trigger.abort_with_reason(Some("user cancelled".into()));
                Reply::Never
            }))
        }))
        .await;
        let tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(&daemon.home)),
            ..Default::default()
        });

        let result = fetch_with(&tool, &url, "markdown", Some(signal), None).await;
        assert!(result.is_err());
        assert_eq!(
            daemon.calls().last().map(|c| c.method.as_str()),
            Some("session.stop")
        );
        server.abort();
    }

    #[tokio::test]
    async fn uses_timed_out_navigation_html_and_reports_no_more_text() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let timed_out = FakeDaemon::start(connected_browser(url.clone(), {
            let url = url.clone();
            move |call| {
                if call.method != "tool.navigate" {
                    return None;
                }
                let url = url.clone();
                Some(Box::pin(async move {
                    Reply::Json(json!({
                        "result": { "tab_id": 1, "url": url, "final_url": url, "reached": "timeout" }
                    }))
                }))
            }
        }))
        .await;
        let empty = FakeDaemon::start(connected_browser(url.clone(), |call| {
            if call.method != "tool.get_html" {
                return None;
            }
            Some(Box::pin(async {
                Reply::Json(json!({
                    "result": { "html": APP_SHELL, "truncated": false, "byte_size": 1, "tab_id": 1 }
                }))
            }))
        }))
        .await;

        let used_tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(&timed_out.home)),
            ..Default::default()
        });
        let unchanged_tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(&empty.home)),
            ..Default::default()
        });

        let (used, _, _) = fetch_with(&used_tool, &url, "markdown", None, None)
            .await
            .expect("used");
        let (unchanged, _, _) = fetch_with(&unchanged_tool, &url, "markdown", None, None)
            .await
            .expect("unchanged");
        assert_eq!(used["rendered"], true);
        assert_eq!(unchanged["renderSkipped"]["reason"], "no_more_text");
        server.abort();
    }

    #[tokio::test]
    async fn never_contacts_daemon_for_content_page_html_format_or_disabled() {
        let article = format!(
            r#"<!doctype html><html><head><title>Notes</title><script src="/analytics.js"></script></head>
<body><article><h1>Release notes</h1><p>{}</p></article></body></html>"#,
            "This release fixes the reader so long articles keep every paragraph. ".repeat(6)
        );
        let (article_url, article_server) = serve_html(article).await;
        let (shell_url, shell_server) = serve_html(APP_SHELL.into()).await;
        let daemon = FakeDaemon::start(connected_browser(shell_url.clone(), |_| None)).await;

        let tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(&daemon.home)),
            ..Default::default()
        });
        let (content_details, _, _) = fetch_with(&tool, &article_url, "markdown", None, None)
            .await
            .expect("article");
        let (raw_details, raw_text, _) = fetch_with(&tool, &shell_url, "html", None, None)
            .await
            .expect("html");

        let disabled_tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(&daemon.home)),
            browser_enabled: Some(false),
            ..Default::default()
        });
        let (disabled_details, _, _) =
            fetch_with(&disabled_tool, &shell_url, "markdown", None, None)
                .await
                .expect("disabled");

        assert_eq!(daemon.connections.load(Ordering::SeqCst), 0);
        assert!(content_details.get("renderSkipped").is_none());
        assert_eq!(raw_text, APP_SHELL);
        assert!(raw_details.get("renderSkipped").is_none());
        assert_eq!(disabled_details["renderSkipped"]["reason"], "disabled");

        article_server.abort();
        shell_server.abort();
    }

    #[tokio::test]
    async fn keeps_at_most_two_browser_sessions_open_at_once() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let held: Arc<Mutex<Vec<tokio::sync::oneshot::Sender<()>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let held_notify = Arc::new(tokio::sync::Notify::new());
        let held_bg = Arc::clone(&held);
        let notify_bg = Arc::clone(&held_notify);

        let url_bg = url.clone();
        let daemon = FakeDaemon::start(connected_browser(url.clone(), move |call| {
            if call.method != "tool.navigate" {
                return None;
            }
            let held = Arc::clone(&held_bg);
            let notify = Arc::clone(&notify_bg);
            let url = url_bg.clone();
            Some(Box::pin(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                held.lock().expect("held").push(tx);
                notify.notify_waiters();
                let _ = rx.await;
                Reply::Json(json!({
                    "result": { "tab_id": 1, "url": url, "final_url": url, "reached": "networkidle" }
                }))
            }))
        }))
        .await;

        let renderer = create_browser_skill_renderer(BrowserSkillOptions {
            home: Some(daemon.home.clone()),
            settle_poll_ms: Some(0),
            status_timeout_ms: Some(2_000),
            ..Default::default()
        });
        let tool = Arc::new(create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(renderer),
            ..Default::default()
        }));

        let f1 = {
            let tool = Arc::clone(&tool);
            let url = url.clone();
            tokio::spawn(async move { fetch_with(&tool, &url, "markdown", None, None).await })
        };
        let f2 = {
            let tool = Arc::clone(&tool);
            let url = url.clone();
            tokio::spawn(async move { fetch_with(&tool, &url, "markdown", None, None).await })
        };
        let f3 = {
            let tool = Arc::clone(&tool);
            let url = url.clone();
            tokio::spawn(async move { fetch_with(&tool, &url, "markdown", None, None).await })
        };

        tokio::time::timeout(Duration::from_secs(5), async {
            while held.lock().expect("held").len() < 2 {
                held_notify.notified().await;
            }
        })
        .await
        .expect("two sessions held");

        let live_while_two_held = daemon.live_sessions.load(Ordering::SeqCst);
        let senders: Vec<_> = held.lock().expect("held").drain(..).collect();
        for tx in senders {
            let _ = tx.send(());
        }

        tokio::time::timeout(Duration::from_secs(5), async {
            while held.lock().expect("held").is_empty() {
                held_notify.notified().await;
            }
        })
        .await
        .expect("third session held");

        let senders: Vec<_> = held.lock().expect("held").drain(..).collect();
        for tx in senders {
            let _ = tx.send(());
        }

        let (r1, r2, r3) =
            tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(f1, f2, f3) })
                .await
                .expect("all fetches complete");

        assert_eq!(live_while_two_held, 2);
        assert_eq!(daemon.max_live_sessions.load(Ordering::SeqCst), 2);
        for res in [r1, r2, r3] {
            let (details, _, _) = res.expect("join").expect("fetch");
            assert_eq!(details["rendered"], true);
        }
        server.abort();
    }

    #[tokio::test]
    async fn returns_url_html_was_read_from_when_page_navigated_after_load() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let tab_list_calls = Arc::new(AtomicUsize::new(0));
        let daemon = FakeDaemon::start(connected_browser(url.clone(), {
            let url = url.clone();
            let count = Arc::clone(&tab_list_calls);
            move |call| {
                if call.method != "tool.tab_list" {
                    return None;
                }
                let n = count.fetch_add(1, Ordering::SeqCst) + 1;
                let current = if n == 1 {
                    url.clone()
                } else {
                    "https://mail.other.test/inbox".into()
                };
                Some(Box::pin(async move {
                    Reply::Json(json!({
                        "result": { "tabs": [{ "tab_id": 1, "url": current, "scope": "agent" }] }
                    }))
                }))
            }
        }))
        .await;

        let outcome = default_renderer(&daemon.home)
            .render(
                &url,
                PageRenderOptions {
                    mode: BrowserMode::Incognito,
                    timeout_ms: 5_000,
                    signal: None,
                    browser: None,
                },
            )
            .await
            .expect("render");
        assert_eq!(outcome.page.url, "https://mail.other.test/inbox");
        server.abort();
    }

    #[tokio::test]
    async fn refuses_page_that_never_holds_still_and_stops_session() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let tab_list_calls = Arc::new(AtomicUsize::new(0));
        let daemon = FakeDaemon::start(connected_browser(url.clone(), {
            let url = url.clone();
            let count = Arc::clone(&tab_list_calls);
            move |call| {
                if call.method != "tool.tab_list" {
                    return None;
                }
                let n = count.fetch_add(1, Ordering::SeqCst) + 1;
                let current = if n.is_multiple_of(2) {
                    url.clone()
                } else {
                    "https://mail.other.test/inbox".into()
                };
                Some(Box::pin(async move {
                    Reply::Json(json!({
                        "result": { "tabs": [{ "tab_id": 1, "url": current, "scope": "agent" }] }
                    }))
                }))
            }
        }))
        .await;

        let err = default_renderer(&daemon.home)
            .render(
                &url,
                PageRenderOptions {
                    mode: BrowserMode::Incognito,
                    timeout_ms: 5_000,
                    signal: None,
                    browser: None,
                },
            )
            .await
            .expect_err("should fail");
        let RenderError::Skip(skip) = err else {
            panic!("expected skip error, got {err:?}");
        };
        assert_eq!(skip.reason, WebfetchBrowserSkipReason::RenderFailed);
        assert_eq!(
            skip.detail.as_deref(),
            Some("the page kept navigating while it was read")
        );
        let calls = daemon.calls();
        assert_eq!(
            calls.iter().filter(|c| c.method == "tool.get_html").count(),
            8
        );
        assert_eq!(
            calls.last().map(|c| c.method.as_str()),
            Some("session.stop")
        );
        server.abort();
    }

    #[tokio::test]
    async fn keeps_page_already_read_when_later_read_fails() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let reads = Arc::new(AtomicUsize::new(0));
        let daemon = FakeDaemon::start(connected_browser(url.clone(), {
            let reads = Arc::clone(&reads);
            move |call| {
                if call.method != "tool.get_html" {
                    return None;
                }
                let n = reads.fetch_add(1, Ordering::SeqCst) + 1;
                Some(Box::pin(async move {
                    if n > 2 {
                        return Reply::Never;
                    }
                    let body = RENDERED.replace("Team board", &format!("Team board {}", "x".repeat(n)));
                    Reply::Json(json!({
                        "result": { "html": body, "truncated": false, "byte_size": body.len(), "tab_id": 1 }
                    }))
                }))
            }
        }))
        .await;
        let tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(&daemon.home)),
            ..Default::default()
        });

        let (details, user_text, _) = fetch_with(&tool, &url, "markdown", None, Some(4))
            .await
            .expect("fetch");
        assert_eq!(details["rendered"], true);
        assert!(user_text.contains("Three cards are waiting for review"));
        assert_eq!(
            daemon.calls().last().map(|c| c.method.as_str()),
            Some("session.stop")
        );
        server.abort();
    }

    #[tokio::test]
    async fn stops_session_created_after_start_call_was_abandoned() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let signal = AbortSignal::default();
        let trigger = signal.clone();
        let started: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let started_read = Arc::clone(&started);
        let daemon = FakeDaemon::start(connected_browser(url.clone(), move |call| {
            let trigger = trigger.clone();
            let started = Arc::clone(&started);
            let started_read = Arc::clone(&started_read);
            match call.method.as_str() {
                "session.start" => {
                    let params = call.params.clone();
                    Some(Box::pin(async move {
                        started.lock().expect("started").push(params);
                        trigger.abort_with_reason(Some("user cancelled".into()));
                        Reply::Json(json!({ "result": { "session_id": "s-orphan" } }))
                    }))
                }
                "system.status" => Some(Box::pin(async move {
                    let sessions: Vec<Value> = started_read
                        .lock()
                        .expect("started")
                        .iter()
                        .map(|p| json!({ "session_id": "s-orphan", "controller": p["controller"] }))
                        .collect();
                    Reply::Json(json!({
                        "result": {
                            "protocol_version": "1.4",
                            "browsers": [browser_json()],
                            "sessions": sessions
                        }
                    }))
                })),
                _ => None,
            }
        }))
        .await;
        let tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(&daemon.home)),
            ..Default::default()
        });

        let res = fetch_with(&tool, &url, "markdown", Some(signal), None).await;
        assert!(res.is_err());
        let stop = daemon
            .calls()
            .into_iter()
            .find(|c| c.method == "session.stop")
            .expect("session.stop");
        assert_eq!(stop.params, json!({ "session_id": "s-orphan" }));
        server.abort();
    }

    #[tokio::test]
    async fn reads_never_settling_page_bounded_number_of_times() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let reads = Arc::new(AtomicUsize::new(0));
        let reads_bg = Arc::clone(&reads);
        let daemon = FakeDaemon::start(connected_browser(url.clone(), move |call| {
            if call.method != "tool.get_html" {
                return None;
            }
            let n = reads_bg.fetch_add(1, Ordering::SeqCst) + 1;
            Some(Box::pin(async move {
                let body = RENDERED.replace("Team board", &format!("Team board {}", "x".repeat(n)));
                Reply::Json(json!({
                    "result": { "html": body, "truncated": false, "byte_size": body.len(), "tab_id": 1 }
                }))
            }))
        }))
        .await;
        let tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(&daemon.home)),
            ..Default::default()
        });

        let (details, _, _) = fetch_with(&tool, &url, "markdown", None, None)
            .await
            .expect("fetch");
        assert_eq!(details["rendered"], true);
        assert_eq!(reads.load(Ordering::SeqCst), 12);
        server.abort();
    }

    #[tokio::test]
    async fn keeps_page_already_read_when_daemon_errors_or_tab_gone() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        for mode in ["error", "notab"] {
            let reads = Arc::new(AtomicUsize::new(0));
            let lists = Arc::new(AtomicUsize::new(0));
            let u = url.clone();
            let daemon = FakeDaemon::start(connected_browser(url.clone(), move |call| {
                let reads = Arc::clone(&reads);
                let lists = Arc::clone(&lists);
                let u = u.clone();
                match call.method.as_str() {
                    "tool.get_html" => {
                        let n = reads.fetch_add(1, Ordering::SeqCst) + 1;
                        Some(Box::pin(async move {
                            if mode == "error" && n > 2 {
                                return Reply::Json(json!({
                                    "error": { "code": "internal", "message": "read failed" }
                                }));
                            }
                            let body = RENDERED
                                .replace("Team board", &format!("Team board {}", "x".repeat(n)));
                            Reply::Json(json!({
                                "result": { "html": body, "truncated": false, "byte_size": body.len(), "tab_id": 1 }
                            }))
                        }))
                    }
                    "tool.tab_list" => {
                        let l = lists.fetch_add(1, Ordering::SeqCst) + 1;
                        Some(Box::pin(async move {
                            if mode == "notab" && l > 4 {
                                return Reply::Json(json!({ "result": { "tabs": [] } }));
                            }
                            Reply::Json(json!({
                                "result": { "tabs": [{ "tab_id": 1, "url": u, "scope": "agent" }] }
                            }))
                        }))
                    }
                    _ => None,
                }
            }))
            .await;
            let tool = create_webfetch_tool_with(WebfetchToolOptions {
                renderer: Some(default_renderer(&daemon.home)),
                ..Default::default()
            });
            let (details, user_text, _) = fetch_with(&tool, &url, "markdown", None, None)
                .await
                .expect("fetch");
            assert_eq!(details["rendered"], true, "{mode}");
            assert!(
                user_text.contains("Three cards are waiting for review"),
                "{mode}"
            );
            assert_eq!(
                daemon.calls().last().map(|c| c.method.as_str()),
                Some("session.stop")
            );
        }
        server.abort();
    }

    #[tokio::test]
    async fn does_not_wait_for_abandoned_session_when_session_start_errored() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let daemon = FakeDaemon::start(connected_browser(url.clone(), |call| {
            if call.method != "session.start" {
                return None;
            }
            Some(Box::pin(async {
                Reply::Json(json!({ "error": { "code": "internal", "message": "no window" } }))
            }))
        }))
        .await;
        let tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(&daemon.home)),
            ..Default::default()
        });

        let (details, _, _) = fetch_with(&tool, &url, "markdown", None, None)
            .await
            .expect("fetch");
        assert_eq!(details["renderSkipped"]["reason"], "incognito_unavailable");
        assert_eq!(
            daemon
                .calls()
                .iter()
                .filter(|c| c.method == "system.status")
                .count(),
            1
        );
        server.abort();
    }

    #[tokio::test]
    async fn reports_rendered_page_url_as_final_url() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let landed = format!("{url}/board");
        let daemon = FakeDaemon::start(connected_browser(landed.clone(), {
            let landed = landed.clone();
            move |call| {
                if call.method != "tool.navigate" {
                    return None;
                }
                let landed = landed.clone();
                Some(Box::pin(async move {
                    Reply::Json(json!({ "result": { "tab_id": 1, "final_url": landed } }))
                }))
            }
        }))
        .await;
        let tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(&daemon.home)),
            ..Default::default()
        });

        let (details, _, _) = fetch_with(&tool, &url, "markdown", None, None)
            .await
            .expect("fetch");
        assert_eq!(details["rendered"], true);
        assert_eq!(details["finalUrl"], landed);
        server.abort();
    }

    #[tokio::test]
    async fn does_not_count_read_from_another_tab_than_navigated() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let daemon = FakeDaemon::start(connected_browser(url.clone(), |call| {
            if call.method != "tool.get_html" {
                return None;
            }
            Some(Box::pin(async {
                Reply::Json(json!({
                    "result": { "html": RENDERED, "truncated": false, "byte_size": RENDERED.len(), "tab_id": 7 }
                }))
            }))
        }))
        .await;
        let tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(&daemon.home)),
            ..Default::default()
        });

        let (details, user_text, _) = fetch_with(&tool, &url, "markdown", None, None)
            .await
            .expect("fetch");
        assert_eq!(details["rendered"], false);
        assert_eq!(details["renderSkipped"]["reason"], "render_failed");
        assert!(!user_text.contains("Three cards are waiting for review"));
        server.abort();
    }

    #[tokio::test]
    async fn opens_no_window_when_aborted_while_picking_browser() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let signal = AbortSignal::default();
        let trigger = signal.clone();
        let daemon = FakeDaemon::start(connected_browser(url.clone(), move |call| {
            if call.method != "system.status" {
                return None;
            }
            trigger.abort();
            None
        }))
        .await;

        let err = default_renderer(&daemon.home)
            .render(
                &url,
                PageRenderOptions {
                    mode: BrowserMode::Incognito,
                    timeout_ms: 5_000,
                    signal: Some(signal),
                    browser: None,
                },
            )
            .await
            .expect_err("aborted");
        assert_eq!(err, RenderError::Aborted);
        assert!(!daemon.calls().iter().any(|c| c.method == "session.start"));
        server.abort();
    }

    #[tokio::test]
    async fn reclaims_session_when_session_start_answered_with_timeout_error() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let daemon = FakeDaemon::start(connected_browser(url.clone(), |call| {
            if call.method != "session.start" {
                return None;
            }
            Some(Box::pin(async {
                Reply::Json(json!({ "error": { "code": "timeout", "message": "extension slow" } }))
            }))
        }))
        .await;
        let tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(&daemon.home)),
            ..Default::default()
        });

        let _ = fetch_with(&tool, &url, "markdown", None, None)
            .await
            .expect("fetch");
        assert!(
            daemon
                .calls()
                .iter()
                .filter(|c| c.method == "system.status")
                .count()
                > 1
        );
        server.abort();
    }

    #[tokio::test]
    async fn discards_signed_in_read_when_tab_navigated_to_unlisted_host_after_load() {
        let article = format!(
            r#"<!doctype html><html><head><title>Notes</title><script src="/analytics.js"></script></head>
<body><article><h1>Release notes</h1><p>{}</p></article></body></html>"#,
            "This release fixes the reader so long articles keep every paragraph. ".repeat(6)
        );
        let (raw_url, server) = serve_html(article).await;
        let url = raw_url.replace("127.0.0.1", "localhost");
        let tab_list_calls = Arc::new(AtomicUsize::new(0));
        let daemon = FakeDaemon::start(connected_browser(url.clone(), {
            let url = url.clone();
            let count = Arc::clone(&tab_list_calls);
            move |call| {
                if call.method != "tool.tab_list" {
                    return None;
                }
                let n = count.fetch_add(1, Ordering::SeqCst) + 1;
                let current = if n == 1 {
                    url.clone()
                } else {
                    "https://mail.other.test/inbox".into()
                };
                Some(Box::pin(async move {
                    Reply::Json(json!({
                        "result": { "tabs": [{ "tab_id": 1, "url": current, "scope": "agent" }] }
                    }))
                }))
            }
        }))
        .await;
        let tool = create_webfetch_tool_with(WebfetchToolOptions {
            renderer: Some(default_renderer(&daemon.home)),
            signed_in_settings: Some(Arc::new(|| SignedInBrowserSettings {
                hosts: vec!["localhost".into()],
                browser: None,
            })),
            ..Default::default()
        });

        let (details, user_text, _) = fetch_with(&tool, &url, "markdown", None, None)
            .await
            .expect("fetch");
        assert!(!user_text.contains("Three cards are waiting for review"));
        assert!(user_text.contains("long articles keep every paragraph"));
        assert_eq!(details["route"], "http");
        assert_eq!(details["signedInSkipped"]["reason"], "host_not_allowed");
        assert!(
            details["signedInSkipped"]["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("mail.other.test")
        );
        assert_eq!(
            daemon.calls().last().map(|c| c.method.as_str()),
            Some("session.stop")
        );
        server.abort();
    }

    #[tokio::test]
    async fn refuses_signed_in_read_when_pinned_browser_missing_and_uses_it_when_present() {
        let (url, server) = serve_html(APP_SHELL.into()).await;
        let mut work = browser_json();
        work["instance_id"] = json!("work");
        work["label"] = json!("Work Chrome");
        work["connected_at_ms"] = json!(1);
        let mut personal = browser_json();
        personal["instance_id"] = json!("personal");
        personal["label"] = json!("Personal");
        personal["connected_at_ms"] = json!(9);

        let daemon_missing = FakeDaemon::start(connected_browser(url.clone(), {
            let personal = personal.clone();
            move |call| {
                if call.method != "system.status" {
                    return None;
                }
                let b = personal.clone();
                Some(Box::pin(async move {
                    Reply::Json(status_reply(vec![b], "1.4"))
                }))
            }
        }))
        .await;

        let err = default_renderer(&daemon_missing.home)
            .render(
                &url,
                PageRenderOptions {
                    mode: BrowserMode::SignedIn,
                    timeout_ms: 5_000,
                    signal: None,
                    browser: Some("Work Chrome".into()),
                },
            )
            .await
            .expect_err("pinned missing");
        let RenderError::Skip(skip) = err else {
            panic!("expected skip, got {err:?}");
        };
        assert_eq!(
            skip.reason,
            WebfetchBrowserSkipReason::IntegrationNotConnected
        );
        assert_eq!(
            skip.cause,
            Some(WebfetchNotConnectedCause::PinnedBrowserMissing)
        );
        assert!(
            !daemon_missing
                .calls()
                .iter()
                .any(|c| c.method == "session.start")
        );

        let daemon_both = FakeDaemon::start(connected_browser(url.clone(), move |call| {
            if call.method != "system.status" {
                return None;
            }
            let browsers = vec![work.clone(), personal.clone()];
            Some(Box::pin(async move {
                Reply::Json(status_reply(browsers, "1.4"))
            }))
        }))
        .await;
        let outcome = default_renderer(&daemon_both.home)
            .render(
                &url,
                PageRenderOptions {
                    mode: BrowserMode::SignedIn,
                    timeout_ms: 5_000,
                    signal: None,
                    browser: Some("Work Chrome".into()),
                },
            )
            .await
            .expect("pinned found");
        assert_eq!(outcome.browser.instance_id, "work");
        server.abort();
    }
}
