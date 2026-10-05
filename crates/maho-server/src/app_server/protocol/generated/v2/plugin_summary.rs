#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginSummary {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "remotePluginId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub remote_plugin_id: Option<String>,
    #[serde(rename = "version", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub version: Option<String>,
    #[serde(rename = "localVersion", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub local_version: Option<String>,
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "shareContext", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub share_context: Option<Box<crate::app_server::protocol::generated::v2::plugin_share_context::PluginShareContext>>,
    #[serde(rename = "source")]
    pub source: Box<crate::app_server::protocol::generated::v2::plugin_source::PluginSource>,
    #[serde(rename = "installed")]
    pub installed: bool,
    #[serde(rename = "enabled")]
    pub enabled: bool,
    #[serde(rename = "installPolicy")]
    pub install_policy: Box<crate::app_server::protocol::generated::v2::plugin_install_policy::PluginInstallPolicy>,
    #[serde(rename = "installPolicySource", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub install_policy_source: Option<Box<crate::app_server::protocol::generated::v2::plugin_install_policy_source::PluginInstallPolicySource>>,
    #[serde(rename = "mustShowInstallationInterstitial", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub must_show_installation_interstitial: Option<bool>,
    #[serde(rename = "authPolicy")]
    pub auth_policy: Box<crate::app_server::protocol::generated::v2::plugin_auth_policy::PluginAuthPolicy>,
    #[serde(rename = "availability")]
    pub availability: Box<crate::app_server::protocol::generated::v2::plugin_availability::PluginAvailability>,
    #[serde(rename = "interface", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub interface: Option<Box<crate::app_server::protocol::generated::v2::plugin_interface::PluginInterface>>,
    #[serde(rename = "keywords")]
    pub keywords: Vec<String>,
}
