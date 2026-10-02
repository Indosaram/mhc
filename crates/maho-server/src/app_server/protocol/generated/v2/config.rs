pub type ConfigDesktop1 = std::collections::BTreeMap<String, Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>;

pub type ConfigDetails02Value3Variant44 = std::collections::BTreeMap<String, Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ConfigDetails02Value3 {
    Variant0(Box<f64>),
    Variant1(Box<String>),
    Variant2(Box<bool>),
    Variant3(Box<Vec<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>>),
    Variant4(Box<ConfigDetails02Value3Variant44>),
}

pub type ConfigDetails02 = std::collections::BTreeMap<String, Option<ConfigDetails02Value3>>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Config {
    #[serde(rename = "model", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub model: Option<String>,
    #[serde(rename = "review_model", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub review_model: Option<String>,
    #[serde(rename = "model_context_window", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub model_context_window: Option<i64>,
    #[serde(rename = "model_auto_compact_token_limit", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub model_auto_compact_token_limit: Option<i64>,
    #[serde(rename = "model_auto_compact_token_limit_scope", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub model_auto_compact_token_limit_scope: Option<Box<crate::app_server::protocol::generated::auto_compact_token_limit_scope::AutoCompactTokenLimitScope>>,
    #[serde(rename = "model_provider", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub model_provider: Option<String>,
    #[serde(rename = "approval_policy", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub approval_policy: Option<Box<crate::app_server::protocol::generated::v2::ask_for_approval::AskForApproval>>,
    #[serde(rename = "approvals_reviewer", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub approvals_reviewer: Option<Box<crate::app_server::protocol::generated::v2::approvals_reviewer::ApprovalsReviewer>>,
    #[serde(rename = "sandbox_mode", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub sandbox_mode: Option<Box<crate::app_server::protocol::generated::v2::sandbox_mode::SandboxMode>>,
    #[serde(rename = "sandbox_workspace_write", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub sandbox_workspace_write: Option<Box<crate::app_server::protocol::generated::v2::sandbox_workspace_write::SandboxWorkspaceWrite>>,
    #[serde(rename = "forced_chatgpt_workspace_id", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub forced_chatgpt_workspace_id: Option<Box<crate::app_server::protocol::generated::v2::forced_chatgpt_workspace_ids::ForcedChatgptWorkspaceIds>>,
    #[serde(rename = "forced_login_method", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub forced_login_method: Option<Box<crate::app_server::protocol::generated::forced_login_method::ForcedLoginMethod>>,
    #[serde(rename = "web_search", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub web_search: Option<Box<crate::app_server::protocol::generated::web_search_mode::WebSearchMode>>,
    #[serde(rename = "tools", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub tools: Option<Box<crate::app_server::protocol::generated::v2::tools_v2::ToolsV2>>,
    #[serde(rename = "instructions", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub instructions: Option<String>,
    #[serde(rename = "developer_instructions", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub developer_instructions: Option<String>,
    #[serde(rename = "compact_prompt", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub compact_prompt: Option<String>,
    #[serde(rename = "model_reasoning_effort", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub model_reasoning_effort: Option<Box<crate::app_server::protocol::generated::reasoning_effort::ReasoningEffort>>,
    #[serde(rename = "model_reasoning_summary", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub model_reasoning_summary: Option<Box<crate::app_server::protocol::generated::reasoning_summary::ReasoningSummary>>,
    #[serde(rename = "model_verbosity", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub model_verbosity: Option<Box<crate::app_server::protocol::generated::verbosity::Verbosity>>,
    #[serde(rename = "service_tier", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub service_tier: Option<String>,
    #[serde(rename = "analytics", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub analytics: Option<Box<crate::app_server::protocol::generated::v2::analytics_config::AnalyticsConfig>>,
    #[serde(rename = "desktop", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub desktop: Option<ConfigDesktop1>,
    #[serde(flatten)]
    pub details: ConfigDetails02,
}
