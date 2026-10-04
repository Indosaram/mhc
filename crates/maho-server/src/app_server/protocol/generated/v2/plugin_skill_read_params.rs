#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginSkillReadParams {
    #[serde(rename = "remoteMarketplaceName")]
    pub remote_marketplace_name: String,
    #[serde(rename = "remotePluginId")]
    pub remote_plugin_id: String,
    #[serde(rename = "skillName")]
    pub skill_name: String,
}
