//! Port of senpi packages/ai/src/api/cursor-agent/exec-modern.ts.
// ported by todo 12
//!
//! Proto builders for the modern Cursor CLI exec frames (`ExecServerMessage`
//! 27-31, 36-38, 40-55). Split out of `cursor-agent.ts` because these are pure
//! message shapes with no transport, stream, or block-state coupling: the
//! dispatcher decides *which* answer a frame gets, this module knows *what*
//! that answer looks like on the wire. Every builder returns a result whose
//! oneof case is set -- an `ExecClientMessage` carrying a result with an unset
//! oneof is a fake success the server reads as "the tool ran and produced
//! nothing".

use serde_json::Value;

use crate::types::{ContentBlock, ToolResultMessage};

use super::r#gen::agent_pb::{
    execute_hook_request, execute_hook_response, mcp_state_exec_result, pi_bash_exec_result, pi_edit_exec_result,
    pi_find_exec_result, pi_grep_exec_result, pi_ls_exec_result, pi_read_exec_result, pi_write_exec_result,
    AfterAgentResponseRequestResponse, AfterAgentThoughtRequestResponse, BeforeSubmitPromptRequestResponse,
    ExecuteHookRequest, ExecuteHookResponse, ExecuteHookResult, McpStateExecResult, McpStateServer, McpStateSuccess,
    McpToolDefinition, PiBashExecError, PiBashExecResult, PiBashExecSuccess, PiEditExecError, PiEditExecRejected,
    PiEditExecResult, PiEditExecSuccess, PiFindExecError, PiFindExecResult, PiFindExecSuccess, PiGrepExecError,
    PiGrepExecResult, PiGrepExecSuccess, PiLsExecError, PiLsExecResult, PiLsExecSuccess, PiReadExecError,
    PiReadExecResult, PiReadExecSuccess, PiTruncation, PiWriteExecError, PiWriteExecRejected, PiWriteExecResult,
    PiWriteExecSuccess, PostToolUseFailureRequestResponse, PostToolUseRequestResponse, PreCompactRequestResponse,
    PreToolUseRequestResponse, StopRequestResponse, SubagentStartRequestResponse, SubagentStopRequestResponse,
};

