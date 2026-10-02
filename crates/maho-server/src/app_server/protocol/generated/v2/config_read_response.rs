pub type ConfigReadResponseOrigins1 = std::collections::BTreeMap<String, Box<crate::app_server::protocol::generated::v2::config_layer_metadata::ConfigLayerMetadata>>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigReadResponse {
    #[serde(rename = "config")]
    pub config: Box<crate::app_server::protocol::generated::v2::config::Config>,
    #[serde(rename = "origins")]
    pub origins: ConfigReadResponseOrigins1,
    #[serde(rename = "layers", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub layers: Option<Vec<Box<crate::app_server::protocol::generated::v2::config_layer::ConfigLayer>>>,
}
