#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadItemVariant01Type2 {
    #[serde(rename = "userMessage")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemVariant01 {
    #[serde(rename = "type")]
    pub r#type: ThreadItemVariant01Type2,
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "clientId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub client_id: Option<String>,
    #[serde(rename = "content")]
    pub content: Vec<Box<crate::app_server::protocol::generated::v2::user_input::UserInput>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadItemVariant13Type4 {
    #[serde(rename = "hookPrompt")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemVariant13 {
    #[serde(rename = "type")]
    pub r#type: ThreadItemVariant13Type4,
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "fragments")]
    pub fragments: Vec<Box<crate::app_server::protocol::generated::v2::hook_prompt_fragment::HookPromptFragment>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadItemVariant25Type6 {
    #[serde(rename = "agentMessage")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemVariant25 {
    #[serde(rename = "type")]
    pub r#type: ThreadItemVariant25Type6,
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "text")]
    pub text: String,
    #[serde(rename = "phase", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub phase: Option<Box<crate::app_server::protocol::generated::message_phase::MessagePhase>>,
    #[serde(rename = "memoryCitation", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub memory_citation: Option<Box<crate::app_server::protocol::generated::v2::memory_citation::MemoryCitation>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadItemVariant37Type8 {
    #[serde(rename = "plan")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemVariant37 {
    #[serde(rename = "type")]
    pub r#type: ThreadItemVariant37Type8,
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "text")]
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadItemVariant49Type10 {
    #[serde(rename = "reasoning")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemVariant49 {
    #[serde(rename = "type")]
    pub r#type: ThreadItemVariant49Type10,
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "summary")]
    pub summary: Vec<String>,
    #[serde(rename = "content")]
    pub content: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadItemVariant511Type12 {
    #[serde(rename = "commandExecution")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemVariant511 {
    #[serde(rename = "type")]
    pub r#type: ThreadItemVariant511Type12,
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "command")]
    pub command: String,
    #[serde(rename = "cwd")]
    pub cwd: Box<crate::app_server::protocol::generated::legacy_app_path_string::LegacyAppPathString>,
    #[serde(rename = "processId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub process_id: Option<String>,
    #[serde(rename = "source")]
    pub source: Box<crate::app_server::protocol::generated::v2::command_execution_source::CommandExecutionSource>,
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::command_execution_status::CommandExecutionStatus>,
    #[serde(rename = "commandActions")]
    pub command_actions: Vec<Box<crate::app_server::protocol::generated::v2::command_action::CommandAction>>,
    #[serde(rename = "aggregatedOutput", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub aggregated_output: Option<String>,
    #[serde(rename = "exitCode", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub exit_code: Option<f64>,
    #[serde(rename = "durationMs", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub duration_ms: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadItemVariant613Type14 {
    #[serde(rename = "fileChange")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemVariant613 {
    #[serde(rename = "type")]
    pub r#type: ThreadItemVariant613Type14,
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "changes")]
    pub changes: Vec<Box<crate::app_server::protocol::generated::v2::file_update_change::FileUpdateChange>>,
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::patch_apply_status::PatchApplyStatus>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadItemVariant715Type16 {
    #[serde(rename = "mcpToolCall")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemVariant715 {
    #[serde(rename = "type")]
    pub r#type: ThreadItemVariant715Type16,
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "server")]
    pub server: String,
    #[serde(rename = "tool")]
    pub tool: String,
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::mcp_tool_call_status::McpToolCallStatus>,
    #[serde(rename = "arguments")]
    pub arguments: Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>,
    #[serde(rename = "appContext", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub app_context: Option<Box<crate::app_server::protocol::generated::v2::mcp_tool_call_app_context::McpToolCallAppContext>>,
    #[serde(rename = "mcpAppResourceUri", default, skip_serializing_if = "Option::is_none")]
    pub mcp_app_resource_uri: Option<String>,
    #[serde(rename = "pluginId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub plugin_id: Option<String>,
    #[serde(rename = "result", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub result: Option<Box<crate::app_server::protocol::generated::v2::mcp_tool_call_result::McpToolCallResult>>,
    #[serde(rename = "error", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub error: Option<Box<crate::app_server::protocol::generated::v2::mcp_tool_call_error::McpToolCallError>>,
    #[serde(rename = "durationMs", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub duration_ms: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadItemVariant817Type18 {
    #[serde(rename = "dynamicToolCall")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemVariant817 {
    #[serde(rename = "type")]
    pub r#type: ThreadItemVariant817Type18,
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "namespace", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub namespace: Option<String>,
    #[serde(rename = "tool")]
    pub tool: String,
    #[serde(rename = "arguments")]
    pub arguments: Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>,
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::dynamic_tool_call_status::DynamicToolCallStatus>,
    #[serde(rename = "contentItems", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub content_items: Option<Vec<Box<crate::app_server::protocol::generated::v2::dynamic_tool_call_output_content_item::DynamicToolCallOutputContentItem>>>,
    #[serde(rename = "success", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub success: Option<bool>,
    #[serde(rename = "durationMs", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub duration_ms: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadItemVariant919Type20 {
    #[serde(rename = "collabAgentToolCall")]
    Value,
}

