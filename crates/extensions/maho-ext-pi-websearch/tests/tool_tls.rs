use maho_ext_pi_websearch::websearch::{native::*, tool::*, types::*};
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

fn tls_acceptor() -> TlsAcceptor {
    let key_pair = rcgen::KeyPair::generate().expect("key pair");
    let params = rcgen::CertificateParams::new(vec![
        "gateway.example.com".into(),
        "anthropic.gateway.example.com".into(),
        "openai.gateway.example.com".into(),
        "openai-a.gateway.example.com".into(),
        "openai-b.gateway.example.com".into(),
    ]).expect("params");
    let cert = params.self_signed(&key_pair).expect("cert");
    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(key_pair.serialize_der().into());
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .expect("single cert");
    TlsAcceptor::from(Arc::new(config))
}

pub(super) fn test_client(address: std::net::SocketAddr) -> reqwest::Client {
    reqwest::Client::builder().no_proxy().danger_accept_invalid_certs(true)
        .proxy(reqwest::Proxy::https(format!("http://{address}")).expect("fixture proxy"))
        .build().expect("client")
}

pub(super) struct ServerGuard {
    task: Option<tokio::task::JoinHandle<Vec<String>>>,
}
impl Drop for ServerGuard {
    fn drop(&mut self) {
        if let Some(h) = self.task.take() {
            h.abort();
        }
    }
}
impl ServerGuard {
    pub(super) async fn join(mut self) -> Vec<String> {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.task.as_mut().expect("task"),
        ).await.expect("server deadline").expect("server join");
        self.task.take();
        result
    }
}

pub(super) fn spawn_gateway_server<F>(
    expected_requests: usize,
    mut handler: F,
) -> (std::net::SocketAddr, ServerGuard)
where
    F: FnMut(&str, &str) -> (u16, String) + Send + 'static,
{
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let address = listener.local_addr().expect("address");
    let listener = tokio::net::TcpListener::from_std(listener).expect("tokio listener");
    let acceptor = tls_acceptor();

    let task = tokio::spawn(async move {
        let mut paths = Vec::new();
        for _ in 0..expected_requests {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut connect = Vec::new();
            while !connect.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.expect("CONNECT header");
                connect.push(byte[0]);
                assert!(connect.len() < 8192, "bounded CONNECT header");
            }
            assert!(connect.starts_with(b"CONNECT "));
            socket.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await.expect("CONNECT response");
            let mut stream = acceptor.accept(socket).await.expect("tls accept");
            let mut bytes = Vec::new();
            let header_end = loop {
                let mut buffer = [0; 4096];
                let count = stream.read(&mut buffer).await.expect("request read");
                assert_ne!(count, 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = String::from_utf8(bytes[..header_end].to_vec()).expect("headers");
            let mut request = headers.lines().next().expect("request line").split_whitespace();
            let method = request.next().expect("method");
            let path = request.next().expect("path");
            let header = |name: &str| headers.lines().filter_map(|line| line.split_once(':'))
                .find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.trim());
            let host = header("host").expect("host");
            let length = header("content-length").unwrap_or("0").parse::<usize>().expect("length");
            while bytes.len() < header_end + length {
                let mut buffer = [0; 4096];
                let count = stream.read(&mut buffer).await.expect("body read");
                assert_ne!(count, 0);
                bytes.extend_from_slice(&buffer[..count]);
            }
            let url = format!("https://{host}{path}");
            let (status, body) = handler(method, &url);
            paths.push(url);
            let response = format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            stream.write_all(response.as_bytes()).await.expect("response write");
            stream.shutdown().await.expect("tls shutdown");
        }
        paths
    });
    (address, ServerGuard { task: Some(task) })
}
struct ToolRegistry {
    available: Vec<NativeModelInfo>,
}
impl NativeModelRegistry for ToolRegistry {
    fn get_api_key_and_headers<'a>(&'a self, _: &'a NativeModelInfo) -> NativeAuthFuture<'a> {
        Box::pin(async {
            Ok(NativeAuthResult::Success {
                api_key: Some("native-test".into()),
                headers: None,
            })
        })
    }
    fn get_available(&self) -> Option<Vec<NativeModelInfo>> {
        Some(self.available.clone())
    }
}

