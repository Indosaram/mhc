mod support;
use maho_ext_api::{AgentToolResult, ContentBlock};
use maho_ext_pi_webfetch::webfetch::tool::DEFAULT_OUTPUT_MAX_BYTES;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn text(result: &AgentToolResult) -> &str {
    match &result.content[0] {
        ContentBlock::Text(text) => &text.text,
        _ => panic!("expected text"),
    }
}

fn assert_disconnected(result: std::io::Result<usize>) {
    match result {
        Ok(0) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        other => panic!("expected client disconnect, got {other:?}"),
    }
}

/// Fires once per URL when the fetcher enters its body phase (the `pi_webfetch` trace event after `reading_body`
/// is set). Installed as the global default: a scoped dispatch loses the callsite whenever a parallel test registers
/// it first on a thread without a subscriber (tracing-core then caches `Interest::never`).
struct BodyPhaseSubscriber(
    Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<()>>>,
);
struct UrlField(Option<String>);
impl tracing::field::Visit for UrlField {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "url" {
            self.0 = Some(format!("{value:?}"));
        }
    }
}
impl tracing::Subscriber for BodyPhaseSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target() == "pi_webfetch"
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut url = UrlField(None);
        event.record(&mut url);
        if let Some(sender) = url.0.and_then(|url| {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&url)
        }) {
            let _ = sender.send(());
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn body_phase(url: &str) -> tokio::sync::oneshot::Receiver<()> {
    static SUBSCRIBER: std::sync::OnceLock<Arc<BodyPhaseSubscriber>> = std::sync::OnceLock::new();
    let subscriber = SUBSCRIBER.get_or_init(|| {
        let subscriber = Arc::new(BodyPhaseSubscriber(Mutex::default()));
        tracing::subscriber::set_global_default(Arc::clone(&subscriber))
            .expect("the body phase subscriber is the only global default in this binary");
        // A callsite registered concurrently with the install may have cached `never`; re-evaluate them all.
        tracing::callsite::rebuild_interest_cache();
        subscriber
    });
    let (sender, receiver) = tokio::sync::oneshot::channel();
    subscriber
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(url.to_owned(), sender);
    receiver
}

/// One request: headers promise 100 bytes, one arrives, then the fixture waits for the client to disconnect.
async fn stalled_body_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("fixture accept");
        let mut bytes = [0; 4096];
        assert!(socket.read(&mut bytes).await.expect("fixture request") > 0);
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nx")
            .await
            .expect("fixture headers");
        assert_disconnected(socket.read(&mut bytes).await);
    });
    (format!("http://{address}"), server)
}

const DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

// upstream fetcher.ts `readResponseBody`: once the body is being read, an abort surfaces as
// WebfetchAbortError("Request aborted") whatever the signal's reason; before that the reason itself surfaces
// (`registered_custom_abort_reason`). The registered extension executor hands the host signal to the fetcher.
#[tokio::test]
async fn registered_cancellation_after_headers_normalizes_reason() {
    let (url, server) = stalled_body_server().await;
    let body_started = body_phase(&url);
    let controller = maho_ai::utils::abort::AbortController::new();
    let request = tokio::spawn((support::registered_tool().execute)(
        "qa".into(),
        json!({"url":url,"timeout":5}),
        Some(controller.signal()),
        None,
    ));
    tokio::time::timeout(DEADLINE, body_started)
        .await
        .expect("body phase deadline")
        .expect("body phase event");
    controller.abort(Some(maho_ai::utils::abort::AbortReason::new(
        "Error",
        "custom body cancellation",
    )));
    let result = tokio::time::timeout(DEADLINE, request)
        .await
        .expect("registered request deadline")
        .expect("registered request");
    tokio::time::timeout(DEADLINE, server)
        .await
        .expect("disconnect deadline")
        .expect("fixture server");
    assert_eq!(text(&result), "Request aborted");
    assert_eq!(result.is_error, Some(true));
}

