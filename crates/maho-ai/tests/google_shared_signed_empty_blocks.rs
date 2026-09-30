//! Port of senpi packages/ai/test/google-shared-signed-empty-blocks.test.ts.

mod google_fixtures;

use google_fixtures::{
    assistant, context, text, text_model, text_with_signature, thinking, thinking_with_signature, tool_call, user_text,
};
use maho_ai::api::google_shared::{convert_messages, ConvertMessagesOptions};
use maho_ai::types::{ContentBlock, Model, StopReason};
use serde_json::{json, Value};

const VALID_SIG: &str = "AAAAAAAAAAAAAAAAAAAAAA==";

fn model(id: &str) -> Model {
    text_model("google-generative-ai", "google", id)
}

fn contents_for(model: &Model, assistant_model_id: &str, content: Vec<ContentBlock>) -> Value {
    let mut assistant_model = model.clone();
    assistant_model.id = assistant_model_id.to_owned();
    serde_json::to_value(convert_messages(
        model,
        &context(vec![user_text("Hi"), assistant(&assistant_model, content, StopReason::ToolUse)]),
        ConvertMessagesOptions::default(),
    ))
    .expect("contents serialize")
}

fn model_turn(contents: &Value) -> Value {
    contents
        .as_array()
        .expect("contents is an array")
        .iter()
        .find(|content| content["role"] == json!("model"))
        .cloned()
        .expect("a model turn")
}

fn signed_parts(turn: &Value) -> Vec<Value> {
    turn["parts"]
        .as_array()
        .expect("parts is an array")
        .iter()
        .filter(|part| part["thoughtSignature"] == json!(VALID_SIG))
        .cloned()
        .collect()
}

#[test]
fn keeps_a_signed_empty_thinking_block_so_its_signature_is_echoed_back() {
    let model = model("gemini-3-pro-preview");
    let contents = contents_for(
        &model,
        "gemini-3-pro-preview",
        vec![
            thinking_with_signature("", VALID_SIG),
            tool_call("call_1", "bash", json!({ "command": "ls" })),
        ],
    );

    let signed = signed_parts(&model_turn(&contents));
    assert_eq!(signed.len(), 1);
    assert_eq!(signed[0]["thought"], json!(true));
}

#[test]
fn keeps_a_signed_empty_text_block_the_same_way() {
    let model = model("gemini-3-pro-preview");
    let contents = contents_for(
        &model,
        "gemini-3-pro-preview",
        vec![
            text_with_signature("", VALID_SIG),
            tool_call("call_1", "bash", json!({ "command": "ls" })),
        ],
    );

    assert_eq!(signed_parts(&model_turn(&contents)).len(), 1);
}

#[test]
fn still_drops_unsigned_empty_blocks() {
    let model = model("gemini-3-pro-preview");
    let contents = contents_for(
        &model,
        "gemini-3-pro-preview",
        vec![
            thinking(""),
            text("   "),
            tool_call("call_1", "bash", json!({ "command": "ls" })),
        ],
    );

    let turn = model_turn(&contents);
    assert_eq!(turn["parts"].as_array().expect("parts is an array").len(), 1);
    assert!(turn["parts"][0].get("functionCall").is_some());
}

#[test]
fn still_drops_signed_empty_blocks_from_a_different_provider_or_model() {
    let model = model("gemini-3-pro-preview");
    let contents = contents_for(
        &model,
        "other-model",
        vec![
            thinking_with_signature("", VALID_SIG),
            text_with_signature("", VALID_SIG),
            tool_call("call_1", "bash", json!({ "command": "ls" })),
        ],
    );

    let turn = model_turn(&contents);
    assert_eq!(turn["parts"].as_array().expect("parts is an array").len(), 1);
    assert!(turn["parts"][0].get("functionCall").is_some());
    assert!(!turn.to_string().contains(VALID_SIG));
}
