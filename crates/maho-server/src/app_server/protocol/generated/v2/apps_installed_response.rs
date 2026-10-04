#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppsInstalledResponse {
    #[serde(rename = "apps")]
    pub apps: Vec<Box<crate::app_server::protocol::generated::v2::installed_app::InstalledApp>>,
}
