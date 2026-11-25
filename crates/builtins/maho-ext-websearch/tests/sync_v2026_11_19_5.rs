use maho_ext_websearch::websearch::{
    config::{load_websearch_config, validate_provider_config},
    provider_endpoints::SearchProvider,
    providers::{build_search_request, normalize_search_response, search_response_error},
    search::{format_search_text, is_keyless_provider, perform_search},
    tool::{create_web_search_tool, create_web_search_tool_with_registry},
    types::{
        ConfigLoadFailureReason, ConfigLoadResult, RoutingStrategy, SearchDetails,
        SearchProviderConfig, SearchProviderEntry, SearchRequest, SearchResultItem,
        WebsearchConfig,
    },
};
use maho_tools::definition::{ToolCall, ToolContent};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const FIRECRAWL_RESULTS: &str = include_str!("fixtures/websearch/firecrawl-search-results.json");
const JINA_RESULTS: &str = include_str!("fixtures/websearch/jina-results.json");
const KEENABLE_RESULTS: &str = include_str!("fixtures/websearch/keenable-results.json");
const OPENROUTER_ANSWER_ONLY: &str = include_str!("fixtures/websearch/openrouter-answer-only.json");
const OPENROUTER_GROUNDED: &str = include_str!("fixtures/websearch/openrouter-grounded.json");
const PARALLEL_MCP_SSE: &str = include_str!("fixtures/websearch/parallel-mcp-content-text.sse");
const PARALLEL_MCP_JSON: &str = include_str!("fixtures/websearch/parallel-mcp-results.json");
const PARALLEL_REST_JSON: &str = include_str!("fixtures/websearch/parallel-rest-results.json");
const SYNTHETIC_RESULTS: &str = include_str!("fixtures/websearch/synthetic-results.json");

#[derive(Clone, Debug)]
struct RecordedRequest {
    target: String,
    headers: std::collections::BTreeMap<String, String>,
    body: String,
}

struct LoopbackServer {
    addr: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for LoopbackServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl LoopbackServer {
    async fn start(
        responder: impl Fn(&RecordedRequest) -> (u16, String, String, Vec<(String, String)>)
        + Send
        + Sync
        + 'static,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let responder = Arc::new(responder);
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let recorded = Arc::clone(&recorded);
                let responder = Arc::clone(&responder);
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let header_end = loop {
                        if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos + 4;
                        }
                        let Ok(n) = socket.read(&mut chunk).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&chunk[..n]);
                    };
                    let head = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
                    let mut headers = std::collections::BTreeMap::new();
                    let mut content_length = 0usize;
                    for line in head.lines().skip(1) {
                        if let Some((name, val)) = line.split_once(':') {
                            let lower = name.trim().to_ascii_lowercase();
                            let val = val.trim().to_owned();
                            if lower == "content-length" {
                                content_length = val.parse().unwrap_or(0);
                            }
                            headers.insert(lower, val);
                        }
                    }
                    while bytes.len() < header_end + content_length {
                        let Ok(n) = socket.read(&mut chunk).await else {
                            return;
                        };
                        if n == 0 {
                            break;
                        }
                        bytes.extend_from_slice(&chunk[..n]);
                    }
                    let target = head
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_owned();
                    let body = String::from_utf8_lossy(
                        &bytes[header_end..bytes.len().min(header_end + content_length)],
                    )
                    .into_owned();
                    let req = RecordedRequest {
                        target,
                        headers,
                        body,
                    };
                    recorded.lock().expect("requests").push(req.clone());
                    let (status, content_type, resp_body, extra_headers) = responder(&req);
                    let mut raw = format!(
                        "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
                        resp_body.len()
                    );
                    for (k, v) in extra_headers {
                        raw.push_str(&format!("{k}: {v}\r\n"));
                    }
                    raw.push_str("\r\n");
                    let _ = socket.write_all(raw.as_bytes()).await;
                    let _ = socket.write_all(resp_body.as_bytes()).await;
                });
            }
        });
        Self {
            addr,
            requests,
            handle,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("requests").clone()
    }
}

fn single_config(entry: SearchProviderEntry) -> WebsearchConfig {
    WebsearchConfig {
        strategy: RoutingStrategy::Priority,
        fallback: true,
        auto: false,
        native_model: None,
        providers: vec![entry],
    }
}

