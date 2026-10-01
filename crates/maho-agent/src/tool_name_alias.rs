//! Port of senpi packages/agent/src/tool-name-alias.ts.

use std::collections::BTreeSet;

use crate::types::{AgentContext, AgentLoopConfig, AgentTool, AgentToolCall, AgentToolResult};
use maho_ai::types::BoxFuture;

/// Some provider wire paths show the model non-native tools as
/// `mcp__<id>__<Name>` (recased, under a namespace senpi never defined), and a
/// model can carry that shape into a call for a tool it learned by its bare
/// name. Resolve such a call only when exactly one available tool matches after
/// stripping the namespace and folding case and `-`/`_` separators; never guess
/// between two candidates.
fn strip_gateway_namespace(requested: &str) -> &str {
    // /^mcp__[^_]+__(.+)$/
    let Some(rest) = requested.strip_prefix("mcp__") else { return requested };
    let Some(index) = rest.find("__") else { return requested };
    let (namespace, remainder) = rest.split_at(index);
    if namespace.is_empty() || namespace.contains('_') || remainder.len() <= 2 {
        return requested;
    }
    &remainder[2..]
}

fn fold_tool_name(name: &str) -> String {
    name.to_lowercase().replace(['-', '_'], "")
}

pub fn resolve_tool_name_alias(requested: &str, available: impl IntoIterator<Item = String>) -> Option<String> {
    let names: Vec<String> = available.into_iter().collect::<BTreeSet<String>>().into_iter().collect();
    if names.iter().any(|name| name == requested) {
        return Some(requested.to_owned());
    }
    let unnamespaced = strip_gateway_namespace(requested);
    if names.iter().any(|name| name == unnamespaced) {
        return Some(unnamespaced.to_owned());
    }
    let key = fold_tool_name(unnamespaced);
    let matches: Vec<&String> = names.iter().filter(|name| fold_tool_name(name) == key).collect();
    match matches.as_slice() {
        [only] => Some((*only).clone()),
        _ => None,
    }
}

pub fn tool_name_correction_notice(requested: &str, resolved: &str) -> String {
    format!("[auto-corrected] no tool is named \"{requested}\"; ran \"{resolved}\". Call tools by their exact listed name.")
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
    let tools = current_context.tools.clone().unwrap_or_default();
    if let Some(exact) = tools.iter().find(|candidate| candidate.name() == tool_call.name).cloned() {
        return Box::pin(async move { Some(exact) });
    }
    let resolver = config.resolve_unknown_tool_call.clone();
    let requested = tool_call.name.clone();
    let context = current_context.clone();
    let aliased = resolve_tool_name_alias(
        &tool_call.name,
        tools.iter().map(|candidate| candidate.name().to_owned()).collect::<Vec<_>>(),
    );
    Box::pin(async move {
        if let Some(resolver) = resolver
            && let Some(resolved) = resolver(requested, context).await
        {
            return Some(resolved);
        }
        let aliased = aliased?;
        tools.into_iter().find(|candidate| candidate.name() == aliased)
    })
}

pub fn with_tool_name_correction(
    result: AgentToolResult,
    requested_name: &str,
    resolved_name: &str,
) -> AgentToolResult {
    let mut content = vec![maho_ai::types::ContentBlock::Text(maho_ai::types::TextContent {
        text: tool_name_correction_notice(requested_name, resolved_name),
        // Model-only: it steers the model back to exact names; the user sees the resolved tool
        // as if called directly.
        audience: Some(maho_ai::types::TextAudience::Model),
        text_signature: None,
    })];
    content.extend(result.content);
    AgentToolResult { content, ..result }
}
