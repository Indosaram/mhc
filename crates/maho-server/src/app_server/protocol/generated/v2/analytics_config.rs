pub type AnalyticsConfigDetails01Value2Variant43 = std::collections::BTreeMap<String, Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum AnalyticsConfigDetails01Value2 {
    Variant0(Box<f64>),
    Variant1(Box<String>),
    Variant2(Box<bool>),
    Variant3(Box<Vec<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>>),
    Variant4(Box<AnalyticsConfigDetails01Value2Variant43>),
}

pub type AnalyticsConfigDetails01 = std::collections::BTreeMap<String, Option<AnalyticsConfigDetails01Value2>>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AnalyticsConfig {
    #[serde(rename = "enabled", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub enabled: Option<bool>,
    #[serde(flatten)]
    pub details: AnalyticsConfigDetails01,
}
