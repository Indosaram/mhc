#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MigrationDetails {
    #[serde(rename = "plugins")]
    pub plugins: Vec<Box<crate::app_server::protocol::generated::v2::plugins_migration::PluginsMigration>>,
    #[serde(rename = "skills")]
    pub skills: Vec<Box<crate::app_server::protocol::generated::v2::skill_migration::SkillMigration>>,
    #[serde(rename = "sessions")]
    pub sessions: Vec<Box<crate::app_server::protocol::generated::v2::session_migration::SessionMigration>>,
    #[serde(rename = "mcpServers")]
    pub mcp_servers: Vec<Box<crate::app_server::protocol::generated::v2::mcp_server_migration::McpServerMigration>>,
    #[serde(rename = "hooks")]
    pub hooks: Vec<Box<crate::app_server::protocol::generated::v2::hook_migration::HookMigration>>,
    #[serde(rename = "subagents")]
    pub subagents: Vec<Box<crate::app_server::protocol::generated::v2::subagent_migration::SubagentMigration>>,
    #[serde(rename = "commands")]
    pub commands: Vec<Box<crate::app_server::protocol::generated::v2::command_migration::CommandMigration>>,
    #[serde(rename = "memory", default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<Vec<String>>,
}
