#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ExperimentalFeatureStage {
    #[serde(rename = "beta")]
    Beta,
    #[serde(rename = "underDevelopment")]
    UnderDevelopment,
    #[serde(rename = "stable")]
    Stable,
    #[serde(rename = "deprecated")]
    Deprecated,
    #[serde(rename = "removed")]
    Removed,
}
