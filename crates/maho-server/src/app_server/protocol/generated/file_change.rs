#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileChangeAdd1 {
    #[serde(rename = "content")]
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileChangeDelete2 {
    #[serde(rename = "content")]
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileChangeUpdate3 {
    #[serde(rename = "unified_diff")]
    pub unified_diff: String,
    #[serde(rename = "move_path", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub move_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum FileChange {
    #[serde(rename = "add")]
    Add(Box<FileChangeAdd1>),
    #[serde(rename = "delete")]
    Delete(Box<FileChangeDelete2>),
    #[serde(rename = "update")]
    Update(Box<FileChangeUpdate3>),
}
