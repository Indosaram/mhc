#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionMigration {
    #[serde(rename = "path")]
    pub path: String,
    #[serde(rename = "cwd")]
    pub cwd: String,
    #[serde(rename = "title", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub title: Option<String>,
}
