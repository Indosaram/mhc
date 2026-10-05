#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReviewTargetUncommittedChanges1 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReviewTargetBaseBranch2 {
    #[serde(rename = "branch")]
    pub branch: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReviewTargetCommit3 {
    #[serde(rename = "sha")]
    pub sha: String,
    #[serde(rename = "title", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub title: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReviewTargetCustom4 {
    #[serde(rename = "instructions")]
    pub instructions: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum ReviewTarget {
    #[serde(rename = "uncommittedChanges")]
    UncommittedChanges(Box<ReviewTargetUncommittedChanges1>),
    #[serde(rename = "baseBranch")]
    BaseBranch(Box<ReviewTargetBaseBranch2>),
    #[serde(rename = "commit")]
    Commit(Box<ReviewTargetCommit3>),
    #[serde(rename = "custom")]
    Custom(Box<ReviewTargetCustom4>),
}
