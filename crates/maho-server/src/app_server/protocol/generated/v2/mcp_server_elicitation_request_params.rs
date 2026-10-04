#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpServerElicitationRequestParamsDetails01Form2 {
    #[serde(rename = "_meta", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub _meta: Option<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
    #[serde(rename = "message")]
    pub message: String,
    #[serde(rename = "requestedSchema")]
    pub requested_schema: Box<crate::app_server::protocol::generated::v2::mcp_elicitation_schema::McpElicitationSchema>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpServerElicitationRequestParamsDetails01OpenaiForm3 {
    #[serde(rename = "_meta", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub _meta: Option<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
    #[serde(rename = "message")]
    pub message: String,
    #[serde(rename = "requestedSchema")]
    pub requested_schema: Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpServerElicitationRequestParamsDetails01Url4 {
    #[serde(rename = "_meta", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub _meta: Option<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
    #[serde(rename = "message")]
    pub message: String,
    #[serde(rename = "url")]
    pub url: String,
    #[serde(rename = "elicitationId")]
    pub elicitation_id: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode")]
pub enum McpServerElicitationRequestParamsDetails01 {
    #[serde(rename = "form")]
    Form(Box<McpServerElicitationRequestParamsDetails01Form2>),
    #[serde(rename = "openai/form")]
    OpenaiForm(Box<McpServerElicitationRequestParamsDetails01OpenaiForm3>),
    #[serde(rename = "url")]
    Url(Box<McpServerElicitationRequestParamsDetails01Url4>),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpServerElicitationRequestParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub turn_id: Option<String>,
    #[serde(rename = "serverName")]
    pub server_name: String,
    #[serde(flatten)]
    pub details: McpServerElicitationRequestParamsDetails01,
}
