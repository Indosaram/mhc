//! Native Claude Code compatibility loaders.

pub mod features;
pub mod shared;

#[cfg(test)]
mod test_support;
#[cfg(test)]
pub(crate) static TEST_PROCESS_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub use features::claude_code_mcp_loader;
pub use features::claude_code_mcp_loader::{
    configure_allowed_env_vars, env_expander, transformer,
    configure_allowed_env_vars::*, env_expander::*, loader::*,
    scope_filter::should_load_mcp_server, transformer::*, types::*,
};
pub use features::claude_code_plugin_loader;
pub use features::claude_code_plugin_loader::*;

pub use features::claude_code_agent_loader::{
    AgentFrontmatter, AgentJsonDefinition, AgentMode, AgentModel, AgentScope,
    ClaudeCodeAgentConfig, LoadedAgent, load_agent_definitions, load_agents_from_dir,
    load_opencode_global_agents, load_opencode_project_agents, load_project_agents,
    load_user_agents, load_native_agents, parse_json_agent_file, parse_markdown_agent_file,
    read_opencode_config_agents,
};
pub use features::claude_code_command_loader::{
    CommandDefinition, CommandFrontmatter, CommandRecord, CommandScope, HandoffDefinition,
    LoadedCommand, clear_command_loader_cache, commands_to_record,
    deduplicate_loaded_commands_by_name, load_all_commands, load_commands_from_dir,
    load_opencode_global_commands, load_opencode_project_commands, load_project_commands,
    load_user_commands,
};
