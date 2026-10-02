use std::collections::BTreeMap;
use maho_ext_compaction::openai_remote_responses_v2::*;
use serde_json::json;

#[test]
fn beta_header_normalizes_case_and_deduplicates_tokens() {
    let headers = BTreeMap::from([("X-Codex-Beta-Features".into(), " alpha,alpha, remote_compaction_v2 ".into()), ("authorization".into(), "Bearer credential".into())]);
    let next = with_remote_compaction_v2_header(&headers);
    assert_eq!(next["x-codex-beta-features"], "alpha,remote_compaction_v2");
    assert_eq!(next["authorization"], headers["authorization"]);
    assert!(!next.contains_key("X-Codex-Beta-Features"));
}

#[test]
fn payload_rewrite_keeps_options_and_appends_native_trigger() {
    let input = [json!({"role":"user","content":"request"})];
    let payload = rewrite_v2_payload(&json!({"model":"m","input":[]}), &input).unwrap();
    assert_eq!(payload["model"], "m");
    assert_eq!(payload["input"][0], input[0]);
    assert!(has_v2_trigger(&payload));
}

#[test]
fn malformed_payload_and_removed_trigger_are_rejected() {
    assert!(rewrite_v2_payload(&json!(null), &[]).is_none());
    assert!(!has_v2_trigger(&json!({"input":[{"type":"message"}]})));
}

#[tokio::test]
async fn v2_stream_runner_receives_trigger_and_extracts_native_checkpoint() {
    use maho_ai::types::{Model, StopReason};
    let model: Model = serde_json::from_value(json!({"id":"m","name":"m","provider":"openai","api":"openai-responses","baseUrl":"https://api.openai.com/v1","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":10000,"maxTokens":8000})).unwrap();
    let request = maho_ext_compaction::openai_remote::OpenAiRemoteCompactionRequest { body: json!({"model":"m","input":[{"role":"user","content":"task"}]}), input_item_count: 1, tokens_before: 123 };
    let runner: maho_ext_compaction::openai_remote_dependencies::OpenAiResponsesStreamRunner = std::sync::Arc::new(|model, context, options| {
        assert!(context.messages.is_empty());
        let transform = options.stream.request.on_payload.as_ref().unwrap();
        let payload = transform(&json!({"model":"m","input":[]}), model, None).unwrap();
        assert!(has_v2_trigger(&payload));
        assert_eq!(options.stream.transport, Some(maho_ai::types::Transport::Sse));
        let stream = maho_ai::utils::event_stream::create_assistant_message_event_stream();
        let mut response = maho_ai::utils::lazy::setup_error_message(model, "");
        response.stop_reason = StopReason::Stop;
        response.timestamp = 5000;
        response.content = serde_json::from_value(json!([{"type":"providerNative","subtype":"openai_compaction","raw":{"type":"compaction","id":"native","encrypted_content":"opaque"}}])).unwrap();
        stream.push(maho_ai::types::AssistantMessageEvent::Done { reason: maho_ai::types::DoneReason::Stop, message: response });
        stream
    });
    let controller = maho_ai::utils::abort::AbortController::new();
    let result = run_openai_responses_v2_compaction(ResponsesV2Options { model: &model, request: &request, first_kept_entry_id: "anchor", origin: json!({}), system_prompt: "system".into(), session_id: "session".into(), api_key: Some("faux".into()), headers: BTreeMap::new(), extra_body: None, signal: controller.signal(), runner: &runner }).await.unwrap().unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["responseId"], "native");
    assert_eq!(details["createdAt"], 5);
    assert_eq!(details["transport"], "responses-v2");
    assert_eq!(result.first_kept_entry_id, "anchor");
}

#[tokio::test(start_paused = true)]
async fn v2_timeout_emits_both_upstream_fallback_events() {
    let model = serde_json::from_value(json!({"id":"m","name":"m","provider":"openai","api":"openai-responses","baseUrl":"https://api.openai.com/v1","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":10000,"maxTokens":8000})).unwrap();
    let request = maho_ext_compaction::openai_remote::OpenAiRemoteCompactionRequest { body: json!({"input":[]}), input_item_count: 0, tokens_before: 123 };
    let runner: maho_ext_compaction::openai_remote_dependencies::OpenAiResponsesStreamRunner = std::sync::Arc::new(|_, _, _| maho_ai::utils::event_stream::create_assistant_message_event_stream());
    let controller = maho_ai::utils::abort::AbortController::new();
    let events = std::sync::Mutex::new(Vec::new());
    let result = attempt_openai_responses_v2_compaction(ResponsesV2Options { model: &model, request: &request, first_kept_entry_id: "anchor", origin: json!({}), system_prompt: String::new(), session_id: "session".into(), api_key: None, headers: BTreeMap::new(), extra_body: None, signal: controller.signal(), runner: &runner }, "request", std::time::Duration::from_secs(1), &|event| events.lock().unwrap().push(event)).await.unwrap();
    assert!(result.is_none());
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0]["action"], "remote_started");
    assert_eq!(events[1]["reason"], "remote-compaction-timeout");
    assert_eq!(events[2]["reason"], "responses-v2-missing-compaction-output");
    assert!(!controller.signal().aborted());
}
