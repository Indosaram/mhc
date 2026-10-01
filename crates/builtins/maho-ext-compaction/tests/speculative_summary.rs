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
