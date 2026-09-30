//! Port of senpi packages/ai/test/google-shared-tool-call-id.test.ts.

mod google_fixtures;

use google_fixtures::{assistant, context, text, text_model, tool_call, tool_result, user_text};
use maho_ai::api::google_shared::{convert_messages, ConvertMessagesOptions};
use maho_ai::types::StopReason;
use serde_json::{json, Value};
use std::collections::BTreeSet;

#[test]
fn never_collides_when_truncating_long_foreign_tool_call_ids_sharing_a_64_char_prefix() {
    // makeClaudeViaGoogleModel(): Claude models behind Google APIs require explicit tool call IDs.
    let mut model = text_model("google-generative-ai", "google", "claude-sonnet-4-5");
    model.name = "Claude Sonnet 4.5".to_owned();
    model.context_window = 200_000;
    model.max_tokens = 8192;
    let mut foreign = google_fixtures::builtin_model("moonshotai", "kimi-k2.6");
    foreign.api = "openai-completions".to_owned();
    foreign.provider = "moonshot".to_owned();

    let shared_prefix = format!("call_{}", "A".repeat(200));
    let first = format!("{shared_prefix}1111");
    let second = format!("{shared_prefix}2222");
    let contents = serde_json::to_value(convert_messages(
        &model,
        &context(vec![
            user_text("run tools"),
            assistant(
                &foreign,
                vec![tool_call(&first, "bash", json!({})), tool_call(&second, "read", json!({}))],
                StopReason::ToolUse,
            ),
            tool_result(&first, "bash", vec![text("ok")], false),
            tool_result(&second, "read", vec![text("ok")], false),
        ]),
        ConvertMessagesOptions::default(),
    ))
    .expect("contents serialize");

    let items = contents.as_array().expect("contents is an array");
    let model_turn = items.iter().find(|content| content["role"] == json!("model")).expect("a model turn");
    let call_ids: Vec<String> = model_turn["parts"]
        .as_array()
        .expect("parts is an array")
        .iter()
        .filter_map(|part| part["functionCall"]["id"].as_str().map(str::to_owned))
        .collect();
    let response_ids: Vec<String> = items
        .iter()
        .filter(|content| content["role"] == json!("user"))
        .flat_map(|content| content["parts"].as_array().cloned().unwrap_or_default())
        .filter_map(|part| part["functionResponse"]["id"].as_str().map(str::to_owned))
        .collect();

    assert_eq!(call_ids.len(), 2);
    assert_eq!(call_ids.iter().collect::<BTreeSet<_>>().len(), 2, "tool call ids must not collide");
    assert_eq!(response_ids.iter().collect::<BTreeSet<_>>(), call_ids.iter().collect::<BTreeSet<_>>());
    for id in &call_ids {
        assert!(id.len() <= 64, "{id} must not exceed 64 characters");
        assert!(
            id.chars().all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '-'),
            "{id} must match the tool call id pattern"
        );
    }
    let _: Value = contents;
}
