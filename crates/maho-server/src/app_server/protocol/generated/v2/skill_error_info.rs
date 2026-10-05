#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkillErrorInfo {
    #[serde(rename = "path")]
    pub path: String,
    #[serde(rename = "message")]
    pub message: String,
}
