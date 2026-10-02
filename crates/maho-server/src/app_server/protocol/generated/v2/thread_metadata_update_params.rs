#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadMetadataUpdateParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "gitInfo", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub git_info: Option<Option<Box<crate::app_server::protocol::generated::v2::thread_metadata_git_info_update_params::ThreadMetadataGitInfoUpdateParams>>>,
}
