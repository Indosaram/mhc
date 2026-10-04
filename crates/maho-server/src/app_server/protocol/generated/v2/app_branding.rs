#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppBranding {
    #[serde(rename = "category", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub category: Option<String>,
    #[serde(rename = "developer", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub developer: Option<String>,
    #[serde(rename = "website", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub website: Option<String>,
    #[serde(rename = "privacyPolicy", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub privacy_policy: Option<String>,
    #[serde(rename = "termsOfService", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub terms_of_service: Option<String>,
    #[serde(rename = "isDiscoverableApp")]
    pub is_discoverable_app: bool,
}
