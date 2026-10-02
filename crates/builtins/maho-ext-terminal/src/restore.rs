use serde_json::{Map,Value};
use crate::terminal_manifest_model::*;

#[derive(Debug,thiserror::Error)]
#[error("terminal manifest is invalid: {0}")]
pub struct InvalidTerminalManifestError(pub String);
type Result<T>=std::result::Result<T,InvalidTerminalManifestError>;
fn invalid(message:impl Into<String>)->InvalidTerminalManifestError {InvalidTerminalManifestError(message.into())}
fn object<'a>(value:&'a Value,message:&str)->Result<&'a Map<String,Value>> {value.as_object().ok_or_else(||invalid(message))}
fn str_field(raw:&Map<String,Value>,field:&str)->Result<String> {raw.get(field).and_then(Value::as_str).filter(|s|!s.is_empty()).map(str::to_owned).ok_or_else(||invalid(format!("field {field} must be a non-empty string")))}
fn num(raw:&Map<String,Value>,field:&str)->Result<f64> {raw.get(field).and_then(Value::as_f64).filter(|v|v.is_finite()).ok_or_else(||invalid(format!("field {field} must be a finite number")))}
fn boolean(raw:&Map<String,Value>,field:&str)->Result<bool> {raw.get(field).and_then(Value::as_bool).ok_or_else(||invalid(format!("field {field} must be a boolean")))}
fn optional(raw:&Map<String,Value>,field:&str)->Result<Option<String>> {if raw.contains_key(field) {str_field(raw,field).map(Some)} else {Ok(None)}}
fn one_of<'a>(raw:&'a Map<String,Value>,field:&str,values:&[&str])->Result<&'a str> {raw.get(field).and_then(Value::as_str).filter(|value|values.contains(value)).ok_or_else(||invalid(format!("field {field} must be one of: {}",values.join(", "))))}
fn checkpoint(value:&Value)->Result<TerminalManifestCheckpoint> {let raw=object(value,"field lastCheckpoint must be an object")?;Ok(TerminalManifestCheckpoint {dev:num(raw,"dev")?,ino:num(raw,"ino")?,size:num(raw,"size")?,mtime_ms:num(raw,"mtimeMs")?,digest:raw.get("digest").and_then(Value::as_str).map(str::to_owned).ok_or_else(||invalid("field digest must be a string"))?,present:boolean(raw,"present")?})}
fn parse_monitor(value:&Value)->Result<ManifestMonitor> {
    let raw=object(value,"a monitor entry must be an object")?;
    let runtime_kind=match one_of(raw,"runtimeKind",&["command","file"])? {"command"=>MonitorRuntimeKind::Command,_=>MonitorRuntimeKind::File};
    let durability_class=match one_of(raw,"durabilityClass",&["ephemeral","restartable-command","checkpointed-file"])? {"ephemeral"=>MonitorDurabilityClass::Ephemeral,"restartable-command"=>MonitorDurabilityClass::RestartableCommand,_=>MonitorDurabilityClass::CheckpointedFile};
    let event=if raw.contains_key("event") {Some(match one_of(raw,"event",&["create","modify"])? {"create"=>FileEvent::Create,_=>FileEvent::Modify})} else {None};
    let last_checkpoint=if raw.get("lastCheckpoint")==Some(&Value::Null) {None} else {Some(checkpoint(raw.get("lastCheckpoint").unwrap_or(&Value::Null))?)};
    let window=object(raw.get("fireWindow").unwrap_or(&Value::Null),"field fireWindow must be an object")?;
    Ok(ManifestMonitor {monitor_id:str_field(raw,"monitorId")?,session_id:str_field(raw,"sessionId")?,description:str_field(raw,"description")?,runtime_kind,durability_class,command:optional(raw,"command")?,path:optional(raw,"path")?,event,filter:optional(raw,"filter")?,cwd:optional(raw,"cwd")?,approved_parent:optional(raw,"approvedParent")?,created_at:num(raw,"createdAt")?,expires_at:if raw.get("expiresAt").is_none_or(Value::is_null) {None} else {Some(num(raw,"expiresAt")?)},persistent:boolean(raw,"persistent")?,suspended:boolean(raw,"suspended")?,last_checkpoint,delivery_paused:boolean(raw,"deliveryPaused")?,fire_window:ManifestFireWindow {start_ms:num(window,"startMs")?,count:num(window,"count")?}})
}