fn entry(config: SearchProviderConfig) -> SearchProviderEntry {
    SearchProviderEntry {
        config,
        priority: None,
        weight: None,
        fallback_model: None,
    }
}

fn req(query: &str, max_results: f64) -> SearchRequest {
    SearchRequest {
        query: query.into(),
        max_results,
        ..Default::default()
    }
}

// --- websearch-max-results.test.ts ---

async fn run_keenable_tool(
    params: Value,
    entry_max_results: Option<f64>,
) -> (Value, String, Vec<Value>, Vec<RecordedRequest>) {
    let server = LoopbackServer::start(|_| {
        (
            200,
            "application/json".into(),
            KEENABLE_RESULTS.into(),
            vec![],
        )
    })
    .await;
    let mut cfg = SearchProviderConfig::new(SearchProvider::Keenable);
    cfg.base_url = Some(server.url("/search"));
    cfg.max_results = entry_max_results;
    let config = single_config(entry(cfg));
    let tool = create_web_search_tool(Arc::new(move || ConfigLoadResult::Ok {
        config: config.clone(),
        source: "test".into(),
    }));
    let progress = Arc::new(Mutex::new(Vec::new()));
    let progress_capture = Arc::clone(&progress);
    let result = (tool.execute)(ToolCall {
        id: "call-1",
        params,
        signal: Default::default(),
        on_update: Some(Arc::new(move |update| {
            if let Some(details) = update.details {
                progress_capture.lock().expect("progress").push(details);
            }
            Ok(())
        })),
        context: None,
    })
    .await
    .expect("execute");
    let text = match &result.content[0] {
        ToolContent::Text { text, .. } => text.clone(),
        _ => String::new(),
    };
    let updates = progress.lock().expect("progress").clone();
    let reqs = server.requests();
    (result.details.expect("details"), text, updates, reqs)
}

#[tokio::test]
async fn max_results_and_snippet_cap_suite() {
    let recorded: Value = serde_json::from_str(KEENABLE_RESULTS).expect("keenable fixture");
    let query = recorded["query"].as_str().expect("query");

    // 1. no max_results -> 10
    let (details, _, progress, reqs) = run_keenable_tool(json!({ "query": query }), None).await;
    assert_eq!(details["results"].as_array().expect("results").len(), 10);
    assert_eq!(details["truncated"], true);
    let sent: Value = serde_json::from_str(&reqs[0].body).expect("body");
    assert_eq!(sent["max_results"], 10.0);
    assert_eq!(progress[0]["maxResults"], 10.0);

    // 2. max_results: 3 -> 3
    let (details, _, progress, reqs) =
        run_keenable_tool(json!({ "query": query, "max_results": 3 }), None).await;
    assert_eq!(details["results"].as_array().expect("results").len(), 3);
    assert_eq!(details["truncated"], true);
    let sent: Value = serde_json::from_str(&reqs[0].body).expect("body");
    assert_eq!(sent["max_results"], 3.0);
    assert_eq!(progress[0]["maxResults"], 3.0);

    // 3. entry maxResults: 7 and no call count -> 7
    let (details, _, _, reqs) = run_keenable_tool(json!({ "query": query }), Some(7.)).await;
    assert_eq!(details["results"].as_array().expect("results").len(), 7);
    let sent: Value = serde_json::from_str(&reqs[0].body).expect("body");
    assert_eq!(sent["max_results"], 7.0);

    // 4. both call count (3) and entry maxResults (7) -> call wins (3)
    let (details, _, _, reqs) =
        run_keenable_tool(json!({ "query": query, "max_results": 3 }), Some(7.)).await;
    assert_eq!(details["results"].as_array().expect("results").len(), 3);
    let sent: Value = serde_json::from_str(&reqs[0].body).expect("body");
    assert_eq!(sent["max_results"], 3.0);

    // 5. call count above range (100) -> clamped to 50
    let (_, _, progress, reqs) =
        run_keenable_tool(json!({ "query": query, "max_results": 100 }), None).await;
    let sent: Value = serde_json::from_str(&reqs[0].body).expect("body");
    assert_eq!(progress[0]["maxResults"], 50.0);
    assert_eq!(sent["max_results"], 50.0);

    // 6. call count 0 or -5 -> clamped to 1
    for count in [0, -5] {
        let (details, _, progress, reqs) =
            run_keenable_tool(json!({ "query": query, "max_results": count }), None).await;
        let sent: Value = serde_json::from_str(&reqs[0].body).expect("body");
        assert_eq!(progress[0]["maxResults"], 1.0);
        assert_eq!(sent["max_results"], 1.0);
        assert_eq!(details["results"].as_array().expect("results").len(), 1);
    }

    // 7. exact fixture count (25) -> not truncated
    let (details, _, _, _) =
        run_keenable_tool(json!({ "query": query, "max_results": 25 }), None).await;
    assert_eq!(details["results"].as_array().expect("results").len(), 25);
    assert_eq!(details["truncated"], false);

    // 8. recorded long snippet stops at 500 chars
    let (_, text, _, _) =
        run_keenable_tool(json!({ "query": query, "max_results": 1 }), None).await;
    let snippet_line = text
        .lines()
        .find(|line| line.contains("satisfies operator narrows"))
        .expect("snippet line");
    assert_eq!(snippet_line.encode_utf16().count(), 3 + 500);
    assert!(snippet_line.ends_with('…'));

    // 9 & 10. exact 500 kept, 501 truncated to 499 + ellipsis
    let details_with = |snippet: String| SearchDetails {
        provider: SearchProvider::Keenable,
        query: "typescript satisfies operator".into(),
        results: vec![SearchResultItem {
            title: "Result".into(),
            url: "https://example.com/result".into(),
            snippet: Some(snippet),
            source: None,
            score: None,
            published_at: None,
        }],
        duration_ms: 1.,
        truncated: false,
        ..Default::default()
    };
    let exact = "a".repeat(500);
    assert!(format_search_text(&details_with(exact.clone())).contains(&format!("   {exact}")));
    let over = "a".repeat(501);
    let formatted = format_search_text(&details_with(over));
    let line = formatted
        .lines()
        .find(|l| l.starts_with("   a"))
        .expect("line");
    assert_eq!(line, format!("   {}…", "a".repeat(499)));
}

