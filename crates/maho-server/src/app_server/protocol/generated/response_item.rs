#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResponseItemMessage1 {
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Box<crate::app_server::protocol::generated::response_item_id::ResponseItemId>>,
    #[serde(rename = "role")]
    pub role: String,
    #[serde(rename = "content")]
    pub content: Vec<Box<crate::app_server::protocol::generated::content_item::ContentItem>>,
    #[serde(rename = "phase", default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<Box<crate::app_server::protocol::generated::message_phase::MessagePhase>>,
    #[serde(rename = "internal_chat_message_metadata_passthrough", default, skip_serializing_if = "Option::is_none")]
    pub internal_chat_message_metadata_passthrough: Option<Box<crate::app_server::protocol::generated::internal_chat_message_metadata_passthrough::InternalChatMessageMetadataPassthrough>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResponseItemAgentMessage2 {
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Box<crate::app_server::protocol::generated::response_item_id::ResponseItemId>>,
    #[serde(rename = "author")]
    pub author: String,
    #[serde(rename = "recipient")]
    pub recipient: String,
    #[serde(rename = "content")]
    pub content: Vec<Box<crate::app_server::protocol::generated::agent_message_input_content::AgentMessageInputContent>>,
    #[serde(rename = "internal_chat_message_metadata_passthrough", default, skip_serializing_if = "Option::is_none")]
    pub internal_chat_message_metadata_passthrough: Option<Box<crate::app_server::protocol::generated::internal_chat_message_metadata_passthrough::InternalChatMessageMetadataPassthrough>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResponseItemReasoning3 {
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Box<crate::app_server::protocol::generated::response_item_id::ResponseItemId>>,
    #[serde(rename = "summary")]
    pub summary: Vec<Box<crate::app_server::protocol::generated::reasoning_item_reasoning_summary::ReasoningItemReasoningSummary>>,
    #[serde(rename = "content", default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<Box<crate::app_server::protocol::generated::reasoning_item_content::ReasoningItemContent>>>,
    #[serde(rename = "encrypted_content", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub encrypted_content: Option<String>,
    #[serde(rename = "internal_chat_message_metadata_passthrough", default, skip_serializing_if = "Option::is_none")]
    pub internal_chat_message_metadata_passthrough: Option<Box<crate::app_server::protocol::generated::internal_chat_message_metadata_passthrough::InternalChatMessageMetadataPassthrough>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResponseItemLocalShellCall4 {
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Box<crate::app_server::protocol::generated::response_item_id::ResponseItemId>>,
    #[serde(rename = "call_id", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub call_id: Option<String>,
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::local_shell_status::LocalShellStatus>,
    #[serde(rename = "action")]
    pub action: Box<crate::app_server::protocol::generated::local_shell_action::LocalShellAction>,
    #[serde(rename = "internal_chat_message_metadata_passthrough", default, skip_serializing_if = "Option::is_none")]
    pub internal_chat_message_metadata_passthrough: Option<Box<crate::app_server::protocol::generated::internal_chat_message_metadata_passthrough::InternalChatMessageMetadataPassthrough>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResponseItemFunctionCall5 {
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Box<crate::app_server::protocol::generated::response_item_id::ResponseItemId>>,
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "namespace", default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(rename = "arguments")]
    pub arguments: String,
    #[serde(rename = "call_id")]
    pub call_id: String,
    #[serde(rename = "internal_chat_message_metadata_passthrough", default, skip_serializing_if = "Option::is_none")]
    pub internal_chat_message_metadata_passthrough: Option<Box<crate::app_server::protocol::generated::internal_chat_message_metadata_passthrough::InternalChatMessageMetadataPassthrough>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResponseItemToolSearchCall6 {
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Box<crate::app_server::protocol::generated::response_item_id::ResponseItemId>>,
    #[serde(rename = "call_id", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub call_id: Option<String>,
    #[serde(rename = "status", default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(rename = "execution")]
    pub execution: String,
    #[serde(rename = "arguments")]
    pub arguments: serde_json::Value,
    #[serde(rename = "internal_chat_message_metadata_passthrough", default, skip_serializing_if = "Option::is_none")]
    pub internal_chat_message_metadata_passthrough: Option<Box<crate::app_server::protocol::generated::internal_chat_message_metadata_passthrough::InternalChatMessageMetadataPassthrough>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResponseItemFunctionCallOutput7 {
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Box<crate::app_server::protocol::generated::response_item_id::ResponseItemId>>,
    #[serde(rename = "call_id")]
    pub call_id: String,
    #[serde(rename = "output")]
    pub output: Box<crate::app_server::protocol::generated::function_call_output_body::FunctionCallOutputBody>,
    #[serde(rename = "internal_chat_message_metadata_passthrough", default, skip_serializing_if = "Option::is_none")]
    pub internal_chat_message_metadata_passthrough: Option<Box<crate::app_server::protocol::generated::internal_chat_message_metadata_passthrough::InternalChatMessageMetadataPassthrough>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResponseItemCustomToolCall8 {
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Box<crate::app_server::protocol::generated::response_item_id::ResponseItemId>>,
    #[serde(rename = "status", default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(rename = "call_id")]
    pub call_id: String,
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "namespace", default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(rename = "input")]
    pub input: String,
    #[serde(rename = "internal_chat_message_metadata_passthrough", default, skip_serializing_if = "Option::is_none")]
    pub internal_chat_message_metadata_passthrough: Option<Box<crate::app_server::protocol::generated::internal_chat_message_metadata_passthrough::InternalChatMessageMetadataPassthrough>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResponseItemCustomToolCallOutput9 {
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Box<crate::app_server::protocol::generated::response_item_id::ResponseItemId>>,
    #[serde(rename = "call_id")]
    pub call_id: String,
    #[serde(rename = "name", default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "output")]
    pub output: Box<crate::app_server::protocol::generated::function_call_output_body::FunctionCallOutputBody>,
    #[serde(rename = "internal_chat_message_metadata_passthrough", default, skip_serializing_if = "Option::is_none")]
    pub internal_chat_message_metadata_passthrough: Option<Box<crate::app_server::protocol::generated::internal_chat_message_metadata_passthrough::InternalChatMessageMetadataPassthrough>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResponseItemToolSearchOutput10 {
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Box<crate::app_server::protocol::generated::response_item_id::ResponseItemId>>,
    #[serde(rename = "call_id", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub call_id: Option<String>,
    #[serde(rename = "status")]
    pub status: String,
    #[serde(rename = "execution")]
    pub execution: String,
    #[serde(rename = "tools")]
    pub tools: Vec<serde_json::Value>,
    #[serde(rename = "internal_chat_message_metadata_passthrough", default, skip_serializing_if = "Option::is_none")]
    pub internal_chat_message_metadata_passthrough: Option<Box<crate::app_server::protocol::generated::internal_chat_message_metadata_passthrough::InternalChatMessageMetadataPassthrough>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResponseItemWebSearchCall11 {
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Box<crate::app_server::protocol::generated::response_item_id::ResponseItemId>>,
    #[serde(rename = "status", default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(rename = "action", default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Box<crate::app_server::protocol::generated::web_search_action::WebSearchAction>>,
    #[serde(rename = "internal_chat_message_metadata_passthrough", default, skip_serializing_if = "Option::is_none")]
    pub internal_chat_message_metadata_passthrough: Option<Box<crate::app_server::protocol::generated::internal_chat_message_metadata_passthrough::InternalChatMessageMetadataPassthrough>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResponseItemImageGenerationCall12 {
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Box<crate::app_server::protocol::generated::response_item_id::ResponseItemId>>,
    #[serde(rename = "status")]
    pub status: String,
    #[serde(rename = "revised_prompt", default, skip_serializing_if = "Option::is_none")]
    pub revised_prompt: Option<String>,
    #[serde(rename = "result")]
    pub result: String,
    #[serde(rename = "internal_chat_message_metadata_passthrough", default, skip_serializing_if = "Option::is_none")]
    pub internal_chat_message_metadata_passthrough: Option<Box<crate::app_server::protocol::generated::internal_chat_message_metadata_passthrough::InternalChatMessageMetadataPassthrough>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResponseItemCompaction13 {
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Box<crate::app_server::protocol::generated::response_item_id::ResponseItemId>>,
    #[serde(rename = "encrypted_content")]
    pub encrypted_content: String,
    #[serde(rename = "internal_chat_message_metadata_passthrough", default, skip_serializing_if = "Option::is_none")]
    pub internal_chat_message_metadata_passthrough: Option<Box<crate::app_server::protocol::generated::internal_chat_message_metadata_passthrough::InternalChatMessageMetadataPassthrough>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResponseItemCompactionTrigger14 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResponseItemContextCompaction15 {
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Box<crate::app_server::protocol::generated::response_item_id::ResponseItemId>>,
    #[serde(rename = "encrypted_content", default, skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,
    #[serde(rename = "internal_chat_message_metadata_passthrough", default, skip_serializing_if = "Option::is_none")]
    pub internal_chat_message_metadata_passthrough: Option<Box<crate::app_server::protocol::generated::internal_chat_message_metadata_passthrough::InternalChatMessageMetadataPassthrough>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResponseItemOther16 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum ResponseItem {
    #[serde(rename = "message")]
    Message(Box<ResponseItemMessage1>),
    #[serde(rename = "agent_message")]
    AgentMessage(Box<ResponseItemAgentMessage2>),
    #[serde(rename = "reasoning")]
    Reasoning(Box<ResponseItemReasoning3>),
    #[serde(rename = "local_shell_call")]
    LocalShellCall(Box<ResponseItemLocalShellCall4>),
    #[serde(rename = "function_call")]
    FunctionCall(Box<ResponseItemFunctionCall5>),
    #[serde(rename = "tool_search_call")]
    ToolSearchCall(Box<ResponseItemToolSearchCall6>),
    #[serde(rename = "function_call_output")]
    FunctionCallOutput(Box<ResponseItemFunctionCallOutput7>),
    #[serde(rename = "custom_tool_call")]
    CustomToolCall(Box<ResponseItemCustomToolCall8>),
    #[serde(rename = "custom_tool_call_output")]
    CustomToolCallOutput(Box<ResponseItemCustomToolCallOutput9>),
    #[serde(rename = "tool_search_output")]
    ToolSearchOutput(Box<ResponseItemToolSearchOutput10>),
    #[serde(rename = "web_search_call")]
    WebSearchCall(Box<ResponseItemWebSearchCall11>),
    #[serde(rename = "image_generation_call")]
    ImageGenerationCall(Box<ResponseItemImageGenerationCall12>),
    #[serde(rename = "compaction")]
    Compaction(Box<ResponseItemCompaction13>),
    #[serde(rename = "compaction_trigger")]
    CompactionTrigger(Box<ResponseItemCompactionTrigger14>),
    #[serde(rename = "context_compaction")]
    ContextCompaction(Box<ResponseItemContextCompaction15>),
    #[serde(rename = "other")]
    Other(Box<ResponseItemOther16>),
}
