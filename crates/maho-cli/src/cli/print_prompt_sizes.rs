//! Port of senpi `packages/coding-agent/src/cli/print-prompt-sizes.ts`.

use super::prompt_size_inventory::CliPromptSizeInventory;
use maho_core::agent_session_runtime::AgentSessionRuntime;
use std::sync::Arc;

/// A full session open, followed only by preview composition; never starts a turn.
pub async fn print_prompt_sizes(
    runtime: &AgentSessionRuntime,
) -> Result<serde_json::Value, String> {
    let inventory = Arc::new(CliPromptSizeInventory);
    maho_core::cli::print_prompt_sizes::print_prompt_sizes(runtime, Some(inventory)).await
}
