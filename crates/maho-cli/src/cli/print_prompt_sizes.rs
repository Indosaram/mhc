//! Port of senpi `packages/coding-agent/src/cli/print-prompt-sizes.ts`.

use super::prompt_size_inventory::CliPromptSizeInventory;
use maho_core::agent_session::AgentSession;
use std::sync::Arc;

/// Print the first-turn size report as JSON and never open a turn. The inventory is the CLI's own
/// filesystem reader: it resolves extension package names and configured MCP servers without
/// attaching them, so this stays free of model calls and network work.
pub async fn print_prompt_sizes(session: &AgentSession) -> Result<serde_json::Value, String> {
    let inventory = Arc::new(CliPromptSizeInventory);
    maho_core::cli::print_prompt_sizes::print_prompt_sizes(session, Some(inventory)).await
}
