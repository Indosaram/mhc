//! Port of senpi packages/ai/test/google-shared-gemini3-unsigned-tool-call.test.ts.

mod google_fixtures;

use google_fixtures::{
    assistant, context, text, text_model, text_with_signature, thinking_with_signature, tool_call,
    tool_call_with_signature, tool_result, user_text,
};
use maho_ai::api::google_shared::{convert_messages, requires_tool_call_id, ConvertMessagesOptions};
use maho_ai::types::{Context, Model, StopReason};
use serde_json::{json, Value};

const VALID_SIG: &str = "AAAAAAAAAAAAAAAAAAAAAA==";
const TEXT_SIG: &str = "BBBBBBBBBBBBBBBBBBBBBB==";

fn make_context(model: &Model, thought_signature: Option<&str>) -> Context {
    let first_call = match thought_signature {
        Some(signature) => tool_call_with_signature("call_1", "bash", json!({ "command": "echo hi" }), signature),
        None => tool_call("call_1", "bash", json!({ "command": "echo hi" })),
    };
    context(vec![
        user_text("Hi"),
        assistant(
            model,
            vec![first_call, tool_call("call_2", "bash", json!({ "command": "ls -la" }))],
            StopReason::ToolUse,
        ),
        tool_result("call_1", "bash", vec![text("hi")], false),
        tool_result("call_2", "bash", vec![text("files")], false),
    ])
}

fn convert(model: &Model, context: &Context, preserve_thinking: Option<bool>) -> Value {
    serde_json::to_value(convert_messages(model, context, ConvertMessagesOptions { preserve_thinking }))
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

fn function_call_parts(contents: &Value) -> Vec<Value> {
    model_turn(contents)["parts"]
        .as_array()
        .expect("parts is an array")
        .iter()
        .filter(|part| part.get("functionCall").is_some())
        .cloned()
        .collect()
}

fn function_response_parts(contents: &Value) -> Vec<Value> {
    contents
        .as_array()
        .expect("contents is an array")
        .iter()
        .flat_map(|content| content["parts"].as_array().cloned().unwrap_or_default())
        .filter(|part| part.get("functionResponse").is_some())
        .collect()
}

fn assert_tool_call_ids_are_preserved(api: &str, id: &str) {
    let model = text_model(api, if api == "google-vertex" { "google-vertex" } else { "google" }, id);
    let contents = convert(&model, &make_context(&model, None), None);

    let calls = function_call_parts(&contents);
    let responses = function_response_parts(&contents);
    let call_ids: Vec<&str> = calls.iter().filter_map(|part| part["functionCall"]["id"].as_str()).collect();
    let response_ids: Vec<&str> =
        responses.iter().filter_map(|part| part["functionResponse"]["id"].as_str()).collect();
    assert_eq!(call_ids, vec!["call_1", "call_2"]);
    assert_eq!(response_ids, vec!["call_1", "call_2"]);
}

#[test]
fn preserves_tool_call_ids_for_gemini_3_pro_preview_via_google_generative_ai_history() {
    assert_tool_call_ids_are_preserved("google-generative-ai", "gemini-3-pro-preview");
}

#[test]
fn preserves_tool_call_ids_for_gemini_3_6_flash_via_google_generative_ai_history() {
    assert_tool_call_ids_are_preserved("google-generative-ai", "gemini-3.6-flash");
}

#[test]
fn preserves_tool_call_ids_for_gemini_3_pro_preview_via_google_vertex_history() {
    assert_tool_call_ids_are_preserved("google-vertex", "gemini-3-pro-preview");
}

#[test]
fn does_not_add_skip_thought_signature_validator_for_unsigned_google_gen_ai_tool_calls() {
    let model = text_model("google-generative-ai", "google", "gemini-3-pro-preview");
    let mut other = model.clone();
    other.id = "other-model".to_owned();
    let contents = convert(&model, &make_context(&other, None), None);

    let calls = function_call_parts(&contents);
    assert_eq!(calls.len(), 2);
    assert!(calls[0].get("thoughtSignature").is_none());
    assert!(calls[1].get("thoughtSignature").is_none());
    assert!(!contents.to_string().contains("skip_thought_signature_validator"));

    let historical: Vec<Value> = model_turn(&contents)["parts"]
        .as_array()
        .expect("parts is an array")
        .iter()
        .filter(|part| part["text"].as_str().is_some_and(|text| text.contains("Historical context")))
        .cloned()
        .collect();
    assert!(historical.is_empty());
}

#[test]
fn does_not_add_skip_thought_signature_validator_for_unsigned_vertex_tool_calls() {
    let model = text_model("google-vertex", "google-vertex", "gemini-3-pro-preview");
    let contents = convert(&model, &make_context(&model, None), None);

    let calls = function_call_parts(&contents);
    assert_eq!(calls.len(), 2);
    assert!(calls[0].get("thoughtSignature").is_none());
    assert!(calls[1].get("thoughtSignature").is_none());
    assert!(!contents.to_string().contains("skip_thought_signature_validator"));
}

#[test]
fn preserves_valid_thought_signature_when_present_for_the_same_provider_and_model() {
    let model = text_model("google-generative-ai", "google", "gemini-3-pro-preview");
    let contents = convert(&model, &make_context(&model, Some(VALID_SIG)), None);

    let calls = function_call_parts(&contents);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["thoughtSignature"], json!(VALID_SIG));
    assert!(calls[1].get("thoughtSignature").is_none());
}

#[test]
fn omits_standalone_same_model_thinking_replay_when_thinking_is_off() {
    let model = text_model("google-generative-ai", "google", "gemini-3-pro-preview");
    let previous = assistant(
        &model,
        vec![
            thinking_with_signature("prior Google thinking", VALID_SIG),
            text_with_signature("previous answer", TEXT_SIG),
        ],
        StopReason::Stop,
    );
    let context = context(vec![user_text("first turn"), previous, user_text("follow-up")]);
    let contents = convert(&model, &context, Some(false));

    assert_eq!(model_turn(&contents)["parts"], json!([{ "text": "previous answer" }]));
}

#[test]
fn does_not_add_a_thought_signature_for_non_gemini_3_models() {
    let model = text_model("google-generative-ai", "google", "gemini-2.5-flash");
    let mut other = model.clone();
    other.id = "other-model".to_owned();
    let contents = convert(&model, &make_context(&other, None), None);

    let calls = function_call_parts(&contents);
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|part| part["functionCall"].get("id").is_none()));
    assert!(calls.iter().all(|part| part.get("thoughtSignature").is_none()));
    let responses = function_response_parts(&contents);
    assert_eq!(responses.len(), 2);
    assert!(responses.iter().all(|part| part["functionResponse"].get("id").is_none()));
}

#[test]
fn requires_tool_call_id_returns_false_for_gemini_2_5_flash() {
    assert!(!requires_tool_call_id("gemini-2.5-flash"));
}

#[test]
fn requires_tool_call_id_returns_true_for_gemini_3_6_flash() {
    assert!(requires_tool_call_id("gemini-3.6-flash"));
}

#[test]
fn requires_tool_call_id_returns_true_for_claude_sonnet_4_5() {
    assert!(requires_tool_call_id("claude-sonnet-4-5"));
}

#[test]
fn requires_tool_call_id_returns_true_for_gpt_oss_120b() {
    assert!(requires_tool_call_id("gpt-oss-120b"));
}
