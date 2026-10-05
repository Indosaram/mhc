#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkillsConfigWriteParams {
    #[serde(rename = "path", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub path: Option<Option<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>>,
    #[serde(rename = "name", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub name: Option<Option<String>>,
    #[serde(rename = "enabled")]
    pub enabled: bool,
}
