use maho_ext_ask_user::params::{claude_params, codex_params};
use serde_json::json;

#[test]
fn codex_schema_requires_wait_flag_and_bounded_choices() {
    let schema = codex_params();
    assert_eq!(schema["required"], json!(["questions", "wait_for_answer"]));
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"]["questions"]["maxItems"], 3);
    assert_eq!(schema["properties"]["questions"]["items"]["required"], json!(["id", "header", "question", "options"]));
    assert_eq!(schema["properties"]["questions"]["items"]["properties"]["options"]["maxItems"], 3);
}

#[test]
fn claude_schema_requires_multi_select_but_not_options() {
    let schema = claude_params();
    assert_eq!(schema["required"], json!(["questions", "waitForAnswer"]));
    assert_eq!(schema["properties"]["questions"]["maxItems"], 4);
    assert_eq!(schema["properties"]["questions"]["items"]["required"], json!(["question", "header", "multiSelect"]));
    assert_eq!(schema["properties"]["questions"]["items"]["properties"]["options"]["maxItems"], 4);
}
