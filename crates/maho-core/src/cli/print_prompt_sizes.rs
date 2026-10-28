//! Port of senpi `packages/coding-agent/src/cli/print-prompt-sizes.ts`.

use crate::agent_session_runtime::AgentSessionRuntime;
use crate::prompt_size_inventory::PromptSizeInventory;
use std::sync::Arc;

/// A full session open, followed only by preview composition; never starts a turn.
pub async fn print_prompt_sizes(
    runtime: &AgentSessionRuntime,
    inventory: Option<Arc<dyn PromptSizeInventory>>,
) -> Result<serde_json::Value, String> {
    if runtime.session().model().id.is_empty() {
        return Ok(serde_json::json!({
            "error": { "reason": "no model selected" }
        }));
    }
    if runtime.diagnostics().iter().any(|d| d.kind == crate::agent_session_services::AgentSessionRuntimeDiagnosticType::Error) {
        return Ok(serde_json::json!({
            "error": { "reason": "session initialization failed" }
        }));
    }
    runtime
        .session()
        .bind_extensions(crate::agent_session::ExtensionBindings {
            mode: Some(maho_ext_api::ExtensionMode::Print),
            sizes_only: true,
            prompt_size_inventory: inventory,
            ..Default::default()
        })
        .await;

    let model = runtime.session().model();
    let system_prompt = runtime.session().system_prompt();
    let prompt_bytes = system_prompt.len();
    Ok(serde_json::json!({
        "model": model.id,
        "provider": model.provider,
        "systemPromptBytes": prompt_bytes,
        "contextWindow": model.context_window,
    }))
}
