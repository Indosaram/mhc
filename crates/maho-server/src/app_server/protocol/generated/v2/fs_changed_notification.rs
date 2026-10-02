#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FsChangedNotification {
    #[serde(rename = "watchId")]
    pub watch_id: String,
    #[serde(rename = "changedPaths")]
    pub changed_paths: Vec<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>,
}
