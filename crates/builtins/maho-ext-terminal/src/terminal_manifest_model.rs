use serde::{Deserialize,Serialize};
pub const TERMINAL_MANIFEST_VERSION:u8=1;
pub const TERMINAL_MANIFEST_CHECKPOINT_DEBOUNCE_MS:u64=30_000;
pub use crate::shared::DURABLE_MONITOR_EXPIRY_MS;

#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum MonitorRuntimeKind {Command,File}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="kebab-case")]
pub enum MonitorDurabilityClass {Ephemeral,RestartableCommand,CheckpointedFile}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum FileEvent {Create,Modify}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct TerminalManifestCheckpoint {pub dev:f64,pub ino:f64,pub size:f64,pub mtime_ms:f64,pub digest:String,pub present:bool}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ManifestFireWindow {pub start_ms:f64,pub count:f64}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ManifestMonitor {
    pub monitor_id:String,pub session_id:String,pub description:String,
    pub runtime_kind:MonitorRuntimeKind,pub durability_class:MonitorDurabilityClass,
    #[serde(skip_serializing_if="Option::is_none")] pub command:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub path:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub event:Option<FileEvent>,
    #[serde(skip_serializing_if="Option::is_none")] pub filter:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub cwd:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub approved_parent:Option<String>,
    pub created_at:f64,pub expires_at:Option<f64>,pub persistent:bool,pub suspended:bool,
    pub last_checkpoint:Option<TerminalManifestCheckpoint>,pub delivery_paused:bool,pub fire_window:ManifestFireWindow,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ManifestBackgroundSession {pub id:String,pub command:String,pub started_at_ms:f64}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct TerminalManifest {pub version:u8,pub session_id:String,pub monitors:Vec<ManifestMonitor>,pub background_sessions:Vec<ManifestBackgroundSession>,pub updated_at:f64}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="lowercase")]
pub enum MonitorSpec {
    Command {description:String,command:String,filter:Option<String>,cwd:Option<String>,persistent:bool},
    File {description:String,path:String,event:FileEvent,#[serde(rename="timeoutMs")] timeout_ms:f64,cwd:String,#[serde(rename="approvedParent")] approved_parent:Option<String>,persistent:bool},
}
pub struct MonitorRegistration {pub monitor_id:String,pub spec:MonitorSpec}
