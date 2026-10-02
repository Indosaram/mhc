#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetAuthStatusResponse {
    #[serde(rename = "authMethod", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub auth_method: Option<Box<crate::app_server::protocol::generated::auth_mode::AuthMode>>,
    #[serde(rename = "authToken", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub auth_token: Option<String>,
    #[serde(rename = "requiresOpenaiAuth", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub requires_openai_auth: Option<bool>,
}
