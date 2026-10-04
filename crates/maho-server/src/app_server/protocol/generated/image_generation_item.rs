#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImageGenerationItem {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "status")]
    pub status: String,
    #[serde(rename = "revisedPrompt", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub revised_prompt: Option<String>,
    #[serde(rename = "result")]
    pub result: String,
    #[serde(rename = "savedPath", default, skip_serializing_if = "Option::is_none")]
    pub saved_path: Option<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>,
}
