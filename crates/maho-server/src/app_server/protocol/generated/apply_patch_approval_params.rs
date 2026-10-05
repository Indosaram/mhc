pub type ApplyPatchApprovalParamsFileChanges1 = std::collections::BTreeMap<String, Box<crate::app_server::protocol::generated::file_change::FileChange>>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ApplyPatchApprovalParams {
    #[serde(rename = "conversationId")]
    pub conversation_id: Box<crate::app_server::protocol::generated::thread_id::ThreadId>,
    #[serde(rename = "callId")]
    pub call_id: String,
    #[serde(rename = "fileChanges")]
    pub file_changes: ApplyPatchApprovalParamsFileChanges1,
    #[serde(rename = "reason", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub reason: Option<String>,
    #[serde(rename = "grantRoot", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub grant_root: Option<String>,
}
