#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum NetworkAccess {
    #[serde(rename = "restricted")]
    Restricted,
    #[serde(rename = "enabled")]
    Enabled,
}
