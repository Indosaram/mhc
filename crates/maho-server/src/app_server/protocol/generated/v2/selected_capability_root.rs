#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SelectedCapabilityRoot {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "location")]
    pub location: Box<crate::app_server::protocol::generated::v2::capability_root_location::CapabilityRootLocation>,
}
