pub type ExperimentalFeatureEnablementSetResponseEnablement1 = std::collections::BTreeMap<String, bool>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExperimentalFeatureEnablementSetResponse {
    #[serde(rename = "enablement")]
    pub enablement: ExperimentalFeatureEnablementSetResponseEnablement1,
}
