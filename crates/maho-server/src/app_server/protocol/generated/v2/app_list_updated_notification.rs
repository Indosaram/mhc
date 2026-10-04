#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppListUpdatedNotification {
    #[serde(rename = "data")]
    pub data: Vec<Box<crate::app_server::protocol::generated::v2::app_info::AppInfo>>,
}
