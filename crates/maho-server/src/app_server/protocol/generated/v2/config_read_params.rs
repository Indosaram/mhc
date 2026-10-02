#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigReadParams {
    #[serde(rename = "includeLayers", default, skip_serializing_if = "Option::is_none")]
    pub include_layers: Option<bool>,
    #[serde(rename = "cwd", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub cwd: Option<Option<String>>,
}
