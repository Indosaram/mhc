#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetWorkspaceMessagesResponse {
    #[serde(rename = "featureEnabled")]
    pub feature_enabled: bool,
    #[serde(rename = "messages")]
    pub messages: Vec<Box<crate::app_server::protocol::generated::v2::workspace_message::WorkspaceMessage>>,
}