// --- websearch-firecrawl-provider.test.ts ---

#[tokio::test]
async fn firecrawl_provider_validation_fixture_and_error() {
    assert!(is_keyless_provider(SearchProvider::Firecrawl));
    assert!(
        validate_provider_config(&entry(SearchProviderConfig::new(SearchProvider::Firecrawl)))
            .is_ok()
    );
    let mut keyed = SearchProviderConfig::new(SearchProvider::Firecrawl);
    keyed.api_key = Some("fc-test".into());
    assert!(validate_provider_config(&entry(keyed)).is_ok());

    let mut plaintext = SearchProviderConfig::new(SearchProvider::Firecrawl);
    plaintext.base_url = Some("http://localhost:3002/v2/search".into());
    assert_eq!(
        validate_provider_config(&entry(plaintext))
            .expect_err("plaintext")
            .0,
        ConfigLoadFailureReason::InvalidConfig
    );

    let fixture: Value = serde_json::from_str(FIRECRAWL_RESULTS).expect("firecrawl fixture");
    let results = normalize_search_response(SearchProvider::Firecrawl, &fixture);
    assert_eq!(results.len(), 4);
    assert_eq!(results[0].title, "TypeScript 4.9 Release Notes");
    assert_eq!(
        results[0].url,
        "https://www.typescriptlang.org/docs/handbook/release-notes/typescript-4-9.html"
    );
    assert_eq!(results[3].title, "https://untitled.example.com/satisfies");
    assert_eq!(
        search_response_error(
            SearchProvider::Firecrawl,
            &json!({ "success": false, "error": "Invalid API key" })
        )
        .as_deref(),
        Some("Invalid API key")
    );

    let server = LoopbackServer::start(|_| {
        (
            200,
            "application/json".into(),
            json!({ "success": false, "error": "Invalid API key" }).to_string(),
            vec![],
        )
    })
    .await;
    let mut cfg = SearchProviderConfig::new(SearchProvider::Firecrawl);
    cfg.base_url = Some(server.url("/v2/search"));
    let details = perform_search(
        &single_config(entry(cfg)),
        &req("current docs", 5.),
        None,
        None,
        None,
    )
    .await
    .expect("perform_search");
    assert_eq!(
        details.error.as_deref(),
        Some("All configured search providers failed: firecrawl Invalid API key")
    );
    assert!(details.results.is_empty());
}