// upstream tool.ts passes the execute `signal` to `fetchUrl`: the plain definition body (the in-process dispatch
// and tool-child path) threads its reasonless signal into the fetcher, so a pre-request abort carries the DOM
// default reason and a body-phase abort is normalized to "Request aborted".
#[tokio::test]
async fn definition_threads_the_abort_signal_into_the_fetcher() {
    let execute = maho_ext_pi_webfetch::webfetch::tool::definition().execute;
    let signal = maho_ext_api::AbortSignal::default();
    signal.abort();
    let refused = execute(maho_ext_api::ToolCall {
        id: "qa",
        params: json!({"url":"http://127.0.0.1:1"}),
        signal,
        on_update: None,
        context: None,
    })
    .await
    .expect_err("an aborted signal refuses the request");
    assert_eq!(refused.to_string(), "This operation was aborted");

    let (url, server) = stalled_body_server().await;
    let body_started = body_phase(&url);
    let signal = maho_ext_api::AbortSignal::default();
    let request = tokio::spawn(execute(maho_ext_api::ToolCall {
        id: "qa",
        params: json!({"url":url,"timeout":5}),
        signal: signal.clone(),
        on_update: None,
        context: None,
    }));
    tokio::time::timeout(DEADLINE, body_started)
        .await
        .expect("body phase deadline")
        .expect("body phase event");
    signal.abort();
    let error = tokio::time::timeout(DEADLINE, request)
        .await
        .expect("definition request deadline")
        .expect("definition request")
        .expect_err("a body-phase abort fails the call");
    assert_eq!(error.to_string(), "Request aborted");
    tokio::time::timeout(DEADLINE, server)
        .await
        .expect("disconnect deadline")
        .expect("fixture server");
}

async fn fixture(response: String) -> (String, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture contract");
    let address = listener.local_addr().expect("fixture contract");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("fixture contract");
        let mut request = Vec::new();
        loop {
            let mut bytes = [0; 4096];
            let count = socket.read(&mut bytes).await.expect("fixture contract");
            assert_ne!(count, 0);
            request.extend_from_slice(&bytes[..count]);
            if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                break;
            }
        }
        socket
            .write_all(response.as_bytes())
            .await
            .expect("fixture contract");
        String::from_utf8(request).expect("fixture contract")
    });
    (format!("http://{address}/fixture"), server)
}

