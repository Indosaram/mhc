#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WebSearchLocation {
    #[serde(rename = "country", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub country: Option<String>,
    #[serde(rename = "region", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub region: Option<String>,
    #[serde(rename = "city", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub city: Option<String>,
    #[serde(rename = "timezone", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub timezone: Option<String>,
}
