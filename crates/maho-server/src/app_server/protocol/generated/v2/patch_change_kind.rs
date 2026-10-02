#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PatchChangeKindAdd1 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PatchChangeKindDelete2 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PatchChangeKindUpdate3 {
    #[serde(rename = "move_path", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub move_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum PatchChangeKind {
    #[serde(rename = "add")]
    Add(Box<PatchChangeKindAdd1>),
    #[serde(rename = "delete")]
    Delete(Box<PatchChangeKindDelete2>),
    #[serde(rename = "update")]
    Update(Box<PatchChangeKindUpdate3>),
}
