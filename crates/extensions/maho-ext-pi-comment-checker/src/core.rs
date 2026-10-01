use comment_checker_core::{EditPair, HookInput, HookToolInput, get_apply_patch_metadata_files, get_string, parse_apply_patch_requests};
use maho_ext_api::{ToolContent, ToolResultEvent};

#[derive(Debug, PartialEq, Eq)]
pub struct CommentCheckRequest {
    pub source_tool_name: String,
    pub tool_name: String,
    pub file_path: String,
    pub tool_input: HookToolInput,
}

pub fn is_tool_failure_output(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    lower.starts_with("error") || lower.contains("error:") || lower.contains("failed to") || lower.contains("could not")
}

fn request(source: &str, tool: &str, path: String, mut input: HookToolInput) -> CommentCheckRequest {
    input.file_path = Some(path.clone());
    CommentCheckRequest { source_tool_name: source.to_owned(), tool_name: tool.to_owned(), file_path: path, tool_input: input }
}

pub fn extract_comment_check_requests(event: &ToolResultEvent) -> Vec<CommentCheckRequest> {
    let text = event.content.iter().filter_map(|block| match block { ToolContent::Text { text, .. } => Some(text.as_str()), ToolContent::Image { .. } => None }).collect::<Vec<_>>().join("\n");
    if event.is_error || is_tool_failure_output(&text) { return Vec::new(); }
    let Some(input) = event.input.as_object() else { return Vec::new(); };
    let name = event.tool_name.to_lowercase();
    if name == "apply_patch" {
        let metadata = event.details.as_ref().map(get_apply_patch_metadata_files).unwrap_or_default();
        let mut requests = Vec::new();
        for file in metadata {
            if file.r#type.as_deref() == Some("delete") { continue; }
            let path = file.move_path.unwrap_or(file.file_path);
            let tool = if file.before.is_empty() { "Write" } else { "Edit" };
            let input = if tool == "Write" { HookToolInput { content: Some(file.after), ..Default::default() } } else { HookToolInput { old_string: Some(file.before), new_string: Some(file.after), ..Default::default() } };
            requests.push(request(&event.tool_name, tool, path, input));
        }
        if !requests.is_empty() { return requests; }
        let Some(patch) = get_string(input, &["input", "patch"]).filter(|patch| !patch.is_empty()) else { return requests; };
        for edit in parse_apply_patch_requests(&patch) {
            let tool = if edit.before.is_empty() { "Write" } else { "Edit" };
            let input = if tool == "Write" { HookToolInput { content: Some(edit.after), ..Default::default() } } else { HookToolInput { old_string: Some(edit.before), new_string: Some(edit.after), ..Default::default() } };
            requests.push(request(&event.tool_name, tool, edit.file_path, input));
        }
        return requests;
    }
    let Some(path) = get_string(input, &["filePath", "file_path", "path"]).filter(|path| !path.is_empty()) else { return Vec::new(); };
    let (tool, value) = match name.as_str() {
        "write" => {
            let Some(content) = get_string(input, &["content"]) else { return Vec::new(); };
            ("Write", HookToolInput { content: Some(content), ..Default::default() })
        }
        "edit" => {
            let old_string = get_string(input, &["oldString", "old_string"]);
            let new_string = get_string(input, &["newString", "new_string"]);
            if old_string.is_none() && new_string.is_none() { return Vec::new(); }
            ("Edit", HookToolInput { old_string, new_string, ..Default::default() })
        }
        "multiedit" | "multi_edit" => {
            let edits: Vec<_> = input.get("edits").and_then(|value| value.as_array()).into_iter().flatten().filter_map(|item| {
                let item = item.as_object()?;
                Some(EditPair { old_string: get_string(item, &["oldString", "old_string"])?, new_string: get_string(item, &["newString", "new_string"])? })
            }).collect();
            if edits.is_empty() { return Vec::new(); }
            ("MultiEdit", HookToolInput { edits: Some(edits), ..Default::default() })
        }
        _ => return Vec::new(),
    };
    vec![request(&event.tool_name, tool, path, value)]
}

pub fn to_hook_input(request: &CommentCheckRequest, session_id: &str, cwd: &str) -> HookInput {
    HookInput { session_id: session_id.to_owned(), tool_name: request.tool_name.clone(), transcript_path: String::new(), cwd: cwd.to_owned(), hook_event_name: "PostToolUse".to_owned(), tool_input: request.tool_input.clone(), tool_response: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn event(tool: &str, input: serde_json::Value) -> ToolResultEvent { ToolResultEvent { tool_call_id: "call".into(), tool_name: tool.into(), input, content: Vec::new(), details: None, is_error: false, usage: None } }
    #[test] fn write_when_content_present() { let result = extract_comment_check_requests(&event("write", json!({"filePath":"a.ts","content":"x"}))); assert_eq!(result[0].tool_name, "Write"); assert_eq!(result[0].tool_input.content.as_deref(), Some("x")); }
    #[test] fn edit_when_strings_present() { let result = extract_comment_check_requests(&event("edit", json!({"path":"a.ts","old_string":"x","new_string":"y"}))); assert_eq!(result[0].tool_input.new_string.as_deref(), Some("y")); }
    #[test] fn multiedit_when_aliases_present() { let result = extract_comment_check_requests(&event("multi_edit", json!({"path":"a.ts","edits":[{"oldString":"x","newString":"y"}]}))); assert_eq!(result[0].tool_name, "MultiEdit"); }
    #[test] fn ignored_when_tool_error() { let mut value = event("write", json!({"path":"a.ts","content":"x"})); value.is_error = true; assert!(extract_comment_check_requests(&value).is_empty()); }
    #[test] fn ignored_when_failure_text() { let mut value = event("write", json!({"path":"a.ts","content":"x"})); value.content.push(ToolContent::text("Failed to write")); assert!(extract_comment_check_requests(&value).is_empty()); }
    #[test] fn ignored_when_unknown_tool() { assert!(extract_comment_check_requests(&event("bash", json!({"path":"a.ts"}))).is_empty()); }
    #[test] fn ignored_when_missing_path() { assert!(extract_comment_check_requests(&event("write", json!({"content":"x"}))).is_empty()); }
    #[test] fn failure_when_error_prefix() { assert!(is_tool_failure_output(" ERROR invalid ")); }
    #[test] fn failure_when_embedded_error() { assert!(is_tool_failure_output("write error: denied")); }
    #[test] fn success_when_normal_output() { assert!(!is_tool_failure_output("wrote file")); }
    #[test] fn hook_when_request_present() { let requests = extract_comment_check_requests(&event("write", json!({"path":"a.ts","content":"x"}))); let input = to_hook_input(&requests[0], "session", "/work"); assert_eq!(input.session_id, "session"); assert_eq!(input.hook_event_name, "PostToolUse"); assert_eq!(input.tool_response, None); }
}
