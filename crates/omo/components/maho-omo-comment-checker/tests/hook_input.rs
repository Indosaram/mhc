mod support;
use std::{path::Path, sync::Arc};
use maho_ext_api::*;
use maho_omo_comment_checker::hook_input::to_hook_input;

struct BoundSession;
impl ToolSessionManager for BoundSession {
    fn session_id(&self) -> &str { "session-1" }
    fn session_file(&self) -> Option<&Path> { Some(Path::new("/tmp/transcript.jsonl")) }
}
impl SessionManager for BoundSession {
    fn get_entries(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_branch(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { None }
}

#[test]
fn bound_session_and_tool_response_reach_checker() {
    let mut ctx = support::context();
    ctx.session_manager = Arc::new(BoundSession);
    ctx.cwd = "/workspace".into();
    let event = ToolResultEvent { tool_name: "write".into(), tool_call_id: "id".into(),
        input: serde_json::json!({"content":"written"}), content: vec![ToolContent::text("original")],
        details: Some(serde_json::json!({"retained":true})), is_error: false, usage: None };
    let hook = to_hook_input(&event, &ctx, "/workspace/file.rs");
    assert_eq!(hook.session_id, "session-1");
    assert_eq!(hook.transcript_path, "/tmp/transcript.jsonl");
    assert_eq!(hook.cwd, "/workspace");
    assert_eq!(hook.tool_name, "write");
    assert_eq!(hook.hook_event_name, "PostToolUse");
    assert_eq!(hook.tool_input.content.as_deref(), Some("written"));
    assert_eq!(hook.tool_response, Some(serde_json::json!({
        "content":[{"type":"text","text":"original"}],"details":{"retained":true},"isError":false
    })));
}

#[test]
fn edit_aliases_preserve_precedence_and_filter_invalid_entries() {
    let event = ToolResultEvent { tool_name: "edit".into(), tool_call_id: "id".into(),
        input: serde_json::json!({"old_string":"primary","oldText":"fallback","newText":"new",
            "edits":[{"oldText":"a","newText":"b"},null,{"old_string":"c"},{"old_string":"d","new_string":"e","oldText":"ignored"}]}),
        content: Vec::new(), details: None, is_error: false, usage: None };
    let hook = to_hook_input(&event, &support::context(), "/tmp/file.rs");
    assert_eq!(hook.tool_input.old_string.as_deref(), Some("primary"));
    assert_eq!(hook.tool_input.new_string.as_deref(), Some("new"));
    let edits = hook.tool_input.edits.expect("valid edits");
    assert_eq!(edits.len(), 2);
    assert_eq!(edits[0].old_string, "a");
    assert_eq!(edits[0].new_string, "b");
    assert_eq!(edits[1].old_string, "d");
    assert_eq!(edits[1].new_string, "e");
}
