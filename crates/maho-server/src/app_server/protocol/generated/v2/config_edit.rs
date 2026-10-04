#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigEdit {
    #[serde(rename = "keyPath")]
    pub key_path: String,
    #[serde(rename = "value")]
    pub value: Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>,
    #[serde(rename = "mergeStrategy")]
    pub merge_strategy: Box<crate::app_server::protocol::generated::v2::merge_strategy::MergeStrategy>,
}
