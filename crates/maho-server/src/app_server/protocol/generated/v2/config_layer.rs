#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigLayer {
    #[serde(rename = "name")]
    pub name: Box<crate::app_server::protocol::generated::v2::config_layer_source::ConfigLayerSource>,
    #[serde(rename = "version")]
    pub version: String,
    #[serde(rename = "config")]
    pub config: Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>,
    #[serde(rename = "disabledReason", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub disabled_reason: Option<String>,
}
