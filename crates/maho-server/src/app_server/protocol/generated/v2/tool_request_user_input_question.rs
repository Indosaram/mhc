#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolRequestUserInputQuestion {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "header")]
    pub header: String,
    #[serde(rename = "question")]
    pub question: String,
    #[serde(rename = "isOther")]
    pub is_other: bool,
    #[serde(rename = "isSecret")]
    pub is_secret: bool,
    #[serde(rename = "options", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub options: Option<Vec<Box<crate::app_server::protocol::generated::v2::tool_request_user_input_option::ToolRequestUserInputOption>>>,
}
