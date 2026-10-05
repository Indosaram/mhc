#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginShareContext {
    #[serde(rename = "remotePluginId")]
    pub remote_plugin_id: String,
    #[serde(rename = "remoteVersion", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub remote_version: Option<String>,
    #[serde(rename = "discoverability", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub discoverability: Option<Box<crate::app_server::protocol::generated::v2::plugin_share_discoverability::PluginShareDiscoverability>>,
    #[serde(rename = "shareUrl", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub share_url: Option<String>,
    #[serde(rename = "creatorAccountUserId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub creator_account_user_id: Option<String>,
    #[serde(rename = "creatorName", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub creator_name: Option<String>,
    #[serde(rename = "sharePrincipals", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub share_principals: Option<Vec<Box<crate::app_server::protocol::generated::v2::plugin_share_principal::PluginSharePrincipal>>>,
}
