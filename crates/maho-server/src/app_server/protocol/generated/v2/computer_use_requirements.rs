#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ComputerUseRequirements {
    #[serde(rename = "allowLockedComputerUse", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub allow_locked_computer_use: Option<bool>,
}
