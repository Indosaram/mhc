#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadSettingsUpdatedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "threadSettings")]
    pub thread_settings: Box<crate::app_server::protocol::generated::v2::thread_settings::ThreadSettings>,
}
