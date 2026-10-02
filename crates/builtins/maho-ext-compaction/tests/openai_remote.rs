use maho_ai::types::Model;
use maho_ext_compaction::openai_remote::*;
use serde_json::json;

#[tokio::test]
async fn websocket_compaction_keeps_leading_prompt_and_retains_user_input_only() {
    let model = serde_json::from_value(json!({"id":"m","name":"m","provider":"openai","api":"openai-responses","baseUrl":"https://api.openai.com/v1","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":10000,"maxTokens":8000})).unwrap();
    let request = OpenAiRemoteCompactionRequest {body:json!({"model":"m","input":[{"role":"user","content":"task"},{"type":"function_call","call_id":"discard"}],"prompt_cache_key":"session","service_tier":"priority"}),input_item_count:2,tokens_before:123};
    let runner: maho_ext_compaction::openai_remote_dependencies::OpenAiResponsesStreamRunner = std::sync::Arc::new(|model,context,options| {
        assert!(context.messages.is_empty());
        if options.stream.transport == Some(maho_ai::types::Transport::Sse) {
            let stream = maho_ai::utils::event_stream::create_assistant_message_event_stream();
            stream.push(maho_ai::types::AssistantMessageEvent::Error {reason:maho_ai::types::ErrorReason::Error,error:maho_ai::utils::lazy::setup_error_message(model,"v2 unavailable")});
            return stream;
        }
        assert_eq!(options.stream.transport,Some(maho_ai::types::Transport::Websocket));
        let payload = options.stream.request.on_payload.as_ref().unwrap()(&json!({"model":"wrong","input":[{"role":"system","content":"instructions"},{"role":"user","content":"discard"}]}),model,None).unwrap();
        assert_eq!(payload["model"],"m");
        assert_eq!(payload["input"][0]["role"],"system");
        assert_eq!(payload["input"][3]["type"],"context_compaction");
        assert_eq!(payload["service_tier"],"priority");
        let stream = maho_ai::utils::event_stream::create_assistant_message_event_stream();
        let mut response = maho_ai::utils::lazy::setup_error_message(model,"");
        response.stop_reason = maho_ai::types::StopReason::Stop;
        response.timestamp = 5000;
        response.content = serde_json::from_value(json!([{"type":"providerNative","subtype":"openai_compaction","raw":{"type":"context_compaction","encrypted_content":"opaque"}}])).unwrap();
        stream.push(maho_ai::types::AssistantMessageEvent::Done {reason:maho_ai::types::DoneReason::Stop,message:response});
        stream
    });
    let controller = maho_ai::utils::abort::AbortController::new();
    let result = run_openai_responses_stream_compaction(maho_ext_compaction::openai_remote_responses_v2::ResponsesV2Options {model:&model,request:&request,first_kept_entry_id:"anchor",origin:json!({}),system_prompt:"system".into(),session_id:"session".into(),api_key:Some("faux".into()),headers:Default::default(),extra_body:None,signal:controller.signal(),runner:&runner},42).await.unwrap().unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["transport"],"websocket");
    assert_eq!(details["retainedInputItemCount"],2);
    assert_eq!(details["replacementInput"][0]["role"],"user");
    assert_eq!(details["replacementInput"][1]["type"],"context_compaction");
    assert_eq!(details["responseId"],"response-42");
    let events = std::sync::Mutex::new(Vec::new());
    let result = run_remote_compaction(RemoteCompactionOptions {
        model:&model,request:&request,request_id:"route",first_kept_entry_id:"anchor",system_prompt:"system",session_id:"session",api_key:Some("faux".into()),headers:Default::default(),extra_body:None,origin:json!({}),signal:&controller.signal(),timeout:std::time::Duration::from_secs(5),now_ms:42,client:&reqwest::Client::new(),runner:&runner,provider_request:None,
    },&|event|events.lock().unwrap().push(event)).await.unwrap().unwrap();
    assert_eq!(result.details.unwrap()["transport"],"websocket");
    let events = events.lock().unwrap();
    assert_eq!(events[0]["transport"],"responses-v2");
    assert_eq!(events[1]["reason"],"responses-v2-missing-compaction-output");
    assert_eq!(events[2]["transport"],"websocket");
    assert_eq!(events[3]["action"],"remote_completed");
}

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

#[tokio::test]
async fn caller_abort_interrupts_incomplete_response_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (ready_sender, ready_receiver) = tokio::sync::oneshot::channel();
    let (finish_sender, finish_receiver) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).await.unwrap();
        assert!(count > 0);
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1000\r\n\r\n{").await.unwrap();
        ready_sender.send(()).unwrap();
        finish_receiver.await.unwrap();
    });
    let model: Model = serde_json::from_value(json!({"id":"m","name":"m","provider":"openai","api":"openai-responses","baseUrl":format!("http://{address}/v1"),"reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":10000,"maxTokens":8000})).unwrap();
    let messages = [json!({"role":"user","content":"task","timestamp":0})];
    let request = create_openai_remote_compaction_request(Some(&model), "", &[], Some(&messages), 999, None, None).unwrap();
    let controller = maho_ai::utils::abort::AbortController::new();
    let signal = controller.signal();
    let client = reqwest::Client::new();
    let operation = run_openai_compact_endpoint_compaction(CompactEndpointOptions { client: &client, headers: reqwest::header::HeaderMap::new(), model: &model, request: &request, request_id: "request", signal: &signal, first_kept_entry_id: "anchor", now_ms: 0, origin: json!({}) }, &|_| {});
    let abort = async { ready_receiver.await.unwrap(); controller.abort(None); };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async { tokio::join!(operation, abort) }).await.unwrap();
    assert_eq!(result.unwrap_err(), "aborted");
    finish_sender.send(()).unwrap();
    server.await.unwrap();
}
