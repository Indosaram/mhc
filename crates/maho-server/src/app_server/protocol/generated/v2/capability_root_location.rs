#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CapabilityRootLocationType1 {
    #[serde(rename = "environment")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CapabilityRootLocation {
    #[serde(rename = "type")]
    pub r#type: CapabilityRootLocationType1,
    #[serde(rename = "environmentId")]
    pub environment_id: String,
    #[serde(rename = "path")]
    pub path: String,
}