fn response(body: &str, content_type: &str, status: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

async fn fetch(body: &str, content_type: &str, format: &str) -> AgentToolResult {
    let (url, server) = fixture(response(body, content_type, "200 OK")).await;
    let tool = support::registered_tool();
    let result = (tool.execute)("qa".into(), json!({"url":url,"format":format}), None, None).await;
    server.await.expect("fixture contract");
    assert_ne!(result.is_error, Some(true));
    result
}

#[tokio::test]
async fn registered_progress_and_metadata() {
    let (url, server) = fixture(response("ready", "text/plain", "200 Custom")).await;
    let updates = Arc::new(Mutex::new(Vec::new()));
    let observed = updates.clone();
    let tool = support::registered_tool();
    let result = (tool.execute)(
        "qa".into(),
        json!({"url":url,"format":"text","timeout":7}),
        None,
        Some(Arc::new(move |update| {
            observed.lock().expect("fixture contract").push(update)
        })),
    )
    .await;
    let request = server.await.expect("fixture contract").to_lowercase();
    assert!(request.contains("sec-fetch-mode: navigate"));
    assert!(request.contains("accept: text/plain;q=1.0"));
    assert_eq!(text(&result), "ready");
    assert_eq!(
        result.details,
        json!({"url":url,"finalUrl":url,"format":"text","status":200,"statusText":"Custom",
        "contentType":"text/plain","bytes":5,"timeoutSeconds":7,"converted":false,"truncated":false,
        "outputTruncated":false,"outputBytes":5,"outputTotalBytes":5})
    );
    assert_eq!(
        updates.lock().expect("fixture contract")[0].details,
        json!({"phase":"fetching","url":url,"format":"text","timeoutSeconds":7})
    );
}

#[tokio::test]
async fn registered_utf8_bom_matches_text_decoder() {
    for (body, expected) in [
        ("\u{feff}ready", "ready"),
        ("\u{feff}\u{feff}ready", "\u{feff}ready"),
        ("r\u{feff}eady", "r\u{feff}eady"),
    ] {
        let result = fetch(body, "text/plain", "text").await;
        assert_eq!(text(&result), expected);
        assert_eq!(result.details["bytes"], body.len());
        assert_eq!(result.details["outputBytes"], expected.len());
        assert_eq!(result.details["outputTotalBytes"], expected.len());
    }
}

#[tokio::test]
async fn registered_markdown_conversion() {
    let result = fetch(
        "<h1>Hello Web</h1><p>Alpha <strong>Beta</strong></p><script>bad()</script>",
        "text/html",
        "markdown",
    )
    .await;
    assert_eq!(text(&result), "## Hello Web\n\nAlpha **Beta**");
    assert_eq!(result.details["converted"], true);
}
#[tokio::test]
async fn registered_text_conversion() {
    let result = fetch(
        "<h1>Hello</h1><p>Alpha<br>Beta</p>",
        "application/xhtml+xml",
        "text",
    )
    .await;
    assert_eq!(text(&result), "Hello\n\nAlpha\nBeta");
    assert_eq!(result.details["converted"], true);
}
#[tokio::test]
async fn registered_raw_html() {
    let body = "<h1>Hello</h1><script>raw()</script>";
    let result = fetch(body, "text/html", "html").await;
    assert_eq!(text(&result), body);
    assert_eq!(result.details["converted"], false);
}
#[tokio::test]
async fn registered_markdown_passthrough() {
    let result = fetch("# Original\n\n**bold**", "text/markdown", "markdown").await;
    assert_eq!(text(&result), "# Original\n\n**bold**");
    assert_eq!(result.details["converted"], false);
}
#[tokio::test]
async fn registered_plain_passthrough() {
    let result = fetch("a  b\n\n\nc", "text/plain", "markdown").await;
    assert_eq!(text(&result), "a  b\n\n\nc");
}
#[tokio::test]
async fn registered_http_error_is_content() {
    let (url, server) = fixture(response("missing", "text/plain", "404 Not Found")).await;
    let result =
        (support::registered_tool().execute)("qa".into(), json!({"url":url}), None, None).await;
    server.await.expect("fixture contract");
    assert_eq!(text(&result), "missing");
    assert_eq!(result.details["status"], 404);
    assert_ne!(result.is_error, Some(true));
}
#[tokio::test]
async fn registered_invalid_url() {
    let result = (support::registered_tool().execute)(
        "qa".into(),
        json!({"url":"file:///secret"}),
        None,
        None,
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(text(&result), "URL must start with http:// or https://");
}
#[tokio::test]
async fn registered_small_output_preserved() {
    let result = fetch("small\nbody", "text/plain", "text").await;
    assert_eq!(text(&result), "small\nbody");
    assert_eq!(result.details["outputTruncated"], false);
}
#[tokio::test]
async fn registered_single_line_utf8_cap() {
    let body = "\u{1f600}".repeat(20000);
    let result = fetch(&body, "text/plain", "text").await;
    assert_eq!(result.details["outputBytes"], DEFAULT_OUTPUT_MAX_BYTES);
    assert_eq!(result.details["outputTotalBytes"], body.len());
    assert!(text(&result).starts_with(&body[..DEFAULT_OUTPUT_MAX_BYTES]));
    assert!(!text(&result).contains('\u{fffd}'));
    assert_eq!(result.details["outputTruncated"], true);
}
#[tokio::test]
async fn registered_multiline_cap() {
    let body = "hello world\n".repeat(10000);
    let result = fetch(&body, "text/plain", "text").await;
    let output = result.details["outputBytes"]
        .as_u64()
        .expect("fixture contract");
    assert!(output <= u64::try_from(DEFAULT_OUTPUT_MAX_BYTES).expect("fixture contract"));
    assert!(text(&result).contains("[Output truncated:"));
    assert_eq!(result.details["outputTotalBytes"], body.len());
}
#[tokio::test]
async fn registered_preaborted() {
    let controller = maho_ai::utils::abort::AbortController::new();
    controller.abort(None);
    let result = (support::registered_tool().execute)(
        "qa".into(),
        json!({"url":"http://127.0.0.1:1"}),
        Some(controller.signal()),
        None,
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(text(&result), "This operation was aborted");
}
#[tokio::test]
async fn registered_oversized_header() {
    let (url, server) =
        fixture("HTTP/1.1 200 OK\r\nContent-Length: 5242881\r\nConnection: close\r\n\r\n".into())
            .await;
    let result =
        (support::registered_tool().execute)("qa".into(), json!({"url":url}), None, None).await;
    server.await.expect("fixture contract");
    assert_eq!(result.is_error, Some(true));
    assert_eq!(text(&result), "Response too large (exceeds 5MB limit)");
}
#[tokio::test]
async fn registered_explicit_article_title() {
    let result = fetch("<title>Wrong</title><h1 class='tit_post'>Preferred</h1><div class='article_view'><p>Article body long enough to be preserved by explicit extraction.</p><aside>Noise</aside></div>", "text/html", "markdown").await;
    assert_eq!(
        text(&result),
        "# Preferred\n\nArticle body long enough to be preserved by explicit extraction."
    );
}
#[tokio::test]
async fn registered_article_without_explicit_wrapper() {
    let result = fetch("<title>Article title</title><nav>Navigation</nav><main><article><h1>Article title</h1><p>First paragraph with enough words to qualify as useful article text and avoid navigation.</p><p>Second paragraph explains important details that should be preserved for the reader.</p></article></main><aside>Sponsored sidebar</aside><footer>Privacy policy</footer>", "text/html", "markdown").await;
    assert!(text(&result).contains("First paragraph"));
    assert!(!text(&result).contains("Navigation"));
    assert!(!text(&result).contains("Sponsored sidebar"));
    assert!(!text(&result).contains("Privacy policy"));
}

#[tokio::test]
async fn registered_text_article_without_explicit_wrapper() {
    let result = fetch("<title>Article title</title><nav>Navigation</nav><main><article><h1>Article title</h1><p>First paragraph with enough words to qualify as useful article text and avoid navigation.</p><p>Second paragraph explains important details that should be preserved for the reader.</p></article></main><aside>Sponsored sidebar</aside>", "text/html", "text").await;
    assert!(text(&result).contains("First paragraph"));
    assert!(!text(&result).contains("Navigation"));
    assert!(!text(&result).contains("Sponsored sidebar"));
}

#[tokio::test]
async fn registered_tistory_text_title_and_linebreaks() {
    let result = fetch("<title>Wrong</title><header><h1>Site</h1></header><h1 class='tit_post'>Preferred</h1><div class='article_view'><p>First long article paragraph<br>second line with important details.</p><p>Next paragraph retained.</p><aside>Noise</aside></div>", "text/html", "text").await;
    assert_eq!(
        text(&result),
        "Preferred\n\nFirst long article paragraph\nsecond line with important details.\n\nNext paragraph retained."
    );
}

#[tokio::test]
async fn registered_entity_decoding_one_layer() {
    let html = "<article><h1>Entity</h1><p>Rendered tag example: &amp;lt;custom-element&amp;gt;</p><p>Escaped ampersand example: AT&amp;amp;T docs</p></article>";
    for format in ["text", "markdown"] {
        let result = fetch(html, "text/html", format).await;
        assert!(text(&result).contains("&lt;custom-element&gt;"));
        assert!(text(&result).contains("AT&amp;T docs"));
    }
}

#[tokio::test]
async fn registered_cancellation_before_headers() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture contract");
    let address = listener.local_addr().expect("fixture contract");
    let (sent, received) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("fixture contract");
        let mut bytes = [0; 4096];
        assert!(socket.read(&mut bytes).await.expect("fixture contract") > 0);
        sent.send(()).expect("fixture contract");
        assert_eq!(socket.read(&mut bytes).await.expect("fixture contract"), 0);
    });
    let controller = maho_ai::utils::abort::AbortController::new();
    let future = (support::registered_tool().execute)(
        "qa".into(),
        json!({"url":format!("http://{address}"),"timeout":5}),
        Some(controller.signal()),
        None,
    );
    let request = tokio::spawn(future);
    tokio::time::timeout(std::time::Duration::from_secs(5), received)
        .await
        .expect("fixture contract")
        .expect("fixture contract");
    controller.abort(None);
    let result = request.await.expect("fixture contract");
    assert_eq!(text(&result), "This operation was aborted");
    assert_eq!(result.is_error, Some(true));
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .expect("fixture contract")
        .expect("fixture contract");
}

