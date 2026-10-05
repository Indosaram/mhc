use serde::{Deserialize,Serialize};

#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum CollaborationModeKind {Plan,Default}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct CollaborationModeSettings {pub model:String,#[serde(deserialize_with="super::nullable::deserialize_required")]pub reasoning_effort:Option<String>,#[serde(deserialize_with="super::nullable::deserialize_required")]pub developer_instructions:Option<String>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct CollaborationMode {pub mode:CollaborationModeKind,pub settings:CollaborationModeSettings}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct CollaborationModeMask {pub name:String,#[serde(deserialize_with="super::nullable::deserialize_required")]pub mode:Option<CollaborationModeKind>,#[serde(deserialize_with="super::nullable::deserialize_required")]pub model:Option<String>,#[serde(deserialize_with="super::nullable::deserialize_required")]pub reasoning_effort:Option<String>}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
pub struct CollaborationModeListParams {}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct CollaborationModeListResponse {pub data:Vec<CollaborationModeMask>}
pub static SENPI_COLLABORATION_MODE:std::sync::LazyLock<CollaborationMode>=std::sync::LazyLock::new(||build_senpi_collaboration_mode("unknown".into(),Some("off".into())));
pub fn build_senpi_collaboration_mode(model:String,reasoning_effort:Option<String>) -> CollaborationMode {
    CollaborationMode {mode:CollaborationModeKind::Default,settings:CollaborationModeSettings {model,reasoning_effort,developer_instructions:None}}
}
pub fn build_senpi_collaboration_mode_preset(model:String) -> CollaborationModeMask {
    CollaborationModeMask {name:"default".into(),mode:None,model:Some(model),reasoning_effort:None}
}
