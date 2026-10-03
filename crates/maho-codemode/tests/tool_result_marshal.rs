use maho_codemode::tool::tool_result_marshal::{marshal_tool_result, tool_result_is_error};
use maho_ext_api::{AgentToolResult, ContentBlock, ImageContent};
use serde_json::{Value, json};

#[test]
fn empty_details_and_text_use_compact_shape() {
    let mut result = AgentToolResult::text("first");
    result.content.push(ContentBlock::text("second"));
    assert_eq!(marshal_tool_result(&result), json!({"text":"first\nsecond"}));
}

#[test]
fn images_use_kernel_display_shape() {
    let mut result = AgentToolResult::text("image");
    result.content.push(ContentBlock::Image(ImageContent { data: "aGVsbG8=".into(), mime_type: "image/png".into() }));
    assert_eq!(marshal_tool_result(&result), json!({"text":"image", "images":[{"mimeType":"image/png", "dataBase64":"aGVsbG8="}], "hasError":false}));
}

#[test]
fn nonempty_details_survive_marshaling() {
    let mut result = AgentToolResult::text("receipt");
    result.details = json!({"task_id":"st_abc"});
    assert_eq!(marshal_tool_result(&result), json!({"text":"receipt", "details":{"task_id":"st_abc"}, "images":[], "hasError":false}));
}

#[test]
fn null_and_array_details_are_not_empty_objects() {
    let mut result = AgentToolResult::text("value");
    for details in [Value::Null, json!([])] {
        result.details = details.clone();
        assert_eq!(marshal_tool_result(&result)["details"], details);
        assert!(marshal_tool_result(&result).get("images").is_some());
    }
}

#[test]
fn error_flag_requires_boolean_true_in_details() {
    let mut result = AgentToolResult::text("error");
    result.is_error = Some(true);
    assert!(!tool_result_is_error(&result));
    result.details = json!({"isError":"true"});
    assert!(!tool_result_is_error(&result));
    result.details = json!({"isError":true});
    assert!(tool_result_is_error(&result));
    assert_eq!(marshal_tool_result(&result)["hasError"], true);
}
