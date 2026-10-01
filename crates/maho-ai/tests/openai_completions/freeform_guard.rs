use maho_ai::types::{AssistantMessageEvent, StopReason};
use serde_json::json;

use super::harness::*;

#[tokio::test]
async fn rejects_freeform_tools_before_serializing_chat_completions_function_tools() {
    let freeform = maho_ai::types::Tool {
        name: "apply_patch".to_owned(),
        description: "freeform apply_patch".to_owned(),
        parameters: json!({
            "type": "object",
            "properties": { "input": { "type": "string" } },
            "required": ["input"],
        }),
        freeform: Some(maho_ai::types::FreeformToolFormat {
            kind: "grammar".to_owned(),
            syntax: "lark".to_owned(),
            definition: "start: /.*/".to_owned(),
        }),
        constrained_sampling: None,
    };
    let model = model(&[("id", json!("gpt-4o-mini")), ("name", json!("GPT-4o mini"))]);
    let context = context(vec![user_message("patch")], Some(vec![freeform]));
    let transport = ScriptedTransport::success([chunk(json!({ "content": "unused" }), Some("stop"))]);

    let result = finish(&run(&model, &context, options("test-key"), transport)).await;

    assert_eq!(result.stop_reason, StopReason::Error);
    assert!(
        result.error_message.unwrap_or_default().contains("Freeform tools cannot be sent to OpenAI Chat Completions"),
        "freeform guard message"
    );
    let _ = std::mem::size_of::<AssistantMessageEvent>();
}
