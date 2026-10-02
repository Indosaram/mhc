#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExternalAgentConfigDetectParams {
    #[serde(rename = "includeHome", default, skip_serializing_if = "Option::is_none")]
    pub include_home: Option<bool>,
    #[serde(rename = "cwds", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub cwds: Option<Option<Vec<String>>>,
    #[serde(rename = "maxSessionAgeDays", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub max_session_age_days: Option<Option<f64>>,
    #[serde(rename = "maxSessions", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub max_sessions: Option<Option<f64>>,
    #[serde(rename = "source", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub source: Option<Option<String>>,
    #[serde(rename = "migrationSource", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub migration_source: Option<Option<String>>,
}
