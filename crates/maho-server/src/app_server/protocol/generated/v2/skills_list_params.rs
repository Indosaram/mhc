#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkillsListParams {
    #[serde(rename = "cwds", default, skip_serializing_if = "Option::is_none")]
    pub cwds: Option<Vec<String>>,
    #[serde(rename = "forceReload", default, skip_serializing_if = "Option::is_none")]
    pub force_reload: Option<bool>,
}