#[tokio::test]
async fn registered_timeout_disconnects() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture contract");
    let address = listener.local_addr().expect("fixture contract");
    let (request_received, request_seen) = tokio::sync::oneshot::channel();
    let mut request_received = Some(request_received);
    let (disconnect_observed, disconnect_seen) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("fixture contract");
        let mut bytes = [0; 4096];
        let mut total = 0;
        loop {
            let count = socket.read(&mut bytes).await.expect("fixture contract");
            if count == 0 {
                break;
            }
            total += count;
            if let Some(sender) = request_received.take() {
                let _ = sender.send(());
            }
        }
        assert!(total > 0, "client sent no request before disconnecting");
        disconnect_observed.send(()).expect("disconnect signal");
    });
    let (result_sender, result_receiver) = tokio::sync::oneshot::channel();
    let request = tokio::spawn(async move {
        let result = (support::registered_tool().execute)(
            "qa".into(),
            json!({"url":format!("http://{address}"),"timeout":1}),
            None,
            None,
        )
        .await;
        result_sender.send(result).expect("result receiver");
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), request_seen)
        .await
        .expect("request phase deadline")
        .expect("request phase signal");
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), result_receiver)
        .await
        .expect("timeout result deadline")
        .expect("timeout result");
    request.await.expect("request task");
    assert_eq!(text(&result), "Request timed out after 1s");
    assert_eq!(result.is_error, Some(true));
    tokio::time::timeout(std::time::Duration::from_secs(5), disconnect_seen)
        .await
        .expect("disconnect deadline")
        .expect("disconnect signal");
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .expect("fixture contract")
        .expect("fixture contract");
}