pub type ThreadItemVariant919AgentsStates21 = std::collections::BTreeMap<String, Box<crate::app_server::protocol::generated::v2::collab_agent_state::CollabAgentState>>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemVariant919 {
    #[serde(rename = "type")]
    pub r#type: ThreadItemVariant919Type20,
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "tool")]
    pub tool: Box<crate::app_server::protocol::generated::v2::collab_agent_tool::CollabAgentTool>,
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::collab_agent_tool_call_status::CollabAgentToolCallStatus>,
    #[serde(rename = "senderThreadId")]
    pub sender_thread_id: String,
    #[serde(rename = "receiverThreadIds")]
    pub receiver_thread_ids: Vec<String>,
    #[serde(rename = "prompt", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub prompt: Option<String>,
    #[serde(rename = "model", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub model: Option<String>,
    #[serde(rename = "reasoningEffort", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub reasoning_effort: Option<Box<crate::app_server::protocol::generated::reasoning_effort::ReasoningEffort>>,
    #[serde(rename = "agentsStates")]
    pub agents_states: ThreadItemVariant919AgentsStates21,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadItemVariant1022Type23 {
    #[serde(rename = "subAgentActivity")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemVariant1022 {
    #[serde(rename = "type")]
    pub r#type: ThreadItemVariant1022Type23,
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "kind")]
    pub kind: Box<crate::app_server::protocol::generated::v2::sub_agent_activity_kind::SubAgentActivityKind>,
    #[serde(rename = "agentThreadId")]
    pub agent_thread_id: String,
    #[serde(rename = "agentPath")]
    pub agent_path: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadItemVariant1124Type25 {
    #[serde(rename = "webSearch")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemVariant1124 {
    #[serde(rename = "type")]
    pub r#type: ThreadItemVariant1124Type25,
    #[serde(flatten)]
    pub details: Box<crate::app_server::protocol::generated::web_search_item::WebSearchItem>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadItemVariant1226Type27 {
    #[serde(rename = "imageView")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemVariant1226 {
    #[serde(rename = "type")]
    pub r#type: ThreadItemVariant1226Type27,
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "path")]
    pub path: Box<crate::app_server::protocol::generated::legacy_app_path_string::LegacyAppPathString>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadItemVariant1328Type29 {
    #[serde(rename = "sleep")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemVariant1328 {
    #[serde(rename = "type")]
    pub r#type: ThreadItemVariant1328Type29,
    #[serde(flatten)]
    pub details: Box<crate::app_server::protocol::generated::sleep_item::SleepItem>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadItemVariant1430Type31 {
    #[serde(rename = "imageGeneration")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemVariant1430 {
    #[serde(rename = "type")]
    pub r#type: ThreadItemVariant1430Type31,
    #[serde(flatten)]
    pub details: Box<crate::app_server::protocol::generated::image_generation_item::ImageGenerationItem>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadItemVariant1532Type33 {
    #[serde(rename = "enteredReviewMode")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemVariant1532 {
    #[serde(rename = "type")]
    pub r#type: ThreadItemVariant1532Type33,
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "review")]
    pub review: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadItemVariant1634Type35 {
    #[serde(rename = "exitedReviewMode")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemVariant1634 {
    #[serde(rename = "type")]
    pub r#type: ThreadItemVariant1634Type35,
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "review")]
    pub review: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadItemVariant1736Type37 {
    #[serde(rename = "contextCompaction")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemVariant1736 {
    #[serde(rename = "type")]
    pub r#type: ThreadItemVariant1736Type37,
    #[serde(rename = "id")]
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ThreadItem {
    Variant0(Box<ThreadItemVariant01>),
    Variant1(Box<ThreadItemVariant13>),
    Variant2(Box<ThreadItemVariant25>),
    Variant3(Box<ThreadItemVariant37>),
    Variant4(Box<ThreadItemVariant49>),
    Variant5(Box<ThreadItemVariant511>),
    Variant6(Box<ThreadItemVariant613>),
    Variant7(Box<ThreadItemVariant715>),
    Variant8(Box<ThreadItemVariant817>),
    Variant9(Box<ThreadItemVariant919>),
    Variant10(Box<ThreadItemVariant1022>),
    Variant11(Box<ThreadItemVariant1124>),
    Variant12(Box<ThreadItemVariant1226>),
    Variant13(Box<ThreadItemVariant1328>),
    Variant14(Box<ThreadItemVariant1430>),
    Variant15(Box<ThreadItemVariant1532>),
    Variant16(Box<ThreadItemVariant1634>),
    Variant17(Box<ThreadItemVariant1736>),
}
