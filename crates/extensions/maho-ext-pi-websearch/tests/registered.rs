mod support;
use maho_ext_api::*;
use maho_ext_pi_websearch::websearch::{tool::register_search_tool, types::*};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn api() -> ExtensionApi {
    ExtensionApi::new(LoadedExtension::new("pi-websearch", "/fixture".into(), Default::default()), Default::default(), Default::default(), Default::default())
}

#[tokio::test]
async fn registered_search_keeps_routing_progress_and_result_contract() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(async move {
        let mut paths = Vec::new();
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 4096];
                let count = socket.read(&mut buffer).await.expect("request");
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") { break; }
            }
            paths.push(String::from_utf8(request).expect("HTTP").lines().next().expect("request line").split_whitespace().nth(1).expect("path").to_owned());
            let body = r#"{"results":[{"title":"Manual","url":"https://manual.example.com"}]}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.expect("response");
        }
        paths
    });
    let config = WebsearchConfig { strategy: RoutingStrategy::RoundRobin, fallback: true, auto: false,
        providers: ["one", "two"].into_iter().map(|id| serde_json::from_value(json!({"provider":"exa","id":id,"baseUrl":format!("http://{address}/{id}"),"maxResults":4})).expect("provider")).collect() };
    let mut api = api();
    register_search_tool(&mut api, Arc::new(Mutex::new(ConfigLoadResult::Success { config, source: "fixture".into() })), None).expect("registration");
    let execute = api.runtime.extension_tool_executor("pi-websearch", "web_search").expect("executor");
    let updates = Arc::new(Mutex::new(Vec::new()));
    let mut results = Vec::new();
    for expected in ["one", "two"] {
        let updates = Arc::clone(&updates);
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), execute("qa", json!({"query":"manual docs"}), None,
            Some(Arc::new(move |update| updates.lock().expect("progress").push(update))), &support::context())).await;
        let failed = !matches!(&result, Ok(Ok(_)));
        results.push((expected, result));
        if failed { break; }
    }
    let server = tokio::time::timeout(std::time::Duration::from_secs(5), servers.join_next()).await;
    if server.is_err() { servers.abort_all(); while servers.join_next().await.is_some() {} }
    for (expected, result) in results {
        let result = result.expect("request deadline").expect("execute");
        assert_eq!(result.details["entryId"], expected);
        assert_eq!(result.details["results"][0]["url"], "https://manual.example.com");
        assert!(matches!(&result.content[0], ContentBlock::Text(text) if text.text.contains("https://manual.example.com")));
    }
    let paths = server.expect("server deadline").expect("server task").expect("server");
    assert_eq!(paths, ["/one", "/two"]);
    assert!(std::net::TcpListener::bind(address).is_ok());
    let updates = updates.lock().expect("progress");
    assert_eq!(updates.len(), 4);
    assert_eq!(updates[0].details["phase"], "searching");
    assert_eq!(updates[3].details["currentProvider"], "exa/two");
}

#[tokio::test]
async fn registered_abort_preserves_custom_reason_and_disconnects() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let (sent, accepted) = tokio::sync::oneshot::channel();
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut bytes = [0; 4096];
        assert!(socket.read(&mut bytes).await.expect("request") > 0);
        sent.send(()).expect("event");
        assert_eq!(socket.read(&mut bytes).await.expect("EOF"), 0);
    });
    let config = serde_json::from_value(json!({"strategy":"priority","fallback":true,"auto":false,
        "providers":[{"provider":"exa","baseUrl":format!("http://{address}")}]})).expect("config");
    let mut api = api();
    register_search_tool(&mut api, Arc::new(Mutex::new(ConfigLoadResult::Success { config, source: "fixture".into() })), None).expect("registration");
    let execute = api.runtime.extension_tool_executor("pi-websearch", "web_search").expect("executor");
    let controller = maho_ai::utils::abort::AbortController::new();
    let signal = controller.signal();
    let mut request = Box::pin(async { execute("qa", json!({"query":"manual docs"}), Some(signal), None, &support::context()).await });
    let accepted_result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! { event = accepted => event.map_err(|error| error.to_string()), result = &mut request => Err(format!("request ended before acceptance: {result:?}")) }
    }).await;
    controller.abort(Some(maho_ai::utils::abort::AbortReason::new("Error", "cancel registered search")));
    let result = if matches!(&accepted_result, Ok(Ok(()))) {
        Some(tokio::time::timeout(std::time::Duration::from_secs(5), &mut request).await)
    } else { None };
    drop(request);
    let server = tokio::time::timeout(std::time::Duration::from_secs(5), tasks.join_next()).await;
    if server.is_err() { tasks.abort_all(); while tasks.join_next().await.is_some() {} }
    accepted_result.expect("accept deadline").expect("acceptance event");
    assert_eq!(result.expect("accepted request").expect("request deadline").expect_err("abort").message, "cancel registered search");
    server.expect("server deadline").expect("server task").expect("server");
    assert!(std::net::TcpListener::bind(address).is_ok());
}

#[test]
fn native_factory_registers_search_executor_and_renderers() {
    let loaded = maho_ext_host::loader::load_extensions(vec![maho_ext_host::loader::NativeExtensionFactory {
        path: "pi-websearch".into(), source_info: SourceInfo::default(), extension: Box::new(maho_ext_pi_websearch::WebsearchExtension { native_registry: None })
    }], std::path::Path::new("/fixture"), Default::default());
    assert!(loaded.errors.is_empty());
    assert!(loaded.runtime.extension_tool_executor("pi-websearch", "web_search").is_some());
    assert_eq!(loaded.extensions[0].tools[0].definition.name, "web_search");
    assert!(loaded.extensions[0].tool_renderers.contains_key("web_search"));
}