#[tokio::test]
async fn registered_body_timeout_normalizes_abort() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut bytes = [0; 4096];
        assert!(socket.read(&mut bytes).await.expect("request") > 0);
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\npartial")
            .await
            .expect("headers");
        assert_disconnected(socket.read(&mut bytes).await);
    });
    let result = (support::registered_tool().execute)(
        "qa".into(),
        json!({"url":format!("http://{address}"),"timeout":1}),
        None,
        None,
    )
    .await;
    assert_eq!(text(&result), "Request aborted");
    assert_eq!(result.is_error, Some(true));
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .expect("bounded fixture")
        .expect("server");
}

#[tokio::test]
async fn registered_redirect_limit_and_body() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture contract");
    let address = listener.local_addr().expect("fixture contract");
    let server = tokio::spawn(async move {
        for _ in 0..21 {
            let (mut socket, _) = listener.accept().await.expect("fixture contract");
            let mut bytes = [0; 4096];
            assert!(socket.read(&mut bytes).await.expect("fixture contract") > 0);
            socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 4\r\nConnection: close\r\n\r\nloop").await.expect("fixture contract");
        }
    });
    let result = (support::registered_tool().execute)(
        "qa".into(),
        json!({"url":format!("http://{address}/start")}),
        None,
        None,
    )
    .await;
    server.await.expect("fixture contract");
    assert_eq!(text(&result), "loop");
    assert_eq!(result.details["status"], 302);
    assert_eq!(result.details["finalUrl"], format!("http://{address}/next"));
}

#[test]
fn extension_metadata_schema() {
    let tool = maho_ext_pi_webfetch::webfetch::tool::definition();
    assert_eq!(tool.name, "webfetch");
    assert_eq!(tool.label, "Web Fetch");
    assert_eq!(tool.parameters["required"], json!(["url"]));
    assert_eq!(
        tool.parameters["properties"]["format"]["enum"],
        json!(["markdown", "text", "html"])
    );
}

