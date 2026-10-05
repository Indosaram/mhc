#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppScreenshot {
    #[serde(rename = "url", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub url: Option<String>,
    #[serde(rename = "fileId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub file_id: Option<String>,
    #[serde(rename = "userPrompt")]
    pub user_prompt: String,
}