fn tool_config(auto: bool, max_results: Option<f64>) -> WebsearchConfig {
    WebsearchConfig {
        strategy: RoutingStrategy::Priority,
        fallback: true,
        auto,
        providers: vec![SearchProviderEntry {
            config: SearchProviderConfig {
                provider: SearchProvider::Exa,
                id: Some("manual".into()),
                api_key: Some("exa-test".into()),
                base_url: Some("https://gateway.example.com/exa".into()),
                search_engine_id: None,
                max_results,
                model: None,
                codex_mode: None,
                search_context_size: None,
                allowed_domains: None,
                blocked_domains: None,
                user_location: None,
                timeout_ms: None,
            },
            priority: None,
            weight: None,
        }],
    }
}

#[tokio::test]
async fn auto_enabled_and_matching_active_model_prepends_native_provider() {
    let (addr, server) = spawn_gateway_server(1, |_method, _url| {
        (200, serde_json::json!({
            "output": [{
                "type": "web_search_call",
                "action": { "sources": [{ "url": "https://native.example.com" }] }
            }]
        }).to_string())
    });
    let client = test_client(addr);
    let mut tool = WebSearchTool::default();
    let model = NativeModelInfo {
        provider: "openai".into(),
        id: "gpt-5.5".into(),
        base_url: "https://gateway.example.com/v1".into(),
    };
    let registry = ToolRegistry { available: vec![] };
    let loaded = ConfigLoadResult::Success {
        config: tool_config(true, None),
        source: "test".into(),
    };
    let params = SearchParams {
        query: "native route".into(),
        allowed_domains: None,
        blocked_domains: None,
    };
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tool.execute(&client, loaded, params, Some(WebSearchToolContext {
            model: Some(&model),
            model_registry: Some(&registry),
        }), None, None),
    ).await.expect("timeout").expect("execute");

    let paths = tokio::time::timeout(std::time::Duration::from_secs(5), server.join()).await.expect("server timeout");
    assert_eq!(paths, vec!["https://gateway.example.com/v1/responses"]);
    let SearchRenderDetails::Result(details) = result.details else { panic!("expected result details") };
    assert_eq!(details.provider, SearchProvider::Openai);
    assert_eq!(details.entry_id.as_deref(), Some("native"));
    let attempts = details.attempts.expect("attempts");
    assert_eq!(attempts.iter().filter_map(|a| a.entry_id.as_deref()).collect::<Vec<_>>(), vec!["native"]);
}

#[tokio::test]
async fn native_route_and_configured_max_results_preserves_configured_limit() {
    let (addr, server) = spawn_gateway_server(1, |_method, _url| {
        (200, serde_json::json!({
            "output": [{
                "type": "web_search_call",
                "action": {
                    "sources": [
                        { "url": "https://one.example.com" },
                        { "url": "https://two.example.com" },
                        { "url": "https://three.example.com" }
                    ]
                }
            }]
        }).to_string())
    });
    let client = test_client(addr);
    let mut tool = WebSearchTool::default();
    let model = NativeModelInfo {
        provider: "openai".into(),
        id: "gpt-5.5".into(),
        base_url: "https://gateway.example.com/v1".into(),
    };
    let registry = ToolRegistry { available: vec![] };
    let loaded = ConfigLoadResult::Success {
        config: tool_config(true, Some(2.0)),
        source: "test".into(),
    };
    let params = SearchParams {
        query: "native route".into(),
        allowed_domains: None,
        blocked_domains: None,
    };
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tool.execute(&client, loaded, params, Some(WebSearchToolContext {
            model: Some(&model),
            model_registry: Some(&registry),
        }), None, None),
    ).await.expect("timeout").expect("execute");

    let _paths = tokio::time::timeout(std::time::Duration::from_secs(5), server.join()).await.expect("server timeout");
    let SearchRenderDetails::Result(details) = result.details else { panic!("expected result details") };
    assert_eq!(details.entry_id.as_deref(), Some("native"));
    assert_eq!(details.results.iter().map(|item| item.url.as_str()).collect::<Vec<_>>(), vec!["https://one.example.com", "https://two.example.com"]);
}

