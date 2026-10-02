#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkillToolDependency {
    #[serde(rename = "type")]
    pub r#type: String,
    #[serde(rename = "value")]
    pub value: String,
    #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "transport", default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(rename = "command", default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(rename = "url", default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}
