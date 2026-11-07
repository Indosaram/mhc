//! Port of senpi `packages/coding-agent/src/cli/print-prompt-sizes.ts`.

use crate::agent_session::AgentSession;
use crate::prompt_size_inventory::PromptSizeInventory;
use std::sync::Arc;

/// Report the sizes of the prompt this session would send; never starts a turn.
///
/// This takes the session rather than an `AgentSessionRuntime`: the single-session CLI path
/// (`cli::runtime::run`) holds an `AgentSession` and never builds a runtime, which contributed only
/// `session()` here. The runtime variant also refused a session whose *mount* reported an error
/// diagnostic. That check has no counterpart on this path: the error diagnostics live on
/// `options.diagnostics`, and `run` returns `Err("Invalid model selection")` before creating the
/// session when any of them is an error, while `CreateAgentSessionResult` carries no diagnostics
/// field at all. Keeping the check would have required synthesising an empty list and a branch that
/// can never run.
pub async fn print_prompt_sizes(
    session: &AgentSession,
    inventory: Option<Arc<dyn PromptSizeInventory>>,
) -> Result<serde_json::Value, String> {
    if session.model().id.is_empty() {
        return Ok(serde_json::json!({
            "error": { "reason": "no model selected" }
        }));
    }
    session
        .bind_extensions(crate::agent_session::ExtensionBindings {
            mode: Some(maho_ext_api::ExtensionMode::Print),
            sizes_only: true,
            prompt_size_inventory: inventory,
            ..Default::default()
        })
        .await;

    let model = session.model();
    let system_prompt = session.system_prompt();
    let prompt_bytes = system_prompt.len();
    Ok(serde_json::json!({
        "model": model.id,
        "provider": model.provider,
        "systemPromptBytes": prompt_bytes,
        "contextWindow": model.context_window,
    }))
}