#[tokio::test]
async fn auto_enabled_and_registry_has_search_capable_models_prepends_discovered() {
    let (addr, server) = spawn_gateway_server(1, |_method, _url| {
        (200, serde_json::json!({
            "content": [{
                "type": "web_search_tool_result",
                "content": [{ "title": "Claude", "url": "https://claude.example.com", "page_age": "1 day ago" }]
            }]
        }).to_string())
    });
    let client = test_client(addr);
    let mut tool = WebSearchTool::default();
    let available = vec![
        NativeModelInfo { provider: "anthropic".into(), id: "claude-sonnet-4".into(), base_url: "https://anthropic.gateway.example.com/v1".into() },
        NativeModelInfo { provider: "openai".into(), id: "gpt-5.5".into(), base_url: "https://openai.gateway.example.com/v1".into() },
    ];
    let registry = ToolRegistry { available };
    let model = NativeModelInfo { provider: "openai".into(), id: "gpt-3.5".into(), base_url: "https://gateway.example.com/v1".into() };
    let loaded = ConfigLoadResult::Success { config: tool_config(true, None), source: "test".into() };
    let params = SearchParams { query: "discovered route".into(), allowed_domains: None, blocked_domains: None };
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tool.execute(&client, loaded, params, Some(WebSearchToolContext {
            model: Some(&model),
            model_registry: Some(&registry),
        }), None, None),
    ).await.expect("timeout").expect("execute");

    let paths = tokio::time::timeout(std::time::Duration::from_secs(5), server.join()).await.expect("server timeout");
    assert_eq!(paths, vec!["https://anthropic.gateway.example.com/v1/messages"]);
    let SearchRenderDetails::Result(details) = result.details else { panic!("result details expected") };
    assert_eq!(details.provider, SearchProvider::Anthropic);
    assert_eq!(details.entry_id.as_deref(), Some("native-anthropic-8bae5c53ca2e2eed"));
    assert_eq!(details.attempts.expect("attempts").iter().filter_map(|a| a.entry_id.as_deref()).collect::<Vec<_>>(), vec!["native-anthropic-8bae5c53ca2e2eed"]);
}

