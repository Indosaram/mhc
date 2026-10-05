#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigValueWriteParams {
    #[serde(rename = "keyPath")]
    pub key_path: String,
    #[serde(rename = "value")]
    pub value: Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>,
    #[serde(rename = "mergeStrategy")]
    pub merge_strategy: Box<crate::app_server::protocol::generated::v2::merge_strategy::MergeStrategy>,
    #[serde(rename = "filePath", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub file_path: Option<Option<String>>,
    #[serde(rename = "expectedVersion", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub expected_version: Option<Option<String>>,
}
