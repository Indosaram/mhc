#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginDetail {
    #[serde(rename = "marketplaceName")]
    pub marketplace_name: String,
    #[serde(rename = "marketplacePath", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub marketplace_path: Option<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>,
    #[serde(rename = "summary")]
    pub summary: Box<crate::app_server::protocol::generated::v2::plugin_summary::PluginSummary>,
    #[serde(rename = "shareUrl", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub share_url: Option<String>,
    #[serde(rename = "description", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub description: Option<String>,
    #[serde(rename = "skills")]
    pub skills: Vec<Box<crate::app_server::protocol::generated::v2::skill_summary::SkillSummary>>,
    #[serde(rename = "hooks")]
    pub hooks: Vec<Box<crate::app_server::protocol::generated::v2::plugin_hook_summary::PluginHookSummary>>,
    #[serde(rename = "apps")]
    pub apps: Vec<Box<crate::app_server::protocol::generated::v2::app_summary::AppSummary>>,
    #[serde(rename = "appTemplates")]
    pub app_templates: Vec<Box<crate::app_server::protocol::generated::v2::app_template_summary::AppTemplateSummary>>,
    #[serde(rename = "mcpServers")]
    pub mcp_servers: Vec<String>,
    #[serde(rename = "scheduledTasks", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub scheduled_tasks: Option<Vec<Box<crate::app_server::protocol::generated::v2::scheduled_task_summary::ScheduledTaskSummary>>>,
}