// --- websearch-jina-provider.test.ts ---

#[tokio::test]
async fn jina_provider_validation_fixture_and_loopback() {
    assert_eq!(
        validate_provider_config(&entry(SearchProviderConfig::new(SearchProvider::Jina)))
            .expect_err("missing key")
            .0,
        ConfigLoadFailureReason::MissingApiKey
    );
    let mut keyed = SearchProviderConfig::new(SearchProvider::Jina);
    keyed.api_key = Some("jina-test".into());
    assert!(validate_provider_config(&entry(keyed.clone())).is_ok());

    let fixture: Value = serde_json::from_str(JINA_RESULTS).expect("jina fixture");
    let results = normalize_search_response(SearchProvider::Jina, &fixture);
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].title, "Documentation - TypeScript 4.9");

    let ok_server =
        LoopbackServer::start(|_| (200, "application/json".into(), JINA_RESULTS.into(), vec![]))
            .await;
    let mut cfg = keyed.clone();
    cfg.base_url = Some(ok_server.url("/"));
    cfg.allowed_domains = Some(vec!["typescriptlang.org".into()]);
    let details = perform_search(
        &single_config(entry(cfg)),
        &req("typescript satisfies operator", 5.),
        None,
        None,
        None,
    )
    .await
    .expect("jina search");
    let reqs = ok_server.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].target, "/typescript%20satisfies%20operator?count=5");
    assert_eq!(reqs[0].headers["authorization"], "Bearer jina-test");
    assert_eq!(reqs[0].headers["x-site"], "typescriptlang.org");
    assert_eq!(reqs[0].headers["x-respond-with"], "no-content");
    assert!(details.error.is_none());
    assert_eq!(details.results.len(), 3);

    let err_server = LoopbackServer::start(|_| {
        (
            200,
            "application/json".into(),
            json!({ "code": 402, "message": "Insufficient balance" }).to_string(),
            vec![],
        )
    })
    .await;
    let mut err_cfg = keyed;
    err_cfg.base_url = Some(err_server.url("/"));
    let details = perform_search(
        &single_config(entry(err_cfg)),
        &req("typescript satisfies operator", 5.),
        None,
        None,
        None,
    )
    .await
    .expect("jina err");
    assert!(details.results.is_empty());
    assert_eq!(
        details.error.as_deref(),
        Some(
            "All configured search providers failed: jina Jina API response reported failure (402)"
        )
    );
}

// --- websearch-ollama-provider.test.ts ---

#[test]
fn ollama_provider_validation_and_clamp() {
    assert_eq!(
        validate_provider_config(&entry(SearchProviderConfig::new(SearchProvider::Ollama)))
            .expect_err("missing key")
            .0,
        ConfigLoadFailureReason::MissingApiKey
    );
    let mut cfg = SearchProviderConfig::new(SearchProvider::Ollama);
    cfg.api_key = Some("ol-test".into());
    assert!(validate_provider_config(&entry(cfg.clone())).is_ok());

    let capped = build_search_request(&cfg, &req("q", 500.)).expect("build");
    assert_eq!(capped.body.expect("body")["max_results"], 10.0);
    let floored = build_search_request(&cfg, &req("q", 0.)).expect("build");
    assert_eq!(floored.body.expect("body")["max_results"], 1.0);
}

// --- websearch-openrouter-provider.test.ts ---

