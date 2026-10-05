#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OverriddenMetadata {
    #[serde(rename = "message")]
    pub message: String,
    #[serde(rename = "overridingLayer")]
    pub overriding_layer: Box<crate::app_server::protocol::generated::v2::config_layer_metadata::ConfigLayerMetadata>,
    #[serde(rename = "effectiveValue")]
    pub effective_value: Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>,
}
