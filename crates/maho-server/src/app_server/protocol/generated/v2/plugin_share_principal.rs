#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginSharePrincipal {
    #[serde(rename = "principalType")]
    pub principal_type: Box<crate::app_server::protocol::generated::v2::plugin_share_principal_type::PluginSharePrincipalType>,
    #[serde(rename = "principalId")]
    pub principal_id: String,
    #[serde(rename = "role")]
    pub role: Box<crate::app_server::protocol::generated::v2::plugin_share_principal_role::PluginSharePrincipalRole>,
    #[serde(rename = "name")]
    pub name: String,
}
