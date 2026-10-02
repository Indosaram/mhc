use maho_ext_mcp::prompts::flatten_prompt_messages;
use serde_json::json;
#[test]
fn one_prompt_message_does_not_add_a_role_prefix() {
    assert_eq!(flatten_prompt_messages(&[json!({"role":"user","content":{"type":"text","text":"hello"}})]),"hello");
}
#[test]
fn multiple_prompt_messages_keep_roles_and_nontext_content() {
    assert_eq!(flatten_prompt_messages(&[json!({"role":"user","content":{"text":"hello"}}),json!({"role":"assistant","content":{"type":"image","data":"encoded"}})]),"user: hello\n\nassistant: {\"type\":\"image\",\"data\":\"encoded\"}");
}
