#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkillsListEntry {
    #[serde(rename = "cwd")]
    pub cwd: String,
    #[serde(rename = "skills")]
    pub skills: Vec<Box<crate::app_server::protocol::generated::v2::skill_metadata::SkillMetadata>>,
    #[serde(rename = "errors")]
    pub errors: Vec<Box<crate::app_server::protocol::generated::v2::skill_error_info::SkillErrorInfo>>,
}