#[tokio::test]
async fn discovery_14_alias_cascade_dedupes_to_one_route_per_endpoint() {
    let (addr, server) = spawn_gateway_server(3, |_method, url| {
        if url.ends_with("/messages") || url.ends_with("/chat/completions") {
            (500, r#"{"error":"native failed"}"#.into())
        } else {
            (200, r#"{"results":[{"title":"Manual","url":"https://manual.example.com","text":"manual"}]}"#.into())
        }
    });
    let client = test_client(addr);
    let mut tool = WebSearchTool::default();
    let mut available = Vec::new();
    for i in 0..8 {
        available.push(NativeModelInfo {
            provider: "anthropic".into(),
            id: if i == 0 { "claude-opus-4".into() } else { format!("claude-opus-4-{i}") },
            base_url: "https://gateway.example.com/v1".into(),
        });
    }
    for i in 0..6 {
        available.push(NativeModelInfo {
            provider: "z-ai".into(),
            id: format!("glm-4.6-{i}"),
            base_url: "https://gateway.example.com/v1".into(),
        });
    }
    assert_eq!(available.len(), 14);
    let registry = ToolRegistry { available };
    let model = NativeModelInfo {
        provider: "openai".into(),
        id: "gpt-3.5".into(),
        base_url: "https://gateway.example.com/v1".into(),
    };
    let loaded = ConfigLoadResult::Success {
        config: tool_config(true, None),
        source: "test".into(),
    };
    let params = SearchParams {
        query: "discovery cascade".into(),
        allowed_domains: None,
        blocked_domains: None,
    };
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tool.execute(&client, loaded, params, Some(WebSearchToolContext {
            model: Some(&model),
            model_registry: Some(&registry),
        }), None, None),
    ).await.expect("timeout").expect("execute");

    let paths = tokio::time::timeout(std::time::Duration::from_secs(5), server.join()).await.expect("server timeout");
    assert_eq!(paths, vec![
        "https://gateway.example.com/v1/messages",
        "https://gateway.example.com/v1/chat/completions",
        "https://gateway.example.com/exa",
    ]);
    let SearchRenderDetails::Result(details) = result.details else { panic!("expected result details") };
    assert_eq!(details.provider, SearchProvider::Exa);
    assert_eq!(details.entry_id.as_deref(), Some("manual"));
    assert_eq!(details.attempts.expect("attempts").iter().filter_map(|a| a.entry_id.as_deref()).collect::<Vec<_>>(), vec![
        "native-anthropic-b9f0e3d4ac1da541",
        "native-z-ai-97ac6bcbbc83556b",
        "manual",
    ]);
}

#[tokio::test]
async fn active_native_model_and_duplicate_discovered_aliases_keeps_active_first() {
    let (addr, server) = spawn_gateway_server(3, |_method, url| {
        if url.ends_with("/responses") || url.ends_with("/messages") {
            (500, r#"{"error":"native failed"}"#.into())
        } else {
            (200, r#"{"results":[{"title":"Manual","url":"https://manual.example.com","text":"manual"}]}"#.into())
        }
    });
    let client = test_client(addr);
    let mut tool = WebSearchTool::default();
    let available = vec![
        NativeModelInfo { provider: "openai".into(), id: "gpt-4o-mini-2026-01-01".into(), base_url: "https://gateway.example.com/v1".into() },
        NativeModelInfo { provider: "openai".into(), id: "gpt-4.1-mini".into(), base_url: "https://gateway.example.com/v1".into() },
        NativeModelInfo { provider: "anthropic".into(), id: "claude-sonnet-4-5-20250929".into(), base_url: "https://gateway.example.com/v1".into() },
    ];
    let registry = ToolRegistry { available };
    let model = NativeModelInfo {
        provider: "openai".into(),
        id: "gpt-5.5".into(),
        base_url: "https://gateway.example.com/v1".into(),
    };
    let loaded = ConfigLoadResult::Success {
        config: tool_config(true, None),
        source: "test".into(),
    };
    let params = SearchParams {
        query: "active route".into(),
        allowed_domains: None,
        blocked_domains: None,
    };
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tool.execute(&client, loaded, params, Some(WebSearchToolContext {
            model: Some(&model),
            model_registry: Some(&registry),
        }), None, None),
    ).await.expect("timeout").expect("execute");

    let paths = tokio::time::timeout(std::time::Duration::from_secs(5), server.join()).await.expect("server timeout");
    assert_eq!(paths, vec![
        "https://gateway.example.com/v1/responses",
        "https://gateway.example.com/v1/messages",
        "https://gateway.example.com/exa",
    ]);
    let SearchRenderDetails::Result(details) = result.details else { panic!("expected result details") };
    assert_eq!(details.provider, SearchProvider::Exa);
    assert_eq!(details.entry_id.as_deref(), Some("manual"));
    assert_eq!(details.attempts.expect("attempts").iter().filter_map(|a| a.entry_id.as_deref()).collect::<Vec<_>>(), vec![
        "native",
        "native-anthropic-b9f0e3d4ac1da541",
        "manual",
    ]);
}
