use maho_ai::types::Model;
use maho_ext_compaction::openai_remote::*;
use serde_json::json;

#[test]
fn compact_request_and_result_preserve_machine_consumed_checkpoint_fields() {
    let model: Model = serde_json::from_value(json!({"id":"m","name":"m","provider":"openai","api":"openai-responses","baseUrl":"https://api.openai.com/v1","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":10000,"maxTokens":8000})).unwrap();
    let messages = [json!({"role":"user","content":"task","timestamp":0})];
    let request = create_openai_remote_compaction_request(Some(&model), "system", &[], Some(&messages), 999, Some("cache"), Some("priority")).unwrap();
    assert_eq!(request.body["prompt_cache_key"], "cache");
    assert_eq!(request.body["instructions"], "system");
    let response = parse_openai_compacted_response(&json!({"id":"r","object":"response.compaction","created_at":5,"output":[{"type":"compaction","encrypted_content":"opaque"},{"type":"function_call","call_id":"discard"}]}), &model, "request", 0).unwrap();
    let result = build_openai_remote_compaction_result(&model, "anchor", &request, &response, None).unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["retainedInputItemCount"], 1);
    assert_eq!(details["responseId"], "r");
    assert_eq!(result.first_kept_entry_id, "anchor");
    assert_eq!(result.tokens_before, 999);
}

#[tokio::test]
async fn compact_transport_posts_to_real_local_http_surface() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            request.extend_from_slice(&buffer[..count]);
            if let Some(boundary) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..boundary]);
                let length: usize = headers.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_owned)).unwrap().parse().unwrap();
                if request.len() >= boundary + 4 + length { break; }
            }
        }
        let text = String::from_utf8(request).unwrap();
        assert!(text.starts_with("POST /v1/responses/compact HTTP/1.1"));
        assert!(text.contains("\"model\":\"m\""));
        let body = json!({"id":"r","object":"response.compaction","created_at":5,"output":[{"type":"compaction","encrypted_content":"opaque"}]}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    let model: Model = serde_json::from_value(json!({"id":"m","name":"m","provider":"openai","api":"openai-responses","baseUrl":format!("http://{address}/v1"),"reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":10000,"maxTokens":8000})).unwrap();
    let messages = [json!({"role":"user","content":"task","timestamp":0})];
    let request = create_openai_remote_compaction_request(Some(&model), "system", &[], Some(&messages), 999, None, None).unwrap();
    let controller = maho_ai::utils::abort::AbortController::new();
    let events = std::sync::Mutex::new(Vec::new());
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), run_openai_compact_endpoint_compaction(CompactEndpointOptions {
        client: &reqwest::Client::new(), headers: reqwest::header::HeaderMap::new(), model: &model, request: &request, request_id: "request", signal: &controller.signal(), first_kept_entry_id: "anchor", now_ms: 0, origin: json!({"credentialSource":"api-key","credentialFingerprint":"fingerprint"}),
    }, &|event| events.lock().unwrap().push(event))).await.unwrap().unwrap().unwrap();
    server.await.unwrap();
    assert_eq!(result.first_kept_entry_id, "anchor");
    let events = events.lock().unwrap();
    assert_eq!(events[0]["action"], "remote_started");
    assert_eq!(events[1]["action"], "remote_completed");
}
