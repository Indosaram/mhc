use serde::{Deserialize,Serialize};
use std::collections::BTreeMap;
use super::base::{AskForApproval,JsonValue,ReasoningEffort,SandboxMode};
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct Config {
    #[serde(deserialize_with="super::nullable::deserialize_required")]pub model:Option<String>,
    #[serde(deserialize_with="super::nullable::deserialize_required")]pub model_provider:Option<String>,
    #[serde(deserialize_with="super::nullable::deserialize_required")]pub approval_policy:Option<AskForApproval>,
    #[serde(deserialize_with="super::nullable::deserialize_required")]pub sandbox_mode:Option<SandboxMode>,
    #[serde(deserialize_with="super::nullable::deserialize_required")]pub model_reasoning_effort:Option<ReasoningEffort>,
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(tag="type",rename_all="camelCase",rename_all_fields="camelCase")]
pub enum ConfigLayerSource {
    Mdm {domain:String,key:String},System {file:String},EnterpriseManaged {id:String,name:String},User {file:String,#[serde(deserialize_with="super::nullable::deserialize_required")]profile:Option<String>},
    Project {dot_codex_folder:String},SessionFlags,LegacyManagedConfigTomlFromFile {file:String},LegacyManagedConfigTomlFromMdm,
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct ConfigLayerMetadata {pub name:ConfigLayerSource,pub version:String}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ConfigLayer {pub name:ConfigLayerSource,pub version:String,pub config:JsonValue,#[serde(deserialize_with="super::nullable::deserialize_required")]pub disabled_reason:Option<String>}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ConfigReadParams {
    #[serde(default,skip_serializing_if="Option::is_none")]pub include_layers:Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none",deserialize_with="super::nullable::deserialize")]pub cwd:Option<Option<String>>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct ConfigReadResponse {pub config:Config,pub origins:BTreeMap<String,ConfigLayerMetadata>,#[serde(deserialize_with="super::nullable::deserialize_required")]pub layers:Option<Vec<ConfigLayer>>}
pub type ConfigRequirementsReadParams=();
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
pub struct ConfigRequirementsReadResponse {pub requirements:()}
