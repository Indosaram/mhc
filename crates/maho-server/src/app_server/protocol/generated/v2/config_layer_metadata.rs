#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigLayerMetadata {
    #[serde(rename = "name")]
    pub name: Box<crate::app_server::protocol::generated::v2::config_layer_source::ConfigLayerSource>,
    #[serde(rename = "version")]
    pub version: String,
}
