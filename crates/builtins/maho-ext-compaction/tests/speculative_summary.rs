use maho_ai::types::Model;
use maho_ext_compaction::speculative_summary::*;
use serde_json::json;

#[test]
fn summary_joins_only_text_blocks() {
    let message = json!({"role":"assistant","content":[{"type":"thinking","thinking":"private"},{"type":"text","text":" first "},{"type":"text","text":"second "}],"stopReason":"stop"});
    assert_eq!(get_summary_text(&message), "first \nsecond");
    assert!(is_assistant_message(&message));
    assert!(!is_assistant_message(&json!({"role":"assistant"})));
}

#[test]
fn summary_reserve_and_disabled_reasoning_are_bounded() {
    let model: Model = serde_json::from_value(json!({"id":"m","name":"m","provider":"custom","api":"openai-responses","baseUrl":"https://example.com","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":10000,"maxTokens":8000})).unwrap();
    assert_eq!(summary_max_tokens(&model, 10000), 5000);
    assert!(!has_summarization_reasoning_override(&model));
}

#[test]
fn reasoning_chooses_first_non_null_supported_effort() {
    let model: Model = serde_json::from_value(json!({"id":"m","name":"m","provider":"custom","api":"openai-responses","baseUrl":"https://example.com","reasoning":true,"thinkingLevelMap":{"low":null,"medium":"mid"},"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":10000,"maxTokens":8000})).unwrap();
    assert_eq!(summarization_reasoning_options(&model), json!({"reasoningEffort":"medium","reasoningSummary":null}));
}

#[tokio::test(start_paused = true)]
async fn stream_ending_without_terminal_result_still_has_watchdog() {
    let stream = maho_ai::utils::event_stream::create_assistant_message_event_stream();
    stream.end(None);
    let controller = maho_ai::utils::abort::AbortController::new();
    let result = consume_summary_stream(&stream, &controller, None, std::time::Duration::from_secs(1), std::time::Duration::from_secs(5), &|_| {}).await;
    assert!(matches!(result, Err(SummaryStreamError::IdleTimeout)));
    assert!(controller.signal().aborted());
}

#[tokio::test(start_paused = true)]
async fn caller_abort_stands_down_without_timeout_failure() {
    let stream = maho_ai::utils::event_stream::create_assistant_message_event_stream();
    let controller = maho_ai::utils::abort::AbortController::new();
    let caller = maho_ai::utils::abort::AbortController::new();
    caller.abort(None);
    let result = consume_summary_stream(&stream, &controller, Some(&caller.signal()), std::time::Duration::from_secs(1), std::time::Duration::from_secs(5), &|_| {}).await;
    assert!(result.unwrap().is_none());
    assert!(controller.signal().aborted());
}

#[tokio::test(start_paused = true)]
async fn wall_clock_budget_can_expire_before_idle_budget() {
    let stream = maho_ai::utils::event_stream::create_assistant_message_event_stream();
    let controller = maho_ai::utils::abort::AbortController::new();
    let result = consume_summary_stream(&stream, &controller, None, std::time::Duration::from_secs(5), std::time::Duration::from_secs(1), &|_| {}).await;
    assert!(matches!(result, Err(SummaryStreamError::DurationBudget)));
}

