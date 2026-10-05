#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginInstallResponse {
    #[serde(rename = "authPolicy")]
    pub auth_policy: Box<crate::app_server::protocol::generated::v2::plugin_auth_policy::PluginAuthPolicy>,
    #[serde(rename = "appsNeedingAuth")]
    pub apps_needing_auth: Vec<Box<crate::app_server::protocol::generated::v2::app_summary::AppSummary>>,
}
