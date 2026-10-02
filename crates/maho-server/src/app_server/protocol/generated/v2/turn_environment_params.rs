#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TurnEnvironmentParams {
    #[serde(rename = "environmentId")]
    pub environment_id: String,
    #[serde(rename = "cwd")]
    pub cwd: Box<crate::app_server::protocol::generated::legacy_app_path_string::LegacyAppPathString>,
    #[serde(rename = "runtimeWorkspaceRoots", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub runtime_workspace_roots: Option<Option<Vec<Box<crate::app_server::protocol::generated::legacy_app_path_string::LegacyAppPathString>>>>,
}