/// Domain parser; the owning sidecar store validates version and session identity first.
pub fn parse_terminal_manifest(value:&Value,session_id:&str)->Result<TerminalManifest> {
    let raw=object(value,"the payload must be an object")?;
    let monitors=raw.get("monitors").and_then(Value::as_array).ok_or_else(||invalid("field monitors must be an array"))?;
    let backgrounds=raw.get("backgroundSessions").and_then(Value::as_array).ok_or_else(||invalid("field backgroundSessions must be an array"))?;
    let monitors=monitors.iter().map(parse_monitor).collect::<Result<Vec<_>>>()?;
    let background_sessions=backgrounds.iter().map(|entry| {let raw=object(entry,"a background session entry must be an object")?;Ok(ManifestBackgroundSession {id:str_field(raw,"id")?,command:str_field(raw,"command")?,started_at_ms:num(raw,"startedAtMs")?})}).collect::<Result<Vec<_>>>()?;
    Ok(TerminalManifest {version:TERMINAL_MANIFEST_VERSION,session_id:session_id.to_owned(),monitors,background_sessions,updated_at:num(raw,"updatedAt")?})
}

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum RestoreOutcome {Restored,Lost,Muted,AttachedElsewhere}
#[derive(Default,Debug,PartialEq,Eq)]
pub struct RestoreDigest {pub restored:usize,pub lost:usize,pub expired:usize,pub muted:usize,pub attached_elsewhere:usize,pub store_error:bool}
pub fn reapply_persisted_mute(monitor:&ManifestMonitor,runtime_id:&str,mut pause:impl FnMut(&[String]))->RestoreOutcome {if monitor.delivery_paused {pause(&[runtime_id.to_owned()]);RestoreOutcome::Muted} else {RestoreOutcome::Restored}}
pub async fn restore_terminal_state<E,F,Fut>(state:std::result::Result<Option<TerminalManifest>,E>,now:f64,mut handler:F)->RestoreDigest where F:FnMut(ManifestMonitor)->Fut,Fut:std::future::Future<Output=RestoreOutcome> {
    let mut digest=RestoreDigest::default();let state=match state {Err(_)=>{digest.store_error=true;return digest;},Ok(None)=>return digest,Ok(Some(state))=>state};
    for monitor in state.monitors {
        if monitor.expires_at.is_some_and(|expires|expires<=now) {digest.expired+=1;continue;}
        if monitor.durability_class==MonitorDurabilityClass::Ephemeral {digest.lost+=1;continue;}
        match handler(monitor).await {RestoreOutcome::Restored=>digest.restored+=1,RestoreOutcome::Lost=>digest.lost+=1,RestoreOutcome::Muted=>digest.muted+=1,RestoreOutcome::AttachedElsewhere=>digest.attached_elsewhere+=1}
    }
    digest.lost+=state.background_sessions.len();digest
}

#[cfg(test)]
mod tests {
    use super::*;use serde_json::json;
    fn raw()->Value {json!({"monitors":[{"monitorId":"mon_1","sessionId":"s","description":"watch","runtimeKind":"command","durabilityClass":"restartable-command","createdAt":1,"expiresAt":null,"persistent":true,"suspended":false,"lastCheckpoint":null,"deliveryPaused":false,"fireWindow":{"startMs":1,"count":0}}],"backgroundSessions":[],"updatedAt":3})}
    #[test] fn strict_fields_ignore_unknown_keys()->Result<()> {let mut value=raw();value["monitors"][0]["wakeCount"]=json!(8);let parsed=parse_terminal_manifest(&value,"s")?;assert_eq!(parsed.version,1);assert_eq!(parsed.monitors.len(),1);value["monitors"][0]["command"]=Value::Null;assert!(parse_terminal_manifest(&value,"s").is_err());Ok(())}
    #[test] fn digest_required_but_empty_valid()->Result<()> {let mut value=raw();value["monitors"][0]["lastCheckpoint"]=json!({"dev":1,"ino":1,"size":0,"mtimeMs":0,"present":false});assert!(parse_terminal_manifest(&value,"s").is_err());value["monitors"][0]["lastCheckpoint"]["digest"]=json!("");assert!(parse_terminal_manifest(&value,"s")?.monitors[0].last_checkpoint.is_some());Ok(())}
    #[test] fn mute_uses_fresh_runtime_id()->Result<()> {let mut monitor=parse_terminal_manifest(&raw(),"s")?.monitors.remove(0);monitor.delivery_paused=true;let mut seen=Vec::new();assert_eq!(reapply_persisted_mute(&monitor,"bash_2",|ids|seen.extend_from_slice(ids)),RestoreOutcome::Muted);assert_eq!(seen,vec!["bash_2"]);Ok(())}
    #[tokio::test] async fn expiry_precedes_durability_handler()->Result<()> {let mut manifest=parse_terminal_manifest(&raw(),"s")?;manifest.monitors[0].expires_at=Some(100.0);let digest=restore_terminal_state(Ok::<_,()>(Some(manifest)),100.0,|_|async {panic!("expired watch must never restore")}).await;assert_eq!(digest.expired,1);assert_eq!(digest.restored,0);Ok(())}
    #[tokio::test] async fn corrupt_and_missing_store_restore_nothing() {assert!(restore_terminal_state(Err::<Option<TerminalManifest>,_>(()),0.0,|_|async {RestoreOutcome::Restored}).await.store_error);assert_eq!(restore_terminal_state(Ok::<_,()>(None),0.0,|_|async {RestoreOutcome::Restored}).await,RestoreDigest::default());}
}
