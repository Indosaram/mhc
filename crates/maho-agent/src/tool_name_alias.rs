//! Port of senpi packages/agent/src/tool-name-alias.ts.

use crate::types::{AgentContext, AgentLoopConfig, AgentTool, AgentToolCall, AgentToolResult};
use maho_ai::types::BoxFuture;

pub fn resolve_tool_name_alias(
    requested: &str,
    available: impl IntoIterator<Item = String>,
) -> Option<String> {
    maho_ai::types::tool_name_match::resolve_tool_name_match(requested, available)
}

pub fn tool_name_correction_notice(requested: &str, resolved: &str) -> String {
    format!(
        "[auto-corrected] no tool is named \"{requested}\"; ran \"{resolved}\". Call tools by their exact listed name."
    )
}

/// Find the tool a call will run, resolving an unknown name through the host
/// resolver and then the alias rule. Runs before `tool_execution_start` so every
/// event names the tool that executes, never the name the model mistyped.
pub fn resolve_call_tool(
    current_context: &AgentContext,
    tool_call: &AgentToolCall,
    config: &AgentLoopConfig,
) -> BoxFuture<'static, Option<AgentTool>> {
    if tool_call.incomplete == Some(true) {
        return Box::pin(async { None });
    }
    if let Some(exact) = current_context
        .tools
        .as_ref()
        .and_then(|tools| {
            tools
                .iter()
                .find(|candidate| candidate.name() == tool_call.name)
        })
        .cloned()
    {
        return Box::pin(async move { Some(exact) });
    }
    let tools = current_context.tools.clone().unwrap_or_default();
    let resolver = config.resolve_unknown_tool_call.clone();
    let requested = tool_call.name.clone();
    let context = current_context.clone();
    let aliased = resolve_tool_name_alias(
        &tool_call.name,
        tools
            .iter()
            .map(|candidate| candidate.name().to_owned())
            .collect::<Vec<_>>(),
    );
    Box::pin(async move {
        if let Some(resolver) = resolver
            && let Some(resolved) = resolver(requested, context).await
        {
            return Some(resolved);
        }
        let aliased = aliased?;
        tools
            .into_iter()
            .find(|candidate| candidate.name() == aliased)
    })
}

pub fn with_tool_name_correction(
    result: AgentToolResult,
    requested_name: &str,
    resolved_name: &str,
) -> AgentToolResult {
    let mut content = vec![maho_ai::types::ContentBlock::Text(
        maho_ai::types::TextContent {
            text: tool_name_correction_notice(requested_name, resolved_name),
            // Model-only: it steers the model back to exact names; the user sees the resolved tool
            // as if called directly.
            audience: Some(maho_ai::types::TextAudience::Model),
            text_signature: None,
        },
    )];
    content.extend(result.content);
    AgentToolResult { content, ..result }
}

#[cfg(test)]
mod tests {
    use super::resolve_tool_name_alias;

    #[test]
    fn resolves_mixed_case_namespaced_forms_and_refuses_ambiguity() {
        assert_eq!(
            resolve_tool_name_alias("mcp__gateway__Read-File", ["read_file".into()]),
            Some("read_file".into())
        );
        assert_eq!(
            resolve_tool_name_alias("mcp__gateway__READ", ["read".into(), "Read".into()]),
            None
        );
    }
}
