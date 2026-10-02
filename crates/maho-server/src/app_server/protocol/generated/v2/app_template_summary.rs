#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppTemplateSummary {
    #[serde(rename = "templateId")]
    pub template_id: String,
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "description", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub description: Option<String>,
    #[serde(rename = "category", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub category: Option<String>,
    #[serde(rename = "canonicalConnectorId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub canonical_connector_id: Option<String>,
    #[serde(rename = "logoUrl", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub logo_url: Option<String>,
    #[serde(rename = "logoUrlDark", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub logo_url_dark: Option<String>,
    #[serde(rename = "materializedAppIds")]
    pub materialized_app_ids: Vec<String>,
    #[serde(rename = "reason", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub reason: Option<Box<crate::app_server::protocol::generated::v2::app_template_unavailable_reason::AppTemplateUnavailableReason>>,
}
