#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigRequirementsReadResponse {
    #[serde(rename = "requirements", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub requirements: Option<Box<crate::app_server::protocol::generated::v2::config_requirements::ConfigRequirements>>,
}
