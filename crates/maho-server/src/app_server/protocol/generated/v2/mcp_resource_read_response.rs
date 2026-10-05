#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpResourceReadResponse {
    #[serde(rename = "contents")]
    pub contents: Vec<Box<crate::app_server::protocol::generated::resource_content::ResourceContent>>,
}