#[tokio::test]
async fn summary_dispatch_uses_native_messages_and_supplied_runtime_runner() {
    use maho_core::compaction::{compaction::prepare_compaction, settings::default_compaction_settings};
    let model: Model = serde_json::from_value(json!({"id":"m","name":"m","provider":"extension-provider","api":"extension-api","baseUrl":"https://example.com","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":10000,"maxTokens":8000})).unwrap();
    let entries = [json!({"type":"message","id":"old","parentId":null,"timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"old ".repeat(500),"timestamp":0}}), json!({"type":"message","id":"keep","parentId":"old","timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"continue task","timestamp":0}})];
    let mut settings = default_compaction_settings(); settings.keep_recent_tokens = 1;
    let preparation = prepare_compaction(&entries, &settings, true, false).unwrap();
    let snapshot = maho_ext_compaction::speculative::SpeculativeCompactionSnapshot { generation: 1, expected_revision: 1, model, context_window: 10000, preparation, branch_entries: entries.to_vec(), prompt_variant: maho_ext_compaction::prompts::PromptVariant::Default, custom_instructions: None, system_prompt: Some("agent prompt".into()), tools: Vec::new() };
    let runner: SummaryStreamRunner = std::sync::Arc::new(|model, context, options| {
        assert_eq!(model.provider, "extension-provider");
        assert_eq!(context.system_prompt.as_deref(), Some("agent prompt"));
        assert_eq!(context.messages.len(), 2);
        assert_eq!(options.max_tokens, Some(5000));
        assert_eq!(options.extra["toolChoice"], "none");
        let stream = maho_ai::utils::event_stream::create_assistant_message_event_stream();
        let mut response = maho_ai::utils::lazy::setup_error_message(model, "");
        response.stop_reason = maho_ai::types::StopReason::Stop;
        response.error_message = None;
        response.content = vec![maho_ai::types::ContentBlock::Text(maho_ai::types::TextContent { text: "summary".into(), text_signature: None, audience: None })];
        stream.push(maho_ai::types::AssistantMessageEvent::Done { reason: maho_ai::types::DoneReason::Stop, message: response });
        stream
    });
    let messages = [json!({"role":"user","content":"task","timestamp":0})];
    let prompt = maho_ext_compaction::prompts::CompactionPrompt { system: "summary prompt", user: "summarize".into() };
    let result = generate_summary_message(SummaryRequestOptions { snapshot: &snapshot, messages: &messages, prompt: &prompt, api_key: Some("faux".into()), headers: None, extra_body: None, signal: None, max_duration: std::time::Duration::from_secs(5), omit_reasoning_options: false, forbid_tool_calls: true, stream_runner: Some(&runner) }, &|_| {}).await.unwrap().unwrap();
    assert_eq!(result.stop_reason, maho_ai::types::StopReason::Stop);
}

#[tokio::test]
async fn generated_compaction_extracts_task_intent_and_structural_metadata() {
    use maho_core::compaction::{compaction::prepare_compaction, settings::default_compaction_settings};
    let model: Model = serde_json::from_value(json!({"id":"m","name":"m","provider":"faux","api":"faux","baseUrl":"https://example.com","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":10000,"maxTokens":8000})).unwrap();
    let entries = [json!({"type":"message","id":"old","parentId":null,"timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"old ".repeat(500),"timestamp":0}}), json!({"type":"message","id":"keep","parentId":"old","timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"continue task","timestamp":0}})];
    let mut settings = default_compaction_settings(); settings.keep_recent_tokens = 1;
    let preparation = prepare_compaction(&entries, &settings, true, false).unwrap();
    let snapshot = maho_ext_compaction::speculative::SpeculativeCompactionSnapshot { generation: 1, expected_revision: 1, model, context_window: 10000, preparation, branch_entries: entries.to_vec(), prompt_variant: maho_ext_compaction::prompts::PromptVariant::Default, custom_instructions: None, system_prompt: None, tools: Vec::new() };
    let runner: SummaryStreamRunner = std::sync::Arc::new(|model, _, _| {
        let stream = maho_ai::utils::event_stream::create_assistant_message_event_stream();
        let mut response = maho_ai::utils::lazy::setup_error_message(model, "");
        response.stop_reason = maho_ai::types::StopReason::Stop;
        response.content = serde_json::from_value(json!([{"type":"text","text":"<task-intent>original task</task-intent><summary>continued work</summary>"}])).unwrap();
        stream.push(maho_ai::types::AssistantMessageEvent::Done { reason: maho_ai::types::DoneReason::Stop, message: response });
        stream
    });
    let result = maho_ext_compaction::speculative::run_extension_compaction(&snapshot, Some("faux".into()), None, None, Some(&runner), &|_| {}).await.unwrap().unwrap();
    assert_eq!(result.summary, "continued work");
    let details = result.details.unwrap();
    assert_eq!(details["taskIntent"], "original task");
    assert_eq!(details["schema"], "senpi.compaction.summary.v1");
    assert!(details["structuralYield"]["savedTokens"].as_f64().unwrap() > 0.);
}