#[test]
fn extension_default_enabled() {
    assert!(maho_ext_pi_webfetch::is_webfetch_enabled(None));
}
#[test]
fn extension_truthy_values() {
    for value in ["1", "true", "yes", "on", " TRUE ", "\tYeS\n"] {
        assert!(maho_ext_pi_webfetch::is_webfetch_enabled(Some(value)));
    }
}
#[test]
fn extension_falsy_values() {
    for value in ["0", "false", "no", "off", " OFF ", "\nNo\t"] {
        assert!(!maho_ext_pi_webfetch::is_webfetch_enabled(Some(value)));
    }
}
#[test]
fn extension_unknown_enabled() {
    assert!(maho_ext_pi_webfetch::is_webfetch_enabled(Some(
        "definitely"
    )));
}

#[test]
fn extension_disabled_no_registration() {
    let mut api = maho_ext_api::ExtensionApi::new(
        maho_ext_api::LoadedExtension::new("pi-webfetch", "/tmp".into(), Default::default()),
        Default::default(),
        Default::default(),
        Default::default(),
    );
    maho_ext_pi_webfetch::WebfetchExtension.register_with_env(&mut api, Some("0"));
    assert!(api.registered.tools.is_empty());
    assert!(api.registered.handlers.is_empty());
    assert!(api.registered.tool_renderers.is_empty());
}
#[test]
fn extension_factory_registers_executor_and_renderers() {
    let mut api = maho_ext_api::ExtensionApi::new(
        maho_ext_api::LoadedExtension::new("pi-webfetch", "/tmp".into(), Default::default()),
        Default::default(),
        Default::default(),
        Default::default(),
    );
    maho_ext_pi_webfetch::WebfetchExtension.register_with_env(&mut api, None);
    assert_eq!(api.registered.tools.len(), 1);
    assert!(
        api.runtime
            .extension_tool_executor("pi-webfetch", "webfetch")
            .is_some()
    );
    assert!(
        api.registered.tool_renderers["webfetch"]
            .clone()
            .downcast::<maho_ext_api::ToolRenderers<(), serde_json::Value>>()
            .is_ok()
    );
    assert!(
        api.registered
            .handlers
            .contains_key(&maho_ext_api::EventKind::SessionStart)
    );
    assert!(
        api.registered
            .handlers
            .contains_key(&maho_ext_api::EventKind::SessionShutdown)
    );
}

#[tokio::test]
async fn registered_streamed_oversize() {
    let body = "x".repeat(5242881);
    let result = fetch_overflow(&body).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(text(&result), "Response too large (exceeds 5MB limit)");
}
async fn fetch_overflow(body: &str) -> AgentToolResult {
    let (url, server) = fixture(format!(
        "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{body}"
    ))
    .await;
    let result =
        (support::registered_tool().execute)("qa".into(), json!({"url":url}), None, None).await;
    server.await.expect("fixture contract");
    result
}
#[tokio::test]
async fn registered_exact_response_limit() {
    let result = fetch(&"x".repeat(5242880), "text/plain", "text").await;
    assert_eq!(result.details["truncated"], true);
    assert_eq!(result.details["bytes"], 5242880);
}
#[tokio::test]
async fn registered_cloudflare_no_retry() {
    let (url, server) = fixture(response("challenge", "text/html", "403 Forbidden")).await;
    let result =
        (support::registered_tool().execute)("qa".into(), json!({"url":url}), None, None).await;
    let request = server.await.expect("fixture contract");
    assert!(request.contains("Chrome/143.0.0.0"));
    assert_eq!(result.details["status"], 403);
    assert_eq!(text(&result), "challenge");
}

