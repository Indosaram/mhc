//! Port of senpi packages/ai/test/google-shared-gemini3-unsigned-tool-call.test.ts.

use crate::google_fixtures;

use google_fixtures::{
    assistant, context, image, image_model, text, text_model, text_with_signature,
    thinking_with_signature, tool_call, tool_call_with_signature, tool_result, user_text,
};
use maho_ai::api::google_shared::{
    ConvertMessagesOptions, convert_messages, requires_tool_call_id,
};
use maho_ai::types::{ContentBlock, Context, Message, Model, StopReason, UserContent, UserMessage};
use serde_json::{Value, json};

const VALID_SIG: &str = "AAAAAAAAAAAAAAAAAAAAAA==";
const TEXT_SIG: &str = "BBBBBBBBBBBBBBBBBBBBBB==";

const FAKE_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

fn foreign_model() -> Model {
    text_model(
        "openai-completions",
        "openrouter",
        "deepseek/deepseek-v4.1-flash",
    )
}

fn make_context(model: &Model, thought_signature: Option<&str>) -> Context {
    make_context_with_error(model, thought_signature, false)
}

fn make_context_with_error(
    model: &Model,
    thought_signature: Option<&str>,
    first_result_is_error: bool,
) -> Context {
    let first_call = match thought_signature {
        Some(signature) => {
            tool_call_with_signature("call_1", "bash", json!({ "command": "echo hi" }), signature)
        }
        None => tool_call("call_1", "bash", json!({ "command": "echo hi" })),
    };
    context(vec![
        user_text("Hi"),
        assistant(
            model,
            vec![
                first_call,
                tool_call("call_2", "bash", json!({ "command": "ls -la" })),
            ],
            StopReason::ToolUse,
        ),
        tool_result("call_1", "bash", vec![text("hi")], first_result_is_error),
        tool_result("call_2", "bash", vec![text("files")], false),
    ])
}

fn user_blocks(blocks: Vec<ContentBlock>) -> Message {
    Message::User(UserMessage {
        content: UserContent::Blocks(blocks),
        timestamp: 0,
    })
}

