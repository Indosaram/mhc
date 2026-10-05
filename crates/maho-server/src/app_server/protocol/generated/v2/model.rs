#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Model {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "model")]
    pub model: String,
    #[serde(rename = "upgrade", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub upgrade: Option<String>,
    #[serde(rename = "upgradeInfo", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub upgrade_info: Option<Box<crate::app_server::protocol::generated::v2::model_upgrade_info::ModelUpgradeInfo>>,
    #[serde(rename = "availabilityNux", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub availability_nux: Option<Box<crate::app_server::protocol::generated::v2::model_availability_nux::ModelAvailabilityNux>>,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(rename = "description")]
    pub description: String,
    #[serde(rename = "hidden")]
    pub hidden: bool,
    #[serde(rename = "supportedReasoningEfforts")]
    pub supported_reasoning_efforts: Vec<Box<crate::app_server::protocol::generated::v2::reasoning_effort_option::ReasoningEffortOption>>,
    #[serde(rename = "defaultReasoningEffort")]
    pub default_reasoning_effort: Box<crate::app_server::protocol::generated::reasoning_effort::ReasoningEffort>,
    #[serde(rename = "inputModalities")]
    pub input_modalities: Vec<Box<crate::app_server::protocol::generated::input_modality::InputModality>>,
    #[serde(rename = "supportsPersonality")]
    pub supports_personality: bool,
    #[serde(rename = "additionalSpeedTiers")]
    pub additional_speed_tiers: Vec<String>,
    #[serde(rename = "serviceTiers")]
    pub service_tiers: Vec<Box<crate::app_server::protocol::generated::v2::model_service_tier::ModelServiceTier>>,
    #[serde(rename = "defaultServiceTier")]
    pub default_service_tier: Option<String>,
    #[serde(rename = "isDefault")]
    pub is_default: bool,
}
