#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadMetadataGitInfoUpdateParams {
    #[serde(rename = "sha", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub sha: Option<Option<String>>,
    #[serde(rename = "branch", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub branch: Option<Option<String>>,
    #[serde(rename = "originUrl", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub origin_url: Option<Option<String>>,
}
