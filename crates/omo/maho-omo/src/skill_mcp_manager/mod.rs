//! Project-scoped skill MCP clients and durable environment grants.
pub mod cleanup;
pub mod connection;
pub mod env_grant_store;
pub mod env_grants_store;
pub mod environment_grant_policy;
pub mod http_client;
pub mod index;
pub mod manager;
pub mod policy_connection;
pub mod project_env_policy;
pub mod stdio_client;
#[cfg(test)]
mod tests;
pub mod types;

pub use manager::{SkillMcpClientOptions, SkillMcpManager, build_skill_mcp_client_key};
pub use types::{SkillMcpClientInfo, SkillMcpServerContext};
pub type Result<T> = std::result::Result<T, String>;
