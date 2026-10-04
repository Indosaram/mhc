#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DynamicToolCallResponse {
    #[serde(rename = "contentItems")]
    pub content_items: Vec<Box<crate::app_server::protocol::generated::v2::dynamic_tool_call_output_content_item::DynamicToolCallOutputContentItem>>,
    #[serde(rename = "success")]
    pub success: bool,
}