#[tokio::test]
async fn openrouter_validation_fixture_and_loopback() {
    let mut no_key = SearchProviderConfig::new(SearchProvider::OpenRouter);
    no_key.model = Some("openai/gpt-5.2".into());
    assert_eq!(
        validate_provider_config(&entry(no_key))
            .expect_err("missing key")
            .0,
        ConfigLoadFailureReason::MissingApiKey
    );
    let mut no_model = SearchProviderConfig::new(SearchProvider::OpenRouter);
    no_model.api_key = Some("or-test".into());
    assert_eq!(
        validate_provider_config(&entry(no_model))
            .expect_err("missing model")
            .0,
        ConfigLoadFailureReason::InvalidConfig
    );
    let mut valid = SearchProviderConfig::new(SearchProvider::OpenRouter);
    valid.api_key = Some("or-test".into());
    valid.model = Some("anthropic/claude-sonnet-4.5".into());
    assert!(validate_provider_config(&entry(valid.clone())).is_ok());

    let grounded: Value = serde_json::from_str(OPENROUTER_GROUNDED).expect("grounded");
    let results = normalize_search_response(SearchProvider::OpenRouter, &grounded);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "Documentation - TypeScript 4.9");

    let grounded_server = LoopbackServer::start(|_| {
        (
            200,
            "application/json".into(),
            OPENROUTER_GROUNDED.into(),
            vec![],
        )
    })
    .await;
    let mut cfg = valid.clone();
    cfg.base_url = Some(grounded_server.url("/api/v1"));
    let details = perform_search(
        &single_config(entry(cfg)),
        &req("typescript satisfies operator", 10.),
        None,
        None,
        None,
    )
    .await
    .expect("grounded");
    assert!(details.error.is_none());
    assert_eq!(details.results.len(), 2);
    assert!(!details.results.iter().any(|r| r.url.contains("uncited")));

    let answer_only_server = LoopbackServer::start(|_| {
        (
            200,
            "application/json".into(),
            OPENROUTER_ANSWER_ONLY.into(),
            vec![],
        )
    })
    .await;
    let mut cfg = valid.clone();
    cfg.base_url = Some(answer_only_server.url("/api/v1"));
    let details = perform_search(
        &single_config(entry(cfg)),
        &req("typescript satisfies operator", 10.),
        None,
        None,
        None,
    )
    .await
    .expect("answer only");
    assert!(details.results.is_empty());
    assert!(
        details
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Search provider openrouter returned no results")
    );

    let err_server = LoopbackServer::start(|_| {
        (
            503,
            "application/json".into(),
            json!({ "error": { "message": "upstream unavailable" } }).to_string(),
            vec![],
        )
    })
    .await;
    let mut cfg = valid;
    cfg.base_url = Some(err_server.url("/api/v1"));
    let details = perform_search(
        &single_config(entry(cfg)),
        &req("typescript satisfies operator", 10.),
        None,
        None,
        None,
    )
    .await
    .expect("503");
    assert!(details.results.is_empty());
    assert!(
        details
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Search failed with HTTP 503: upstream unavailable")
    );
}

// --- websearch-parallel-provider.test.ts ---

#[tokio::test]
async fn parallel_provider_keyless_sse_and_mcp_error() {
    assert!(is_keyless_provider(SearchProvider::Parallel));
    assert!(
        validate_provider_config(&entry(SearchProviderConfig::new(SearchProvider::Parallel)))
            .is_ok()
    );

    let mcp_server = LoopbackServer::start(|_| {
        (
            200,
            "application/json".into(),
            PARALLEL_MCP_JSON.into(),
            vec![],
        )
    })
    .await;
    let mut cfg = SearchProviderConfig::new(SearchProvider::Parallel);
    cfg.base_url = Some(mcp_server.url("/mcp"));
    let details = perform_search(
        &single_config(entry(cfg)),
        &req("typescript satisfies operator", 10.),
        None,
        None,
        None,
    )
    .await
    .expect("mcp json");
    assert!(details.error.is_none());
    assert_eq!(details.results.len(), 3);
    assert_eq!(
        details.results[0].published_at.as_deref(),
        Some("2023-01-03")
    );
    assert_eq!(details.results[1].published_at, None);
    assert_eq!(
        details.results[2].published_at.as_deref(),
        Some("2025-05-08")
    );

    let sse_server = LoopbackServer::start(|_| {
        (
            200,
            "text/event-stream".into(),
            PARALLEL_MCP_SSE.into(),
            vec![],
        )
    })
    .await;
    let mut cfg = SearchProviderConfig::new(SearchProvider::Parallel);
    cfg.base_url = Some(sse_server.url("/mcp"));
    let details = perform_search(
        &single_config(entry(cfg)),
        &req("typescript satisfies operator", 10.),
        None,
        None,
        None,
    )
    .await
    .expect("mcp sse");
    assert!(details.error.is_none());
    assert_eq!(details.results.len(), 3);

    let err_server = LoopbackServer::start(|_| {
        (
            200,
            "application/json".into(),
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": { "isError": true, "content": [{ "type": "text", "text": "query rejected" }] }
            })
            .to_string(),
            vec![],
        )
    })
    .await;
    let mut cfg = SearchProviderConfig::new(SearchProvider::Parallel);
    cfg.base_url = Some(err_server.url("/mcp"));
    let details = perform_search(
        &single_config(entry(cfg)),
        &req("typescript satisfies operator", 10.),
        None,
        None,
        None,
    )
    .await
    .expect("mcp error");
    assert_eq!(
        details.error.as_deref(),
        Some(
            "All configured search providers failed: parallel Parallel search error: query rejected"
        )
    );
    assert!(details.results.is_empty());

    let rest_payload: Value = serde_json::from_str(PARALLEL_REST_JSON).expect("rest fixture");
    let rest_results = normalize_search_response(SearchProvider::Parallel, &rest_payload);
    assert_eq!(rest_results.len(), 3);
}

