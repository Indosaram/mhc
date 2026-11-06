use maho_ext_pi_websearch::websearch::{tool::*, types::*};
#[tokio::test]
async fn conflicting_filters_precede_configuration_failure() {
    let mut tool = WebSearchTool::default();
    let loaded = ConfigLoadResult::Failure {
        reason: ConfigLoadFailureReason::MissingConfig,
        message: "missing".into(),
        source: None,
    };
    let params = SearchParams {
        query: "query".into(),
        allowed_domains: Some(vec!["a.example".into()]),
        blocked_domains: Some(vec!["b.example".into()]),
    };
    let result = tool
        .execute(&reqwest::Client::new(), loaded, params, None, None, None)
        .await
        .expect("execute");
    let SearchRenderDetails::Error(details) = result.details else {
        panic!("expected typed error")
    };
    assert!(details.reason.is_none());
    assert_eq!(details.query, "query");
}
#[tokio::test]
async fn configuration_failure_preserves_reason_without_progress() {
    let mut tool = WebSearchTool::default();
    let loaded = ConfigLoadResult::Failure {
        reason: ConfigLoadFailureReason::ProviderNativeBypass,
        message: "native bypass".into(),
        source: None,
    };
    let params = SearchParams {
        query: "query".into(),
        allowed_domains: None,
        blocked_domains: None,
    };
    let mut updates = 0;
    let mut update = |_: SearchToolResult| updates += 1;
    let result = tool
        .execute(
            &reqwest::Client::new(),
            loaded,
            params,
            None,
            None,
            Some(&mut update),
        )
        .await
        .expect("execute");
    let SearchRenderDetails::Error(details) = result.details else {
        panic!("expected typed error")
    };
    assert!(matches!(
        details.reason,
        Some(ConfigLoadFailureReason::ProviderNativeBypass)
    ));
    assert_eq!(updates, 0);
}
#[tokio::test]
async fn tool_round_robin_state_persists_and_progress_is_typed() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(async move {
        let mut paths = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut bytes = Vec::new();
            loop {
                let mut buffer = [0; 1024];
                let count = stream.read(&mut buffer).await.expect("read");
                assert_ne!(count, 0);
                bytes.extend_from_slice(&buffer[..count]);
                if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&bytes);
            paths.push(
                request
                    .lines()
                    .next()
                    .expect("request line")
                    .split_whitespace()
                    .nth(1)
                    .expect("path")
                    .to_owned(),
            );
            let body = r#"{"results":[{"title":"Manual","url":"https://manual.example.com"}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.expect("write");
        }
        paths
    });
    let config=WebsearchConfig{strategy:RoutingStrategy::RoundRobin,fallback:true,auto:false,providers:["one","two"].into_iter().map(|id|serde_json::from_value(serde_json::json!({"provider":"exa","id":id,"baseUrl":format!("http://{address}/{id}"),"maxResults":4})).expect("provider")).collect()};
    let mut tool = WebSearchTool::default();
    let mut updates = Vec::new();
    let client = reqwest::Client::new();
    for expected in ["one", "two"] {
        let mut update = |result: SearchToolResult| updates.push(result.details);
        let loaded = ConfigLoadResult::Success {
            config: config.clone(),
            source: "fixture".into(),
        };
        let params = SearchParams {
            query: "route progress".into(),
            allowed_domains: None,
            blocked_domains: None,
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tool.execute(&client, loaded, params, None, None, Some(&mut update)),
        )
        .await
        .expect("timeout")
        .expect("execute");
        let SearchRenderDetails::Result(details) = result.details else {
            panic!("result details")
        };
        assert_eq!(details.entry_id.as_deref(), Some(expected));
    }
    let paths = tokio::time::timeout(std::time::Duration::from_secs(5), servers.join_next())
        .await
        .expect("server deadline")
        .expect("server task")
        .expect("server");
    assert_eq!(paths, vec!["/one", "/two"]);
    assert_eq!(updates.len(), 4);
    let SearchRenderDetails::Progress(progress) = &updates[0] else {
        panic!("initial progress")
    };
    assert_eq!(progress.max_results, 4.0);
    assert_eq!(progress.provider_labels, vec!["exa/one", "exa/two"]);
    let SearchRenderDetails::Progress(progress) = &updates[3] else {
        panic!("route progress")
    };
    assert_eq!(progress.current_provider.as_deref(), Some("exa/two"));
    assert_eq!(
        progress.route_labels,
        Some(vec!["exa/two".into(), "exa/one".into()])
    );
}