fn parts(contents: &Value, index: usize) -> Vec<Value> {
    contents[index]["parts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn all_parts(contents: &Value) -> Vec<Value> {
    contents
        .as_array()
        .expect("contents is an array")
        .iter()
        .flat_map(|content| content["parts"].as_array().cloned().unwrap_or_default())
        .collect()
}

fn single_step(call: ContentBlock, result: Vec<ContentBlock>) -> Context {
    context(vec![
        user_text("read file"),
        assistant(&foreign_model(), vec![call], StopReason::ToolUse),
        tool_result("call_img", "read", result, false),
    ])
}

fn convert(model: &Model, context: &Context, preserve_thinking: Option<bool>) -> Value {
    serde_json::to_value(convert_messages(
        model,
        context,
        ConvertMessagesOptions { preserve_thinking },
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
    let model = text_model(
        api,
        if api == "google-vertex" {
            "google-vertex"
        } else {
            "google"
        },
        id,
    );
    let contents = convert(&model, &make_context(&model, Some(VALID_SIG)), None);

    let calls = function_call_parts(&contents);
    let responses = function_response_parts(&contents);
    let call_ids: Vec<&str> = calls
        .iter()
        .filter_map(|part| part["functionCall"]["id"].as_str())
        .collect();
    let response_ids: Vec<&str> = responses
        .iter()
        .filter_map(|part| part["functionResponse"]["id"].as_str())
        .collect();
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
    assert!(calls.is_empty());
    assert!(function_response_parts(&contents).is_empty());
    assert_eq!(contents.as_array().expect("contents").len(), 3);
    assert_eq!(contents[2]["parts"].as_array().expect("results").len(), 2);
    let turn = model_turn(&contents);
    assert_eq!(turn["parts"].as_array().expect("parts").len(), 2);
    assert_eq!(
        turn["parts"][0]["text"],
        json!("[Tool Call: bash (id: call_1)]\nArguments: {\n  \"command\": \"echo hi\"\n}")
    );
    assert!(
        turn["parts"][1]["text"]
            .as_str()
            .expect("text call")
            .contains("call_2")
    );
    assert!(
        turn["parts"][1]["text"]
            .as_str()
            .expect("text call")
            .contains("\"command\": \"ls -la\"")
    );
    assert_eq!(
        contents[2]["parts"][0]["text"],
        json!("[Tool Result: bash (id: call_1)]\nhi")
    );
    assert_eq!(
        contents[2]["parts"][1]["text"],
        json!("[Tool Result: bash (id: call_2)]\nfiles")
    );
    assert!(
        !contents
            .to_string()
            .contains("skip_thought_signature_validator")
    );

    let historical: Vec<Value> = model_turn(&contents)["parts"]
        .as_array()
        .expect("parts is an array")
        .iter()
        .filter(|part| {
            part["text"]
                .as_str()
                .is_some_and(|text| text.contains("Historical context"))
        })
        .cloned()
        .collect();
    assert!(historical.is_empty());
}

#[test]
fn does_not_add_skip_thought_signature_validator_for_unsigned_vertex_tool_calls() {
    let model = text_model("google-vertex", "google-vertex", "gemini-3-pro-preview");
    let contents = convert(&model, &make_context(&model, None), None);

    let calls = function_call_parts(&contents);
    assert!(calls.is_empty());
    assert!(function_response_parts(&contents).is_empty());
    assert_eq!(contents.as_array().expect("contents").len(), 3);
    assert_eq!(contents[2]["parts"].as_array().expect("results").len(), 2);
    let turn = model_turn(&contents);
    assert_eq!(turn["parts"].as_array().expect("parts").len(), 2);
    assert_eq!(
        turn["parts"][0]["text"],
        json!("[Tool Call: bash (id: call_1)]\nArguments: {\n  \"command\": \"echo hi\"\n}")
    );
    assert!(
        turn["parts"][1]["text"]
            .as_str()
            .expect("text call")
            .contains("call_2")
    );
    assert!(
        turn["parts"][1]["text"]
            .as_str()
            .expect("text call")
            .contains("\"command\": \"ls -la\"")
    );
    assert_eq!(
        contents[2]["parts"][0]["text"],
        json!("[Tool Result: bash (id: call_1)]\nhi")
    );
    assert_eq!(
        contents[2]["parts"][1]["text"],
        json!("[Tool Result: bash (id: call_2)]\nfiles")
    );
    assert!(
        !contents
            .to_string()
            .contains("skip_thought_signature_validator")
    );
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
    let context = context(vec![
        user_text("first turn"),
        previous,
        user_text("follow-up"),
    ]);
    let contents = convert(&model, &context, Some(false));

    assert_eq!(
        model_turn(&contents)["parts"],
        json!([{ "text": "previous answer" }])
    );
}

#[test]
fn does_not_add_a_thought_signature_for_non_gemini_3_models() {
    let model = text_model("google-generative-ai", "google", "gemini-2.5-flash");
    let mut other = model.clone();
    other.id = "other-model".to_owned();
    let contents = convert(&model, &make_context(&other, None), None);

    let calls = function_call_parts(&contents);
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .all(|part| part["functionCall"].get("id").is_none())
    );
    assert!(
        calls
            .iter()
            .all(|part| part.get("thoughtSignature").is_none())
    );
    let responses = function_response_parts(&contents);
    assert_eq!(responses.len(), 2);
    assert!(
        responses
            .iter()
            .all(|part| part["functionResponse"].get("id").is_none())
    );
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

#[test]
fn replays_unsigned_tool_calls_and_their_results_as_text_for_vertex_gemini_3() {
    let model = text_model("google-vertex", "google-vertex", "gemini-3-pro-preview");
    let contents = convert(&model, &make_context(&foreign_model(), None), None);

    let roles: Vec<&str> = contents
        .as_array()
        .expect("contents")
        .iter()
        .filter_map(|c| c["role"].as_str())
        .collect();
    assert_eq!(roles, vec!["user", "model", "user"]);
    let calls = parts(&contents, 1);
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|part| part.get("functionCall").is_none()));
    assert_eq!(
        calls[0]["text"],
        json!("[Tool Call: bash (id: call_1)]\nArguments: {\n  \"command\": \"echo hi\"\n}")
    );
    let results = parts(&contents, 2);
    assert!(
        results
            .iter()
            .all(|part| part.get("functionResponse").is_none())
    );
    assert_eq!(
        results[0]["text"],
        json!("[Tool Result: bash (id: call_1)]\nhi")
    );
    assert_eq!(
        results[1]["text"],
        json!("[Tool Result: bash (id: call_2)]\nfiles")
    );
    assert!(
        !contents
            .to_string()
            .contains("skip_thought_signature_validator")
    );
}

#[test]
fn replays_same_model_unsigned_tool_calls_as_text_too() {
    let model = text_model("google-vertex", "google-vertex", "gemini-3-pro-preview");
    let contents = convert(&model, &make_context(&model, None), None);
    let parts = all_parts(&contents);
    assert!(
        parts.iter().all(
            |part| part.get("functionCall").is_none() && part.get("functionResponse").is_none()
        )
    );
    let starts = |prefix: &str| {
        parts
            .iter()
            .filter(|part| part["text"].as_str().is_some_and(|t| t.starts_with(prefix)))
            .count()
    };
    assert_eq!(starts("[Tool Call: bash (id: call_"), 2);
    assert_eq!(starts("[Tool Result: bash (id: call_"), 2);
}

#[test]
fn marks_an_errored_text_replayed_tool_result() {
    let model = text_model("google-vertex", "google-vertex", "gemini-3-pro-preview");
    let contents = convert(
        &model,
        &make_context_with_error(&foreign_model(), None, true),
        None,
    );
    assert_eq!(
        parts(&contents, 2)[0]["text"],
        json!("[Tool Result: bash (id: call_1)] (error)\nhi")
    );
}

#[test]
fn downgrades_step_to_text_when_the_signature_is_only_on_a_later_call() {
    let model = text_model("google-generative-ai", "google", "gemini-3-pro-preview");
    let context = context(vec![
        user_text("run commands"),
        assistant(
            &model,
            vec![
                tool_call("call_1", "bash", json!({ "command": "first" })),
                tool_call_with_signature(
                    "call_2",
                    "bash",
                    json!({ "command": "second" }),
                    VALID_SIG,
                ),
            ],
            StopReason::ToolUse,
        ),
        tool_result("call_1", "bash", vec![text("result 1")], false),
        tool_result("call_2", "bash", vec![text("result 2")], false),
    ]);
    let contents = convert(&model, &context, None);
    assert!(function_call_parts(&contents).is_empty());
    let turn = model_turn(&contents);
    assert!(
        turn["parts"][0]["text"]
            .as_str()
            .expect("text")
            .contains("[Tool Call: bash (id: call_1)]")
    );
    assert!(
        turn["parts"][1]["text"]
            .as_str()
            .expect("text")
            .contains("[Tool Call: bash (id: call_2)]")
    );
    assert_eq!(
        parts(&contents, 2)[0]["text"],
        json!("[Tool Result: bash (id: call_1)]\nresult 1")
    );
    assert_eq!(
        parts(&contents, 2)[1]["text"],
        json!("[Tool Result: bash (id: call_2)]\nresult 2")
    );
}

fn two_turns(model: &Model, boundary: Message) -> Value {
    let foreign = foreign_model();
    let context = context(vec![
        user_text("turn 1 prompt"),
        assistant(
            &foreign,
            vec![tool_call(
                "call_old",
                "bash",
                json!({ "command": "echo old" }),
            )],
            StopReason::ToolUse,
        ),
        tool_result("call_old", "bash", vec![text("old output")], false),
        boundary,
        assistant(
            &foreign,
            vec![tool_call(
                "call_new",
                "bash",
                json!({ "command": "echo new" }),
            )],
            StopReason::ToolUse,
        ),
        tool_result("call_new", "bash", vec![text("new output")], false),
    ]);
    convert(model, &context, None)
}

#[test]
fn preserves_structured_replay_for_earlier_turns_and_only_downgrades_current_turn() {
    let model = text_model("google-vertex", "google-vertex", "gemini-3-pro-preview");
    let contents = two_turns(&model, user_text("turn 2 prompt"));
    assert_eq!(contents[1]["role"], json!("model"));
    assert_eq!(
        contents[1]["parts"][0]["functionCall"],
        json!({ "name": "bash", "args": { "command": "echo old" }, "id": "call_old" })
    );
    assert_eq!(
        contents[2]["parts"][0]["functionResponse"],
        json!({ "name": "bash", "response": { "output": "old output" }, "id": "call_old" })
    );
    assert_eq!(contents[3]["role"], json!("model"));
    assert!(contents[3]["parts"][0].get("functionCall").is_none());
    assert_eq!(
        contents[3]["parts"][0]["text"],
        json!("[Tool Call: bash (id: call_new)]\nArguments: {\n  \"command\": \"echo new\"\n}")
    );
    assert_eq!(contents[4]["role"], json!("user"));
    assert_eq!(
        contents[4]["parts"][0]["text"],
        json!("[Tool Result: bash (id: call_new)]\nnew output")
    );
}

#[test]
fn treats_an_image_only_user_message_as_the_turn_boundary() {
    let model = image_model("google-vertex", "google-vertex", "gemini-3-pro-preview");
    let contents = two_turns(&model, user_blocks(vec![image(FAKE_PNG, "image/png")]));
    assert_eq!(
        contents[1]["parts"][0]["functionCall"]["id"],
        json!("call_old")
    );
    // The image folds into the tool-result user turn right after the earlier step.
    let current = 3;
    assert_eq!(contents[current]["role"], json!("model"));
    assert!(contents[current]["parts"][0].get("functionCall").is_none());
    assert_eq!(
        contents[current]["parts"][0]["text"],
        json!("[Tool Call: bash (id: call_new)]\nArguments: {\n  \"command\": \"echo new\"\n}")
    );
    let result = &contents[current + 1];
    assert_eq!(result["role"], json!("user"));
    assert!(result["parts"][0].get("functionResponse").is_none());
    assert_eq!(
        result["parts"][0]["text"],
        json!("[Tool Result: bash (id: call_new)]\nnew output")
    );
}

#[test]
fn ignores_an_empty_user_message_when_locating_the_turn_boundary() {
    let model = text_model("google-vertex", "google-vertex", "gemini-3-pro-preview");
    let context = context(vec![
        user_text("run commands"),
        assistant(
            &foreign_model(),
            vec![tool_call("call_1", "bash", json!({ "command": "first" }))],
            StopReason::ToolUse,
        ),
        tool_result("call_1", "bash", vec![text("result 1")], false),
        user_blocks(vec![text("   ")]),
    ]);
    let contents = convert(&model, &context, None);
    let turn = model_turn(&contents);
    assert!(turn["parts"][0].get("functionCall").is_none());
    assert!(
        turn["parts"][0]["text"]
            .as_str()
            .expect("text")
            .contains("[Tool Call: bash (id: call_1)]")
    );
}

#[test]
fn appends_inline_data_image_parts_for_image_only_tool_results_in_text_replay() {
    let model = image_model("google-vertex", "google-vertex", "gemini-3-pro-preview");
    let call = tool_call("call_img", "read", json!({ "path": "img.png" }));
    let contents = convert(
        &model,
        &single_step(call, vec![image(FAKE_PNG, "image/png")]),
        None,
    );
    let result = parts(&contents, 2);
    assert_eq!(contents[2]["role"], json!("user"));
    assert_eq!(result.len(), 2);
    assert_eq!(
        result[0]["text"],
        json!("[Tool Result: read (id: call_img)]")
    );
    assert_eq!(
        result[1]["inlineData"],
        json!({ "mimeType": "image/png", "data": FAKE_PNG })
    );
}

#[test]
fn appends_inline_data_image_parts_for_text_and_image_tool_results_in_text_replay() {
    let model = image_model("google-vertex", "google-vertex", "gemini-3-pro-preview");
    let call = tool_call("call_img", "read", json!({ "path": "img.png" }));
    let content = vec![text("here is the image"), image(FAKE_PNG, "image/png")];
    let contents = convert(&model, &single_step(call, content), None);
    let result = parts(&contents, 2);
    assert_eq!(result.len(), 2);
    assert_eq!(
        result[0]["text"],
        json!("[Tool Result: read (id: call_img)]\nhere is the image")
    );
    assert_eq!(
        result[1]["inlineData"],
        json!({ "mimeType": "image/png", "data": FAKE_PNG })
    );
}

#[test]
fn keeps_the_transform_layer_tool_image_placeholder_for_non_vision_models() {
    let model = text_model("google-vertex", "google-vertex", "gemini-3-pro-preview");
    let call = tool_call("call_img", "read", json!({ "path": "img.png" }));
    let contents = convert(
        &model,
        &single_step(call, vec![image(FAKE_PNG, "image/png")]),
        None,
    );
    let result = parts(&contents, 2);
    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0]["text"],
        json!(
            "[Tool Result: read (id: call_img)]\n(tool image omitted: model does not support images)"
        )
    );
    assert!(result[0].get("inlineData").is_none());
}