/// Flatten a tool result's content into the single `output` string the Pi frames carry.
pub fn pi_output_text(tool_result: &ToolResultMessage) -> String {
    tool_result
        .content
        .iter()
        .map(|item| match item {
            ContentBlock::Text(text) => text.text.clone(),
            ContentBlock::Image(image) => format!("[{} image]", image.mime_type),
            ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) | ContentBlock::ProviderNative(_) => {
                "[undefined image]".to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Read one field off a tool result's `details` bag, or off a nested object
/// inside it. `details` is `unknown` by design -- every tool ships its own
/// shape -- so each read is narrowed rather than asserted.
fn bag_value(bag: Option<&Value>, key: &str) -> Option<Value> {
    match bag? {
        Value::Object(map) => map.get(key).cloned(),
        _ => None,
    }
}

fn detail_count(tool_result: &ToolResultMessage, key: &str) -> Option<u32> {
    positive_count(bag_value(tool_result.details.as_ref(), key))
}

/// `positiveCount`: a finite number above zero floors to an integer; everything else is absent.
fn positive_count(value: Option<Value>) -> Option<u32> {
    let value = value?.as_f64()?;
    if value.is_finite() && value > 0.0 { Some(value.floor() as u32) } else { None }
}

/// The entry cap a listing hit, from either shape a local tool records it in.
fn result_limit_reached(tool_result: &ToolResultMessage) -> Option<u32> {
    if let Some(flat) = detail_count(tool_result, "resultLimitReached") {
        return Some(flat);
    }
    let meta = bag_value(tool_result.details.as_ref(), "meta");
    let limits = bag_value(meta.as_ref(), "limits");
    positive_count(bag_value(limits.as_ref(), "resultLimit").and_then(|limit| bag_value(Some(&limit), "reached")))
}

/// Translate a local tool's truncation summary into `PiTruncation`.
///
/// Two shapes reach here: `details.truncation` (carries an explicit
/// `truncated` boolean) and `details.meta.truncation` (whose presence *is* the
/// signal). Returns `None` when nothing was truncated: the field is `optional`
/// on every Pi success message, and emitting a zeroed `PiTruncation` would
/// tell the server the output was trimmed to nothing.
pub fn pi_truncation(tool_result: &ToolResultMessage) -> Option<PiTruncation> {
    let direct = bag_value(tool_result.details.as_ref(), "truncation");
    let meta = bag_value(tool_result.details.as_ref(), "meta");
    let truncation = match direct {
        Some(direct) => Some(direct),
        None => bag_value(meta.as_ref(), "truncation"),
    }?;
    if truncation.is_null() {
        return None;
    }
    let flag = bag_value(Some(&truncation), "truncated");
    if flag.as_ref().is_some_and(|flag| flag != &Value::Bool(true)) {
        return None;
    }
    let truncated_by = bag_value(Some(&truncation), "truncatedBy");
    let total_lines = bag_value(Some(&truncation), "totalLines");
    let output_lines = bag_value(Some(&truncation), "outputLines");
    let output_bytes = bag_value(Some(&truncation), "outputBytes");
    Some(PiTruncation {
        truncated: true,
        truncated_by: truncated_by.as_ref().and_then(Value::as_str).unwrap_or_default().to_owned(),
        total_lines: total_lines.as_ref().and_then(Value::as_f64).unwrap_or(0.0) as u32,
        output_lines: output_lines.as_ref().and_then(Value::as_f64).unwrap_or(0.0) as u32,
        output_bytes: output_bytes.as_ref().and_then(Value::as_f64).unwrap_or(0.0) as u32,
        first_line_exceeds_limit: bag_value(Some(&truncation), "firstLineExceedsLimit")
            == Some(Value::Bool(true)),
        last_line_partial: bag_value(Some(&truncation), "lastLinePartial") == Some(Value::Bool(true)),
        ..PiTruncation::default()
    })
}

pub fn build_pi_read_result(tool_result: &ToolResultMessage) -> PiReadExecResult {
    let text = pi_output_text(tool_result);
    if tool_result.is_error {
        return build_pi_read_error(if text.is_empty() { "Read failed" } else { &text });
    }
    PiReadExecResult {
        result: Some(pi_read_exec_result::Result::Success(PiReadExecSuccess {
            output: text,
            truncation: pi_truncation(tool_result),
        })),
    }
}

pub fn build_pi_read_error(error: &str) -> PiReadExecResult {
    PiReadExecResult {
        result: Some(pi_read_exec_result::Result::Error(PiReadExecError { error: error.to_owned() })),
    }
}

pub fn build_pi_bash_result(tool_result: &ToolResultMessage) -> PiBashExecResult {
    let text = pi_output_text(tool_result);
    let truncation = pi_truncation(tool_result);
    if tool_result.is_error {
        return PiBashExecResult {
            result: Some(pi_bash_exec_result::Result::Error(PiBashExecError {
                error: if text.is_empty() { "Command failed".to_owned() } else { text },
                truncation,
                ..PiBashExecError::default()
            })),
        };
    }
    PiBashExecResult {
        result: Some(pi_bash_exec_result::Result::Success(PiBashExecSuccess {
            output: text,
            truncation,
            ..PiBashExecSuccess::default()
        })),
    }
}

pub fn build_pi_bash_error(error: &str) -> PiBashExecResult {
    PiBashExecResult {
        result: Some(pi_bash_exec_result::Result::Error(PiBashExecError {
            error: error.to_owned(),
            ..PiBashExecError::default()
        })),
    }
}

/// `PiEditExecSuccess` requires `diff` and `patch` alongside `output`. The
/// local `edit` tool reports them under `details`; when it does not, the
/// strings stay empty rather than being faked from the output text.
pub fn build_pi_edit_result(tool_result: &ToolResultMessage) -> PiEditExecResult {
    let text = pi_output_text(tool_result);
    if tool_result.is_error {
        return build_pi_edit_error(if text.is_empty() { "Edit failed" } else { &text });
    }
    let diff = bag_value(tool_result.details.as_ref(), "diff");
    let patch = bag_value(tool_result.details.as_ref(), "patch");
    PiEditExecResult {
        result: Some(pi_edit_exec_result::Result::Success(PiEditExecSuccess {
            output: text,
            diff: diff.as_ref().and_then(Value::as_str).unwrap_or_default().to_owned(),
            patch: patch.as_ref().and_then(Value::as_str).unwrap_or_default().to_owned(),
            first_changed_line: detail_count(tool_result, "firstChangedLine"),
        })),
    }
}

pub fn build_pi_edit_error(error: &str) -> PiEditExecResult {
    PiEditExecResult {
        result: Some(pi_edit_exec_result::Result::Error(PiEditExecError { error: error.to_owned() })),
    }
}

/// A refusal is not an execution failure: `PiEditExecResult` models them as
/// separate variants, and answering a denied call with `error` reads as "the
/// edit ran and broke", which invites a retry of an operation that was never
/// permitted.
pub fn build_pi_edit_rejected(reason: &str) -> PiEditExecResult {
    PiEditExecResult {
        result: Some(pi_edit_exec_result::Result::Rejected(PiEditExecRejected { reason: reason.to_owned() })),
    }
}

pub fn build_pi_write_result(tool_result: &ToolResultMessage) -> PiWriteExecResult {
    let text = pi_output_text(tool_result);
    if tool_result.is_error {
        return build_pi_write_error(if text.is_empty() { "Write failed" } else { &text });
    }
    PiWriteExecResult {
        result: Some(pi_write_exec_result::Result::Success(PiWriteExecSuccess { output: text })),
    }
}

pub fn build_pi_write_error(error: &str) -> PiWriteExecResult {
    PiWriteExecResult {
        result: Some(pi_write_exec_result::Result::Error(PiWriteExecError { error: error.to_owned() })),
    }
}

/// Same variant split as [`build_pi_edit_rejected`].
pub fn build_pi_write_rejected(reason: &str) -> PiWriteExecResult {
    PiWriteExecResult {
        result: Some(pi_write_exec_result::Result::Rejected(PiWriteExecRejected { reason: reason.to_owned() })),
    }
}

pub fn build_pi_grep_result(tool_result: &ToolResultMessage) -> PiGrepExecResult {
    let text = pi_output_text(tool_result);
    if tool_result.is_error {
        return build_pi_grep_error(if text.is_empty() { "Grep failed" } else { &text });
    }
    let match_limit_reached = detail_count(tool_result, "perFileLimitReached");
    PiGrepExecResult {
        result: Some(pi_grep_exec_result::Result::Success(PiGrepExecSuccess {
            truncation: pi_truncation(tool_result)
                .or_else(|| grep_internal_cap_truncation(tool_result, &text, match_limit_reached)),
            match_limit_reached,
            lines_truncated: bag_value(tool_result.details.as_ref(), "linesTruncated") == Some(Value::Bool(true)),
            output: text,
        })),
    }
}

/// The one grep cap that reaches Cursor through no other field: a search that
/// hit the tool's own total cap sets only the flat `details.truncated` flag,
/// answering as an unqualified success over incomplete results otherwise.
fn grep_internal_cap_truncation(
    tool_result: &ToolResultMessage,
    text: &str,
    match_limit_reached: Option<u32>,
) -> Option<PiTruncation> {
    if match_limit_reached.is_some() {
        return None;
    }
    if bag_value(tool_result.details.as_ref(), "truncated") != Some(Value::Bool(true)) {
        return None;
    }
    Some(PiTruncation {
        truncated: true,
        truncated_by: "matches".to_owned(),
        total_lines: 0,
        output_lines: if text.is_empty() { 0 } else { text.split('\n').count() as u32 },
        output_bytes: text.len() as u32,
        first_line_exceeds_limit: false,
        last_line_partial: false,
        ..PiTruncation::default()
    })
}

pub fn build_pi_grep_error(error: &str) -> PiGrepExecResult {
    PiGrepExecResult {
        result: Some(pi_grep_exec_result::Result::Error(PiGrepExecError { error: error.to_owned() })),
    }
}

pub fn build_pi_find_result(tool_result: &ToolResultMessage) -> PiFindExecResult {
    let text = pi_output_text(tool_result);
    if tool_result.is_error {
        return build_pi_find_error(if text.is_empty() { "Find failed" } else { &text });
    }
    PiFindExecResult {
        result: Some(pi_find_exec_result::Result::Success(PiFindExecSuccess {
            output: text,
            truncation: pi_truncation(tool_result),
            result_limit_reached: result_limit_reached(tool_result),
        })),
    }
}

pub fn build_pi_find_error(error: &str) -> PiFindExecResult {
    PiFindExecResult {
        result: Some(pi_find_exec_result::Result::Error(PiFindExecError { error: error.to_owned() })),
    }
}

pub fn build_pi_ls_result(tool_result: &ToolResultMessage) -> PiLsExecResult {
    let text = pi_output_text(tool_result);
    if tool_result.is_error {
        return build_pi_ls_error(if text.is_empty() { "Ls failed" } else { &text });
    }
    PiLsExecResult {
        result: Some(pi_ls_exec_result::Result::Success(PiLsExecSuccess {
            output: text,
            truncation: pi_truncation(tool_result),
            entry_limit_reached: result_limit_reached(tool_result),
        })),
    }
}

pub fn build_pi_ls_error(error: &str) -> PiLsExecResult {
    PiLsExecResult {
        result: Some(pi_ls_exec_result::Result::Error(PiLsExecError { error: error.to_owned() })),
    }
}

/// Answer `mcpStateExecArgs` (frame 36) from the catalog already advertised in
/// `RequestContext.tools`.
///
/// This client hosts no MCP servers of its own: every forwarded tool is a
/// local tool published under a synthetic `providerIdentifier`. Regrouping the
/// same list keeps the server's view of "which servers exist and what do they
/// expose" consistent with what it was told at context time.
///
/// `serverIdentifiers` filters the answer when the server asks about specific
/// servers. A restart request is answered with the same state rather than an
/// error -- there is nothing to restart.
pub fn build_mcp_state_result(
    tools: &[McpToolDefinition],
    server_identifiers: &[String],
) -> McpStateExecResult {
    let mut by_provider: Vec<(String, Vec<McpToolDefinition>)> = Vec::new();
    for tool in tools {
        let identifier = tool.provider_identifier.clone();
        match by_provider.iter_mut().find(|(existing, _)| *existing == identifier) {
            Some((_, server_tools)) => server_tools.push(tool.clone()),
            None => by_provider.push((identifier, vec![tool.clone()])),
        }
    }

    let servers = by_provider
        .into_iter()
        .filter(|(identifier, _)| server_identifiers.is_empty() || server_identifiers.contains(identifier))
        .map(|(identifier, server_tools)| McpStateServer {
            server_name: identifier.clone(),
            server_identifier: identifier,
            tools: server_tools,
            status: Some("connected".to_owned()),
            ..McpStateServer::default()
        })
        .collect();

    McpStateExecResult {
        result: Some(mcp_state_exec_result::Result::Success(McpStateSuccess { servers })),
    }
}

/// Build the neutral response for a hook query: the matching response case
/// with every field unset.
///
/// This client runs no Cursor hooks, and every field of every response variant
/// is `optional` -- so an empty response of the right case means "no hook had
/// anything to say", which is exactly true. Returns `None` for a request case
/// this build does not model, which the dispatcher answers with
/// `ExecClientThrow` rather than guessing a case.
pub fn build_neutral_hook_result(request: Option<&ExecuteHookRequest>) -> Option<ExecuteHookResult> {
    let response = match request?.request.as_ref()? {
        execute_hook_request::Request::PreCompact(_) => {
            execute_hook_response::Response::PreCompact(PreCompactRequestResponse::default())
        }
        execute_hook_request::Request::SubagentStart(_) => {
            execute_hook_response::Response::SubagentStart(SubagentStartRequestResponse::default())
        }
        execute_hook_request::Request::SubagentStop(_) => {
            execute_hook_response::Response::SubagentStop(SubagentStopRequestResponse::default())
        }
        execute_hook_request::Request::PreToolUse(_) => {
            execute_hook_response::Response::PreToolUse(PreToolUseRequestResponse::default())
        }
        execute_hook_request::Request::PostToolUse(_) => {
            execute_hook_response::Response::PostToolUse(PostToolUseRequestResponse::default())
        }
        execute_hook_request::Request::PostToolUseFailure(_) => {
            execute_hook_response::Response::PostToolUseFailure(PostToolUseFailureRequestResponse::default())
        }
        execute_hook_request::Request::BeforeSubmitPrompt(_) => {
            execute_hook_response::Response::BeforeSubmitPrompt(BeforeSubmitPromptRequestResponse::default())
        }
        execute_hook_request::Request::AfterAgentResponse(_) => {
            execute_hook_response::Response::AfterAgentResponse(AfterAgentResponseRequestResponse::default())
        }
        execute_hook_request::Request::AfterAgentThought(_) => {
            execute_hook_response::Response::AfterAgentThought(AfterAgentThoughtRequestResponse::default())
        }
        execute_hook_request::Request::Stop(_) => {
            execute_hook_response::Response::Stop(StopRequestResponse::default())
        }
    };
    Some(ExecuteHookResult { response: Some(ExecuteHookResponse { response: Some(response) }) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_result(content: &[ContentBlock], is_error: bool, details: Option<Value>) -> ToolResultMessage {
        ToolResultMessage {
            tool_call_id: "call-1".to_owned(),
            tool_name: "read".to_owned(),
            content: content.to_vec(),
            details,
            usage: None,
            added_tool_names: None,
            is_error,
            timestamp: 0,
        }
    }

    fn text(content: &str) -> ContentBlock {
        ContentBlock::text(content)
    }

    #[test]
    fn output_text_flattens_text_and_images() {
        let image = ContentBlock::Image(crate::types::ImageContent {
            data: "aGk=".to_owned(),
            mime_type: "image/png".to_owned(),
        });
        let result = tool_result(&[text("a"), image, text("b")], false, None);
        assert_eq!(pi_output_text(&result), "a\n[image/png image]\nb");
    }

    #[test]
    fn read_result_maps_error_and_success_variants() {
        let failed = build_pi_read_result(&tool_result(&[], true, None));
        assert!(matches!(
            failed.result,
            Some(pi_read_exec_result::Result::Error(PiReadExecError { ref error })) if error == "Read failed"
        ));

        let details = json!({ "truncation": { "truncated": true, "truncatedBy": "lines", "totalLines": 10, "outputLines": 4, "outputBytes": 12, "firstLineExceedsLimit": true } });
        let ok = build_pi_read_result(&tool_result(&[text("body")], false, Some(details)));
        match ok.result {
            Some(pi_read_exec_result::Result::Success(success)) => {
                assert_eq!(success.output, "body");
                let truncation = success.truncation.expect("truncation carried");
                assert!(truncation.truncated);
                assert_eq!(truncation.truncated_by, "lines");
                assert_eq!((truncation.total_lines, truncation.output_lines, truncation.output_bytes), (10, 4, 12));
                assert!(truncation.first_line_exceeds_limit);
                assert!(!truncation.last_line_partial);
            }
            other => panic!("expected success, got {other:?}"),
        }
    }

    #[test]
    fn truncation_is_absent_unless_signalled() {
        // No details at all.
        assert!(pi_truncation(&tool_result(&[text("x")], false, None)).is_none());
        // A `meta.truncation` object whose presence is the signal.
        let meta = json!({ "meta": { "truncation": {} } });
        assert!(pi_truncation(&tool_result(&[text("x")], false, Some(meta))).is_some());
        // An explicit `truncated: false` is not truncation.
        let not_truncated = json!({ "truncation": { "truncated": false } });
        assert!(pi_truncation(&tool_result(&[text("x")], false, Some(not_truncated))).is_none());
    }

    #[test]
    fn edit_result_keeps_diff_and_patch_from_details() {
        let details = json!({ "diff": "--- a\n+++ b", "patch": "@@", "firstChangedLine": 7 });
        let result = build_pi_edit_result(&tool_result(&[text("edited")], false, Some(details)));
        match result.result {
            Some(pi_edit_exec_result::Result::Success(success)) => {
                assert_eq!(success.output, "edited");
                assert_eq!(success.diff, "--- a\n+++ b");
                assert_eq!(success.patch, "@@");
                assert_eq!(success.first_changed_line, Some(7));
            }
            other => panic!("expected success, got {other:?}"),
        }

        let bare = build_pi_edit_result(&tool_result(&[text("edited")], false, None));
        match bare.result {
            Some(pi_edit_exec_result::Result::Success(success)) => {
                assert_eq!(success.diff, "");
                assert_eq!(success.patch, "");
                assert_eq!(success.first_changed_line, None);
            }
            other => panic!("expected success, got {other:?}"),
        }
    }

    #[test]
    fn edit_and_write_rejections_use_the_rejected_variant() {
        assert!(matches!(
            build_pi_edit_rejected("denied").result,
            Some(pi_edit_exec_result::Result::Rejected(PiEditExecRejected { ref reason })) if reason == "denied"
        ));
        assert!(matches!(
            build_pi_write_rejected("denied").result,
            Some(pi_write_exec_result::Result::Rejected(PiWriteExecRejected { ref reason })) if reason == "denied"
        ));
    }

    #[test]
    fn grep_reports_the_internal_match_cap_only_when_no_per_file_cap_did() {
        let capped = json!({ "truncated": true });
        let result = build_pi_grep_result(&tool_result(&[text("a\nb\nc")], false, Some(capped)));
        match result.result {
            Some(pi_grep_exec_result::Result::Success(success)) => {
                assert_eq!(success.match_limit_reached, None);
                let truncation = success.truncation.expect("internal cap truncation");
                assert_eq!(truncation.truncated_by, "matches");
                assert_eq!(truncation.output_lines, 3);
                assert_eq!(truncation.output_bytes, 5);
                assert!(!success.lines_truncated);
            }
            other => panic!("expected success, got {other:?}"),
        }

        let per_file = json!({ "perFileLimitReached": 200, "linesTruncated": true });
        let result = build_pi_grep_result(&tool_result(&[text("a")], false, Some(per_file)));
        match result.result {
            Some(pi_grep_exec_result::Result::Success(success)) => {
                assert_eq!(success.match_limit_reached, Some(200));
                assert!(success.truncation.is_none());
                assert!(success.lines_truncated);
            }
            other => panic!("expected success, got {other:?}"),
        }
    }

    #[test]
    fn find_and_ls_read_the_nested_meta_limit_shape() {
        let nested = json!({ "meta": { "limits": { "resultLimit": { "reached": 100 } } } });
        let find = build_pi_find_result(&tool_result(&[text("f")], false, Some(nested.clone())));
        match find.result {
            Some(pi_find_exec_result::Result::Success(success)) => {
                assert_eq!(success.result_limit_reached, Some(100));
            }
            other => panic!("expected success, got {other:?}"),
        }
        let ls = build_pi_ls_result(&tool_result(&[text("f")], false, Some(nested)));
        match ls.result {
            Some(pi_ls_exec_result::Result::Success(success)) => {
                assert_eq!(success.entry_limit_reached, Some(100));
            }
            other => panic!("expected success, got {other:?}"),
        }
        // A flat `resultLimitReached` wins over the nested shape.
        let flat = json!({ "resultLimitReached": 3, "meta": { "limits": { "resultLimit": { "reached": 100 } } } });
        let ls = build_pi_ls_result(&tool_result(&[text("f")], false, Some(flat)));
        match ls.result {
            Some(pi_ls_exec_result::Result::Success(success)) => assert_eq!(success.entry_limit_reached, Some(3)),
            other => panic!("expected success, got {other:?}"),
        }
    }

    #[test]
    fn bash_error_carries_the_truncation_beside_the_error() {
        let details = json!({ "truncation": { "truncated": true, "truncatedBy": "bytes" } });
        let result = build_pi_bash_result(&tool_result(&[text("boom")], true, Some(details)));
        match result.result {
            Some(pi_bash_exec_result::Result::Error(error)) => {
                assert_eq!(error.error, "boom");
                assert!(error.truncation.is_some_and(|truncation| truncation.truncated_by == "bytes"));
            }
            other => panic!("expected error, got {other:?}"),
        }
        let empty = build_pi_bash_result(&tool_result(&[], true, None));
        match empty.result {
            Some(pi_bash_exec_result::Result::Error(error)) => assert_eq!(error.error, "Command failed"),
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn mcp_state_regroups_tools_by_provider_and_honours_the_filter() {
        let tool = |name: &str, provider: &str| McpToolDefinition {
            name: name.to_owned(),
            provider_identifier: provider.to_owned(),
            tool_name: name.to_owned(),
            ..McpToolDefinition::default()
        };
        let tools = vec![tool("a", "pi-agent"), tool("b", "pi-agent"), tool("c", "other")];

        let all = build_mcp_state_result(&tools, &[]);
        match all.result {
            Some(mcp_state_exec_result::Result::Success(success)) => {
                assert_eq!(success.servers.len(), 2);
                assert_eq!(success.servers[0].server_name, "pi-agent");
                assert_eq!(success.servers[0].tools.len(), 2);
                assert_eq!(success.servers[0].status.as_deref(), Some("connected"));
                assert_eq!(success.servers[1].server_identifier, "other");
            }
            other => panic!("expected success, got {other:?}"),
        }

        let filtered = build_mcp_state_result(&tools, &["other".to_owned()]);
        match filtered.result {
            Some(mcp_state_exec_result::Result::Success(success)) => {
                assert_eq!(success.servers.len(), 1);
                assert_eq!(success.servers[0].server_name, "other");
            }
            other => panic!("expected success, got {other:?}"),
        }
    }

    #[test]
    fn neutral_hook_answers_the_matching_case_and_refuses_unknown_ones() {
        let request = ExecuteHookRequest {
            request: Some(execute_hook_request::Request::PreToolUse(Default::default())),
        };
        let result = build_neutral_hook_result(Some(&request)).expect("modelled case");
        assert!(matches!(
            result.response.and_then(|response| response.response),
            Some(execute_hook_response::Response::PreToolUse(_))
        ));

        assert!(build_neutral_hook_result(None).is_none());
        assert!(build_neutral_hook_result(Some(&ExecuteHookRequest { request: None })).is_none());
    }
}
