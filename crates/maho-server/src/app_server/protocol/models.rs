use serde::{Deserialize,Serialize};
use super::base::ReasoningEffort;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum InputModality {Text,Image,Audio}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ModelUpgradeInfo {pub model:String,pub upgrade_copy:Option<String>,pub model_link:Option<String>,pub migration_markdown:Option<String>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct ModelAvailabilityNux {pub message:String}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ReasoningEffortOption {pub reasoning_effort:ReasoningEffort,pub description:String}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct ModelServiceTier {pub id:String,pub name:String,pub description:String}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Model {
    pub id:String,pub model:String,pub upgrade:Option<String>,pub upgrade_info:Option<ModelUpgradeInfo>,pub availability_nux:Option<ModelAvailabilityNux>,
    pub display_name:String,pub description:String,pub hidden:bool,pub supported_reasoning_efforts:Vec<ReasoningEffortOption>,pub default_reasoning_effort:ReasoningEffort,
    pub input_modalities:Vec<InputModality>,pub supports_personality:bool,pub additional_speed_tiers:Vec<String>,pub service_tiers:Vec<ModelServiceTier>,pub default_service_tier:Option<String>,pub is_default:bool,
}
#[derive(Clone,Debug,Default,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ModelListParams {
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub cursor:Option<Option<String>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub limit:Option<Option<f64>>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub include_hidden:Option<Option<bool>>,
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ModelListResponse {pub data:Vec<Model>,pub next_cursor:Option<String>}
