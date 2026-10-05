#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigBatchWriteParams {
    #[serde(rename = "edits")]
    pub edits: Vec<Box<crate::app_server::protocol::generated::v2::config_edit::ConfigEdit>>,
    #[serde(rename = "filePath", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub file_path: Option<Option<String>>,
    #[serde(rename = "expectedVersion", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub expected_version: Option<Option<String>>,
    #[serde(rename = "reloadUserConfig", default, skip_serializing_if = "Option::is_none")]
    pub reload_user_config: Option<bool>,
}
