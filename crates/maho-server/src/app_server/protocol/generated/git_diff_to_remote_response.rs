#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GitDiffToRemoteResponse {
    #[serde(rename = "sha")]
    pub sha: Box<crate::app_server::protocol::generated::git_sha::GitSha>,
    #[serde(rename = "diff")]
    pub diff: String,
}
