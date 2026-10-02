#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExperimentalFeature {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "stage")]
    pub stage: Box<crate::app_server::protocol::generated::v2::experimental_feature_stage::ExperimentalFeatureStage>,
    #[serde(rename = "displayName", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub display_name: Option<String>,
    #[serde(rename = "description", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub description: Option<String>,
    #[serde(rename = "announcement", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub announcement: Option<String>,
    #[serde(rename = "enabled")]
    pub enabled: bool,
    #[serde(rename = "defaultEnabled")]
    pub default_enabled: bool,
}