mod upstream_tool_suite {
    use maho_ext_pi_websearch::websearch::{native::*, tool::*, types::*};
    use std::sync::{Arc, Mutex};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    struct Registry(Vec<NativeModelInfo>);
    impl NativeModelRegistry for Registry {
        fn get_api_key_and_headers<'a>(&'a self, _: &'a NativeModelInfo) -> NativeAuthFuture<'a> {
            Box::pin(async {
                Ok(NativeAuthResult::Success {
                    api_key: Some("native-test".into()),
                    headers: None,
                })
            })
        }
        fn get_available(&self) -> Option<Vec<NativeModelInfo>> {
            Some(self.0.clone())
        }
    }

    fn model(provider: &str, id: &str, base_url: &str) -> NativeModelInfo {
        NativeModelInfo {
            provider: provider.into(),
            id: id.into(),
            base_url: base_url.into(),
        }
    }

    struct GatewayGuard(tokio::task::JoinHandle<()>);
    impl Drop for GatewayGuard {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    /// Records request lines. CONNECT tunnels (native https routes) are refused so the attempt
    /// fails at the transport and routing falls through; plain requests get an Exa payload.
    async fn gateway() -> (
        String,
        reqwest::Client,
        Arc<Mutex<Vec<String>>>,
        GatewayGuard,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let task = tokio::spawn(async move {
            let mut requests = tokio::task::JoinSet::new();
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let log = Arc::clone(&log);
                requests.spawn(async move {
                    let mut bytes = Vec::new();
                    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        let mut buffer = [0; 1024];
                        let Ok(count) = stream.read(&mut buffer).await else {
                            return;
                        };
                        if count == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&buffer[..count]);
                    }
                    let line = String::from_utf8_lossy(&bytes)
                        .lines()
                        .next()
                        .unwrap_or("")
                        .to_owned();
                    let mut parts = line.split_whitespace();
                    let entry = format!(
                        "{} {}",
                        parts.next().unwrap_or(""),
                        parts.next().unwrap_or("")
                    );
                    let response = if entry.starts_with("CONNECT ") {
                        "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_owned()
                    } else {
                        let body = r#"{"results":[{"title":"Manual","url":"https://manual.example.com","text":"manual"}]}"#;
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    };
                    log.lock().expect("request log").push(entry);
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        let proxy = reqwest::Proxy::https(format!("http://{address}")).expect("proxy");
        let client = reqwest::Client::builder()
            .no_proxy()
            .proxy(proxy)
            .build()
            .expect("client");
        (
            format!("http://{address}/exa"),
            client,
            seen,
            GatewayGuard(task),
        )
    }

    fn config(manual: &str, auto: bool, max_results: Option<f64>) -> WebsearchConfig {
        let mut provider = serde_json::json!({"provider":"exa","id":"manual","apiKey":"exa-test","baseUrl":manual});
        if let Some(max) = max_results {
            provider["maxResults"] = serde_json::json!(max);
        }
        WebsearchConfig {
            strategy: RoutingStrategy::Priority,
            fallback: true,
            auto,
            providers: vec![serde_json::from_value(provider).expect("provider")],
        }
    }

    async fn run(
        client: &reqwest::Client,
        config: WebsearchConfig,
        query: &str,
        active: NativeModelInfo,
        available: Vec<NativeModelInfo>,
    ) -> (SearchDetails, Vec<SearchToolResult>) {
        let registry = Registry(available);
        let mut updates = Vec::new();
        let mut update = |result: SearchToolResult| updates.push(result);
        let loaded = ConfigLoadResult::Success {
            config,
            source: "test".into(),
        };
        let params = SearchParams {
            query: query.into(),
            allowed_domains: None,
            blocked_domains: None,
        };
        let context = WebSearchToolContext {
            model: Some(&active),
            model_registry: Some(&registry),
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            WebSearchTool::default().execute(
                client,
                loaded,
                params,
                Some(context),
                None,
                Some(&mut update),
            ),
        )
        .await
        .expect("execution deadline")
        .expect("execute");
        let SearchRenderDetails::Result(details) = result.details else {
            panic!("result details")
        };
        (details, updates)
    }

    fn entry_ids(details: &SearchDetails) -> Vec<String> {
        details
            .attempts
            .iter()
            .flatten()
            .map(|attempt| attempt.entry_id.clone().unwrap_or_default())
            .collect()
    }

    fn no_step_counter(updates: &[SearchToolResult]) {
        for update in updates {
            assert!(!has_step_counter(&update.text), "{}", update.text);
        }
    }

    fn has_step_counter(text: &str) -> bool {
        text.split('[').skip(1).any(|rest| {
            let Some((inner, _)) = rest.split_once(']') else {
                return false;
            };
            let Some((left, right)) = inner.split_once('/') else {
                return false;
            };
            !left.is_empty()
                && !right.is_empty()
                && left
                    .bytes()
                    .chain(right.bytes())
                    .all(|byte| byte.is_ascii_digit())
        })
    }

    #[test]
    fn definition_exposes_name_label_and_free_code_schema() {
        let tool = definition();
        assert_eq!(tool.name, "web_search");
        assert_eq!(tool.label, "Web Search");
        assert!(tool.description.contains("Search the web"));
        assert_eq!(tool.parameters["required"], serde_json::json!(["query"]));
        for key in ["query", "allowed_domains", "blocked_domains"] {
            assert!(tool.parameters["properties"].get(key).is_some(), "{key}");
        }
        assert!(tool.parameters["properties"].get("maxResults").is_none());
        assert_eq!(tool.parameters["additionalProperties"], false);
    }

    #[tokio::test]
    async fn auto_with_matching_active_model_prepends_native_route_and_keeps_configured_limit() {
        let (manual, client, seen, _server) = gateway().await;
        let active = model("openai", "gpt-5.5", "https://gateway.example.com/v1");
        let (details, updates) = run(
            &client,
            config(&manual, true, Some(2.0)),
            "native route",
            active,
            Vec::new(),
        )
        .await;
        assert_eq!(
            *seen.lock().expect("request log"),
            ["CONNECT gateway.example.com:443", "POST /exa"]
        );
        assert_eq!(entry_ids(&details), ["native", "manual"]);
        let SearchRenderDetails::Progress(first) = &updates[0].details else {
            panic!("initial progress")
        };
        assert_eq!(first.provider_labels, ["openai/native", "exa/manual"]);
        assert_eq!(first.max_results, 2.0);
    }

    #[tokio::test]
    async fn auto_disabled_or_unsupported_active_model_keeps_manual_route_only() {
        for (auto, id) in [(false, "gpt-5.5"), (true, "gpt-3.5")] {
            let (manual, client, seen, _server) = gateway().await;
            let active = model("openai", id, "https://gateway.example.com/v1");
            let (details, _) = run(
                &client,
                config(&manual, auto, None),
                "manual route",
                active,
                Vec::new(),
            )
            .await;
            assert_eq!(
                *seen.lock().expect("request log"),
                ["POST /exa"],
                "auto={auto} id={id}"
            );
            assert_eq!(details.provider, SearchProvider::Exa);
            assert_eq!(details.entry_id.as_deref(), Some("manual"));
        }
    }

    #[tokio::test]
    async fn configured_route_emits_progress_without_step_counter() {
        let (manual, client, _, _server) = gateway().await;
        let active = model("openai", "gpt-5.5", "https://gateway.example.com/v1");
        let (details, updates) = run(
            &client,
            config(&manual, false, Some(4.0)),
            "route progress",
            active,
            Vec::new(),
        )
        .await;
        assert_eq!(details.provider, SearchProvider::Exa);
        assert_eq!(
            updates[0].text,
            "Searching \"route progress\" via exa/manual (max 4)"
        );
        let progress = updates
            .iter()
            .find_map(|update| match &update.details {
                SearchRenderDetails::Progress(progress) => progress.current_provider.clone(),
                _ => None,
            })
            .expect("route progress");
        assert_eq!(progress, "exa/manual");
        no_step_counter(&updates);
    }

    #[tokio::test]
    async fn discovered_native_entry_id_renders_as_provider_native() {
        let (address, server) = crate::tool_tls::spawn_gateway_server(1, |_, _| {
            (
                200,
                serde_json::json!({"output": [{"type": "web_search_call", "action": {
                    "sources": [{"url": "https://native.example.com"}]
                }}]})
                .to_string(),
            )
        });
        let client = crate::tool_tls::test_client(address);
        let provider = serde_json::json!({"provider":"openai","id":"native-openai-abc123","apiKey":"openai-test","baseUrl":"https://gateway.example.com/v1/responses","model":"gpt-5.5"});
        let config = WebsearchConfig {
            strategy: RoutingStrategy::Priority,
            fallback: true,
            auto: false,
            providers: vec![serde_json::from_value(provider).expect("provider")],
        };
        let active = model("openai", "gpt-5.5", "https://gateway.example.com/v1");
        let (_, updates) = run(&client, config, "native label", active, Vec::new()).await;
        assert_eq!(
            server.join().await,
            ["https://gateway.example.com/v1/responses"]
        );
        let SearchRenderDetails::Progress(first) = &updates[0].details else {
            panic!("initial progress")
        };
        assert_eq!(first.provider_labels, ["openai/native"]);
        let SearchRenderDetails::Progress(route) = &updates[1].details else {
            panic!("route progress")
        };
        assert_eq!(route.current_provider.as_deref(), Some("openai/native"));
        no_step_counter(&updates);
    }

    #[tokio::test]
    async fn auto_discovery_prepends_available_routes_with_upstream_fingerprint_ids() {
        let (manual, client, seen, _server) = gateway().await;
        let active = model("openai", "gpt-3.5", "https://gateway.example.com/v1");
        let available = vec![
            model(
                "anthropic",
                "claude-sonnet-4",
                "https://anthropic.gateway.example.com/v1",
            ),
            model("openai", "gpt-5.5", "https://openai.gateway.example.com/v1"),
        ];
        let (details, _) = run(
            &client,
            config(&manual, true, None),
            "discovered route",
            active,
            available,
        )
        .await;
        assert_eq!(
            *seen.lock().expect("request log"),
            [
                "CONNECT anthropic.gateway.example.com:443",
                "CONNECT openai.gateway.example.com:443",
                "POST /exa"
            ]
        );
        let ids = entry_ids(&details);
        assert_eq!(ids[0], "native-anthropic-8bae5c53ca2e2eed");
        assert!(ids[1].starts_with("native-openai-"));
        assert_eq!(ids[2], "manual");
    }

    #[tokio::test]
    async fn same_provider_distinct_routes_keep_unique_upstream_ids() {
        let (manual, client, seen, _server) = gateway().await;
        let active = model("custom", "not-search", "https://gateway.example.com/v1");
        let available = vec![
            model(
                "openai",
                "gpt-4.1",
                "https://openai-a.gateway.example.com/v1",
            ),
            model(
                "openai",
                "gpt-5.5",
                "https://openai-b.gateway.example.com/v1",
            ),
        ];
        let (details, _) = run(
            &client,
            config(&manual, true, None),
            "distinct route ids",
            active,
            available,
        )
        .await;
        assert_eq!(
            *seen.lock().expect("request log"),
            [
                "CONNECT openai-a.gateway.example.com:443",
                "CONNECT openai-b.gateway.example.com:443",
                "POST /exa"
            ]
        );
        assert_eq!(
            entry_ids(&details),
            [
                "native-openai-55398fe2d4e9bf84",
                "native-openai-a41f926c003bdbda",
                "manual"
            ]
        );
    }

    #[tokio::test]
    async fn error_details_serialize_without_provider_attribution() {
        let mut tool = WebSearchTool::default();
        let client = reqwest::Client::new();
        let loaded = ConfigLoadResult::Success {
            config: config("https://unused.example.com", false, None),
            source: "test".into(),
        };
        let params = SearchParams {
            query: "bad filters".into(),
            allowed_domains: Some(vec!["example.com".into()]),
            blocked_domains: Some(vec!["example.org".into()]),
        };
        let result = tool
            .execute(&client, loaded, params, None, None, None)
            .await
            .expect("execute");
        assert_eq!(
            serde_json::to_value(result.details).expect("details"),
            serde_json::json!({"phase":"error","query":"bad filters","error":"Error: Cannot specify both allowed_domains and blocked_domains in the same request"})
        );
        let loaded = ConfigLoadResult::Failure {
            reason: ConfigLoadFailureReason::InvalidConfig,
            message: "Invalid provider config in .pi/websearch.json".into(),
            source: None,
        };
        let params = SearchParams {
            query: "config failure".into(),
            allowed_domains: None,
            blocked_domains: None,
        };
        let result = tool
            .execute(&client, loaded, params, None, None, None)
            .await
            .expect("execute");
        assert_eq!(
            serde_json::to_value(result.details).expect("details"),
            serde_json::json!({"phase":"error","query":"config failure","error":"Invalid provider config in .pi/websearch.json","reason":"invalid_config"})
        );
    }
}