#[tokio::test]
async fn registered_custom_abort_reason() {
    let controller = maho_ai::utils::abort::AbortController::new();
    controller.abort(Some(maho_ai::utils::abort::AbortReason::new(
        "Error",
        "custom abort",
    )));
    let result = (support::registered_tool().execute)(
        "qa".into(),
        json!({"url":"http://127.0.0.1:1"}),
        Some(controller.signal()),
        None,
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(text(&result), "custom abort");
}

#[tokio::test]
async fn registered_network_refused_message() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("address");
    drop(listener);
    let result = (support::registered_tool().execute)(
        "qa".into(),
        json!({"url":format!("http://{address}")}),
        None,
        None,
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(text(&result), format!("connect ECONNREFUSED {address}"));
}

#[tokio::test]
async fn registered_pinned_content_differential() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/pinned-content.json"))
            .expect("source-generated fixtures");
    for case in cases.as_array().expect("fixture cases") {
        for format in ["markdown", "text"] {
            let html = case["html"].as_str().expect("html");
            let source_conversion = if format == "markdown" {
                maho_ext_pi_webfetch::webfetch::content::html_to_markdown(
                    html,
                    "http://example.test/post",
                )
            } else {
                maho_ext_pi_webfetch::webfetch::content::html_to_text(
                    html,
                    "http://example.test/post",
                )
            };
            assert_eq!(
                source_conversion,
                case[format].as_str().expect("source output"),
                "{} {format}",
                case["name"]
            );
            let (url, server) = fixture(response(html, "text/html", "200 OK")).await;
            let expected = case[format].as_str().expect("source output").replace(
                "http://example.test/guide",
                url::Url::parse(&url)
                    .expect("fixture URL")
                    .join("/guide")
                    .expect("relative guide")
                    .as_ref(),
            );
            let result = (support::registered_tool().execute)(
                "qa".into(),
                json!({"url":url,"format":format}),
                None,
                None,
            )
            .await;
            server.await.expect("fixture server");
            assert_eq!(text(&result), expected, "{} {format}", case["name"]);
        }
    }
}

#[tokio::test]
async fn registered_closed_socket_message() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut bytes = [0; 4096];
        assert!(socket.read(&mut bytes).await.expect("request") > 0);
    });
    let result = (support::registered_tool().execute)(
        "qa".into(),
        json!({"url":format!("http://{address}")}),
        None,
        None,
    )
    .await;
    server.await.expect("server");
    assert_eq!(text(&result), "other side closed");
    assert_eq!(result.is_error, Some(true));
}

#[tokio::test]
async fn extension_lifecycle_clears_ui_only_interactively() {
    let mut api = maho_ext_api::ExtensionApi::new(
        maho_ext_api::LoadedExtension::new("pi-webfetch", "/tmp".into(), Default::default()),
        Default::default(),
        Default::default(),
        Default::default(),
    );
    maho_ext_pi_webfetch::WebfetchExtension.register_with_env(&mut api, None);
    let ui = Arc::new(support::TestUi::default());
    let mut context = support::context();
    context.ui = ui.clone();
    for interactive in [false, true] {
        context.has_ui = interactive;
        for (kind, mut event) in [
            (
                maho_ext_api::EventKind::SessionStart,
                maho_ext_api::ExtensionEvent::SessionStart(maho_ext_api::SessionStartEvent {
                    sizes_only: false,
                    reason: maho_ext_api::SessionReason::Startup,
                    initial_model_provenance: None,
                    previous_session_file: None,
                    attended: None,
                }),
            ),
            (
                maho_ext_api::EventKind::SessionShutdown,
                maho_ext_api::ExtensionEvent::SessionShutdown(maho_ext_api::SessionShutdownEvent {
                    reason: maho_ext_api::SessionReason::Quit,
                    target_session_file: None,
                    signal: None,
                }),
            ),
        ] {
            for handler in &api.registered.handlers[&kind] {
                handler(&mut event, &context).await.expect("lifecycle");
            }
        }
        assert_eq!(
            ui.cleared.lock().expect("UI receipts").len(),
            if interactive { 4 } else { 0 }
        );
    }
    assert_eq!(
        *ui.cleared.lock().expect("UI receipts"),
        [
            "status:pi-webfetch",
            "widget:pi-webfetch",
            "status:pi-webfetch",
            "widget:pi-webfetch"
        ]
    );
}
