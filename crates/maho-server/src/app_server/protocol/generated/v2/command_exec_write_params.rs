#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandExecWriteParams {
    #[serde(rename = "processId")]
    pub process_id: String,
    #[serde(rename = "deltaBase64", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub delta_base64: Option<Option<String>>,
    #[serde(rename = "closeStdin", default, skip_serializing_if = "Option::is_none")]
    pub close_stdin: Option<bool>,
}
