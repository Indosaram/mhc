#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkillSummary {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "description")]
    pub description: String,
    #[serde(rename = "shortDescription", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub short_description: Option<String>,
    #[serde(rename = "interface", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub interface: Option<Box<crate::app_server::protocol::generated::v2::skill_interface::SkillInterface>>,
    #[serde(rename = "path", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub path: Option<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>,
    #[serde(rename = "enabled")]
    pub enabled: bool,
}
