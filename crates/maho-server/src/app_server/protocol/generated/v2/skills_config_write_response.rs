#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkillsConfigWriteResponse {
    #[serde(rename = "effectiveEnabled")]
    pub effective_enabled: bool,
}
