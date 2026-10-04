#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkillMetadata {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "description")]
    pub description: String,
    #[serde(rename = "shortDescription", default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
    #[serde(rename = "interface", default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<Box<crate::app_server::protocol::generated::v2::skill_interface::SkillInterface>>,
    #[serde(rename = "dependencies", default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Box<crate::app_server::protocol::generated::v2::skill_dependencies::SkillDependencies>>,
    #[serde(rename = "path")]
    pub path: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "scope")]
    pub scope: Box<crate::app_server::protocol::generated::v2::skill_scope::SkillScope>,
    #[serde(rename = "enabled")]
    pub enabled: bool,
}
