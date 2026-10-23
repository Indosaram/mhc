//! Port of senpi packages/ai/test/google-shared-image-tool-result-routing.test.ts.

use crate::google_fixtures;

use google_fixtures::{assistant, context, image, image_model, text, tool_call, tool_call_with_signature, tool_result, user_text};
use maho_ai::api::google_shared::{convert_messages, ConvertMessagesOptions};
use maho_ai::types::{Context, Model, StopReason};
use serde_json::{json, Value};

fn make_context(model: &Model) -> Context {
    context(vec![
        user_text("read the files"),
        assistant(
            model,
            vec![
                if model.id.starts_with("gemini-3") {
                    tool_call_with_signature("call_a", "read", json!({ "path": "a.txt" }), "AAAAAAAAAAAAAAAAAAAAAA==")
                } else {
                    tool_call("call_a", "read", json!({ "path": "a.txt" }))
                },
                tool_call("call_img", "read", json!({ "path": "image.png" })),
                tool_call("call_b", "read", json!({ "path": "b.txt" })),
            ],
            StopReason::ToolUse,
        ),
        tool_result("call_a", "read", vec![text("alpha text")], false),
        tool_result("call_img", "read", vec![image("abc", "image/png")], false),
        tool_result("call_b", "read", vec![text("beta text")], false),
    ])
}

fn convert(model: &Model) -> Value {
    serde_json::to_value(convert_messages(model, &make_context(model), ConvertMessagesOptions::default()))
        .expect("contents serialize")
}

#[test]
fn keeps_synthetic_images_inside_the_tool_result_turn_for_gemini_2_x_google_api_models() {
    let model = image_model("google-generative-ai", "google", "gemini-2.5-flash");
    let contents = convert(&model);

    let items = contents.as_array().expect("contents is an array");
    assert_eq!(items.len(), 3);
    let parts = items[2]["parts"].as_array().expect("parts");
    assert_eq!(parts.len(), 5);
    assert!(parts[0].get("functionResponse").is_some());
    assert!(parts[1].get("functionResponse").is_some());
    assert_eq!(parts[2]["text"], json!("Tool result image:"));
    assert!(parts[3].get("inlineData").is_some());
    assert!(parts[4].get("functionResponse").is_some());
}

#[test]
fn nests_image_tool_results_for_gemini_3_google_api_models() {
    let model = image_model("google-generative-ai", "google", "gemini-3-pro-preview");
    let contents = convert(&model);

    let items = contents.as_array().expect("contents is an array");
    assert_eq!(items.len(), 3);
    let tool_result_turn = &items[2];
    assert_eq!(tool_result_turn["parts"].as_array().expect("parts").len(), 3);
    let image_response = &tool_result_turn["parts"][1]["functionResponse"];
    assert!(image_response.is_object());
    assert_eq!(image_response["parts"].as_array().expect("nested parts").len(), 1);
    assert!(image_response["parts"][0].get("inlineData").is_some());
}
