#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkillDependencies {
    #[serde(rename = "tools")]
    pub tools: Vec<Box<crate::app_server::protocol::generated::v2::skill_tool_dependency::SkillToolDependency>>,
}