// --- websearch-public-provider.test.ts ---

#[tokio::test]
async fn public_provider_config_and_call_max_results_precedence() {
    assert!(is_keyless_provider(SearchProvider::Public));
    assert!(
        validate_provider_config(&entry(SearchProviderConfig::new(SearchProvider::Public))).is_ok()
    );
    let mut with_base = SearchProviderConfig::new(SearchProvider::Public);
    with_base.base_url = Some("https://search.example.com".into());
    assert_eq!(
        validate_provider_config(&entry(with_base))
            .expect_err("public baseUrl")
            .0,
        ConfigLoadFailureReason::InvalidConfig
    );

    let empty = tempfile::tempdir().expect("tempdir");
    let loaded = load_websearch_config(empty.path(), empty.path())
        .await
        .expect("load");
    let ConfigLoadResult::Ok { config, .. } = loaded else {
        panic!("expected default config");
    };
    assert!(
        !config
            .providers
            .iter()
            .any(|p| p.config.provider == SearchProvider::Public)
    );

    // Call max_results outranks entry maxResults on public fan-out.
    let ddg = include_str!("fixtures/websearch/duckduckgo-results.html");
    let server = LoopbackServer::start(move |req| {
        if req.target.starts_with("/engines/duckduckgo-html") {
            (200, "text/html".into(), ddg.into(), vec![])
        } else {
            (500, "text/plain".into(), "down".into(), vec![])
        }
    })
    .await;
    let mut pub_cfg = SearchProviderConfig::new(SearchProvider::Public);
    pub_cfg.base_url = Some(server.url("/engines"));
    pub_cfg.max_results = Some(5.);

    let mut tool_cfg = single_config(entry(pub_cfg.clone()));
    let first_cfg = tool_cfg.clone();
    let tool = create_web_search_tool_with_registry(
        Arc::new(move || ConfigLoadResult::Ok {
            config: first_cfg.clone(),
            source: "test".into(),
        }),
        None,
        None,
        Arc::new(tokio::sync::Mutex::new((String::new(), None))),
    );
    let res_call_override = (tool.execute)(ToolCall {
        id: "1",
        params: json!({ "query": "typescript satisfies", "max_results": 2 }),
        signal: Default::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("call override");
    assert_eq!(
        res_call_override.details.expect("details")["results"]
            .as_array()
            .expect("arr")
            .len(),
        2
    );

    tool_cfg.providers[0].config.max_results = Some(3.);
    let tool_entry = create_web_search_tool(Arc::new(move || ConfigLoadResult::Ok {
        config: tool_cfg.clone(),
        source: "test".into(),
    }));
    let res_entry = (tool_entry.execute)(ToolCall {
        id: "2",
        params: json!({ "query": "typescript satisfies" }),
        signal: Default::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("entry default");
    assert_eq!(
        res_entry.details.expect("details")["results"]
            .as_array()
            .expect("arr")
            .len(),
        3
    );
}

// --- websearch-synthetic-provider.test.ts ---

#[tokio::test]
async fn synthetic_provider_fixture_and_local_slice() {
    assert_eq!(
        validate_provider_config(&entry(SearchProviderConfig::new(SearchProvider::Synthetic)))
            .expect_err("missing key")
            .0,
        ConfigLoadFailureReason::MissingApiKey
    );
    let mut cfg = SearchProviderConfig::new(SearchProvider::Synthetic);
    cfg.api_key = Some("syn-test".into());
    assert!(validate_provider_config(&entry(cfg.clone())).is_ok());

    let fixture: Value = serde_json::from_str(SYNTHETIC_RESULTS).expect("synthetic fixture");
    let results = normalize_search_response(SearchProvider::Synthetic, &fixture);
    assert_eq!(results.len(), 4);
    assert_eq!(results[0].title, "Documentation - TypeScript 4.9");
    assert_eq!(results[0].published_at.as_deref(), Some("2024-11-20"));
    assert!(
        results[0]
            .snippet
            .as_deref()
            .unwrap_or_default()
            .contains("satisfies operator")
    );
    assert_eq!(
        results[3].title,
        "https://example.com/guides/satisfies-operator"
    );

    let server = LoopbackServer::start(|_| {
        (
            200,
            "application/json".into(),
            SYNTHETIC_RESULTS.into(),
            vec![],
        )
    })
    .await;
    cfg.base_url = Some(server.url("/v2/search"));
    let details = perform_search(
        &single_config(entry(cfg)),
        &req("typescript satisfies operator", 2.),
        None,
        None,
        None,
    )
    .await
    .expect("synthetic search");
    assert!(details.error.is_none());
    assert!(details.truncated);
    assert_eq!(details.results.len(), 2);
}

// --- websearch-tinyfish-provider.test.ts ---

#[tokio::test]
async fn tinyfish_provider_pagination_cap_and_429_retry_after() {
    assert_eq!(
        validate_provider_config(&entry(SearchProviderConfig::new(SearchProvider::Tinyfish)))
            .expect_err("missing key")
            .0,
        ConfigLoadFailureReason::MissingApiKey
    );
    let mut cfg = SearchProviderConfig::new(SearchProvider::Tinyfish);
    cfg.api_key = Some("tf-test".into());
    assert!(validate_provider_config(&entry(cfg.clone())).is_ok());

    // 10-page cap when every page returns duplicates
    let cap_server = LoopbackServer::start(|_| {
        let results: Vec<Value> = (0..10)
            .map(|i| json!({ "title": format!("R{i}"), "url": format!("https://example.com/{i}") }))
            .collect();
        (
            200,
            "application/json".into(),
            json!({ "results": results }).to_string(),
            vec![],
        )
    })
    .await;
    let mut cap_cfg = cfg.clone();
    cap_cfg.base_url = Some(cap_server.url("/search"));
    let details = perform_search(
        &single_config(entry(cap_cfg)),
        &req("q", 20.),
        None,
        None,
        None,
    )
    .await
    .expect("10-page cap");
    assert_eq!(cap_server.requests().len(), 10);
    assert_eq!(details.results.len(), 10);

    // Later page 429 with Retry-After fails the search cleanly
    let err_server = LoopbackServer::start(|req| {
        let page = req
            .target
            .split('&')
            .find_map(|p| p.strip_prefix("page="))
            .unwrap_or("0");
        if page == "0" {
            let results: Vec<Value> = (0..10)
                .map(|i| json!({ "title": format!("R{i}"), "url": format!("https://example.com/{i}") }))
                .collect();
            (
                200,
                "application/json".into(),
                json!({ "results": results }).to_string(),
                vec![],
            )
        } else {
            (
                429,
                "text/plain".into(),
                "rate limited".into(),
                vec![("Retry-After".into(), "7".into())],
            )
        }
    })
    .await;
    let mut err_cfg = cfg;
    err_cfg.base_url = Some(err_server.url("/search"));
    let details = perform_search(
        &single_config(entry(err_cfg)),
        &req("q", 15.),
        None,
        None,
        None,
    )
    .await
    .expect("later page 429");
    assert!(
        details
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("HTTP 429")
    );
    assert_eq!(
        details.blocked,
        Some(maho_ext_websearch::websearch::types::SearchBlockReason::RateLimited)
    );
    assert_eq!(details.retry_after_seconds, Some(7.0));
}
