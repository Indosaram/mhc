pub type ExperimentalFeatureEnablementSetParamsEnablement1 = std::collections::BTreeMap<String, bool>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExperimentalFeatureEnablementSetParams {
    #[serde(rename = "enablement")]
    pub enablement: ExperimentalFeatureEnablementSetParamsEnablement1,
}
