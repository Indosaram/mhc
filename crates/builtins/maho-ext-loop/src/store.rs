use std::{collections::BTreeMap,sync::Arc};
use serde_json::Value;
use maho_core::session_sidecar_store::{self as sidecar,CreateSidecarStoreOptions,SidecarError,SidecarStore,SidecarStoreRef};
use crate::types::*;
#[derive(Debug,thiserror::Error)]
pub enum LoopStoreError {
    #[error("{0}")] Invalid(String),
    #[error("{0}")] UnsupportedVersion(String),
    #[error("{0}")] Io(String),
}
fn remap(error:SidecarError)->LoopStoreError { match error { SidecarError::Invalid(message)=>LoopStoreError::Invalid(message.replace("sidecar store","loop store")),SidecarError::UnsupportedVersion(message)=>LoopStoreError::UnsupportedVersion(message.replace("sidecar store","loop store")),SidecarError::Io(message)=>LoopStoreError::Io(message) } }
fn invalid(id:&str,field:&str)->SidecarError { SidecarError::Invalid(format!("loop entry {id} has an invalid {field}")) }
fn string(id:&str,field:&str,value:&Value,empty:bool)->Result<(),SidecarError> { if value.as_str().is_some_and(|value|empty || !value.is_empty()) { Ok(()) } else { Err(invalid(id,field)) } }
fn number(id:&str,field:&str,value:&Value,integer:bool,positive:bool)->Result<(),SidecarError> { if value.as_f64().is_some_and(|value|value.is_finite() && (!integer || value.fract()==0.0 && value.abs()<=9_007_199_254_740_991.0) && (if positive { value>0.0 } else { !integer || value>=0.0 })) { Ok(()) } else { Err(invalid(id,field)) } }
fn boolean(id:&str,field:&str,value:&Value)->Result<(),SidecarError> { if value.is_boolean() { Ok(()) } else { Err(invalid(id,field)) } }
fn validate_entry(id:&str,entry:&Value)->Result<(),SidecarError> {
    let Some(object)=entry.as_object() else { return Err(SidecarError::Invalid(format!("loop entry {id} is not an object"))); };
    if entry["id"].as_str()!=Some(id) { return Err(SidecarError::Invalid(format!("loop entry {id} carries a mismatched id"))); }
    for field in ["originalArgs","reentryPrompt"] { string(id,field,&entry[field],field=="originalArgs")?; }
    for field in ["createdAt","expiresAt","noopStreak","tickCount"] { number(id,field,&entry[field],true,false)?; }
    for field in ["lastFiredAt","lastScheduledForAt","queuedForAt"] { if !entry[field].is_null() { number(id,field,&entry[field],true,false)?; } }
    boolean(id,"coalescedFirePending",&entry["coalescedFirePending"])?;
    if entry["phase"]=="ended" { number(id,"endedAt",&entry["endedAt"],true,false)?; string(id,"endReason",&entry["endReason"],false)?; if let Some(detail)=object.get("endDetail") { string(id,"endDetail",detail,true)?; } } else if ["endedAt","endReason","endDetail"].iter().any(|field|object.contains_key(*field)) { return Err(SidecarError::Invalid(format!("loop entry {id} carries terminal fields while not ended"))); }
    let payload=&entry["payload"];
    if payload["type"]=="prompt" { string(id,"payload.prompt",&payload["prompt"],false)?; }
    let sentinel=&entry["sentinelDelivery"];
    boolean(id,"sentinelDelivery.autonomousPreambleDelivered",&sentinel["autonomousPreambleDelivered"])?; boolean(id,"sentinelDelivery.forceFullDelivery",&sentinel["forceFullDelivery"])?;
    let fingerprint=&sentinel["lastLoopFileDelivered"];
    if !fingerprint.is_null() { for field in ["path","contentHash","anchorDeliveryId"] { string(id,&format!("fingerprint.{field}"),&fingerprint[field],false)?; } number(id,"fingerprint.mtimeMs",&fingerprint["mtimeMs"],false,false)?; number(id,"fingerprint.size",&fingerprint["size"],true,false)?; }
    let sources=entry["wakeSources"].as_array().ok_or_else(||invalid(id,"wakeSources"))?;
    for source in sources { string(id,"wakeSource.id",&source["id"],false)?; number(id,"wakeSource.createdAt",&source["createdAt"],true,false)?; if let Some(description)=source.get("description") { string(id,"wake source description",description,true)?; } }
    match entry["kind"].as_str() {
        Some("fixed")=>{
            string(id,"cronExpression",&entry["cronExpression"],false)?;
            number(id,"nextFireAt",&entry["nextFireAt"],true,false)?; number(id,"intervalMs",&entry["intervalMs"],true,true)?;
            for field in ["requestedInterval","effectiveInterval"] { number(id,&format!("{field}.value"),&entry[field]["value"],true,true)?; }
            string(id,"requestedInterval.raw",&entry["requestedInterval"]["raw"],false)?; string(id,"effectiveInterval.human",&entry["effectiveInterval"]["human"],false)?; boolean(id,"effectiveInterval.rounded",&entry["effectiveInterval"]["rounded"])?;
            if let Some(notice)=entry["effectiveInterval"].get("roundingNotice") { string(id,"effectiveInterval roundingNotice",notice,true)?; }
        },
        Some("dynamic")=>{
            if !matches!(entry["keepaliveCredit"].as_f64(),Some(0.0|1.0)) { return Err(invalid(id,"keepaliveCredit")); }
            let wake=&entry["pendingWakeup"];
            if !wake.is_null() {
                for field in ["id","loopId","reason","prompt"] { string(id,&format!("pendingWakeup.{field}"),&wake[field],false)?; }
                number(id,"pendingWakeup.requestedDelaySeconds",&wake["requestedDelaySeconds"],false,false)?;
                number(id,"pendingWakeup.delaySeconds",&wake["delaySeconds"],true,true)?;
                for field in ["dueAt","createdAt"] { number(id,&format!("pendingWakeup.{field}"),&wake[field],true,false)?; }
                boolean(id,"pendingWakeup.noop",&wake["noop"])?;
            }
        },
        _=>return Err(SidecarError::Invalid(format!("loop entry {id} has an unknown kind"))),
    }
    serde_json::from_value::<CronEntry>(entry.clone()).map_err(|error|SidecarError::Invalid(format!("loop entry {id}: {error}")))?;
    Ok(())
}
fn parse_payload(raw:&Value,reference:&SidecarStoreRef)->Result<Value,SidecarError> {
    number("store","updatedAt",&raw["updatedAt"],true,false)?;
    let entries=raw["entries"].as_object().ok_or_else(||SidecarError::Invalid("loop store entries must be an object".into()))?;
    for (id,entry) in entries { validate_entry(id,entry)?; }
    if !raw["activeDynamicId"].is_null() { let id=raw["activeDynamicId"].as_str().ok_or_else(||SidecarError::Invalid("loop store activeDynamicId must be a string or null".into()))?; if entries.get(id).is_none_or(|entry|entry["kind"]!="dynamic") { return Err(SidecarError::Invalid(format!("loop store activeDynamicId {id} does not name a dynamic loop"))); } }
    let mut result=raw.clone(); result["version"]=LOOP_STATE_VERSION.into(); result["sessionId"]=reference.session_id.clone().into(); Ok(result)
}
fn store(reference:&LoopStoreRef)->SidecarStore { sidecar::create_sidecar_store(CreateSidecarStoreOptions { base_dir:reference.base_dir.to_string_lossy().into_owned(),session_id:reference.session_id.clone(),version:i64::from(LOOP_STATE_VERSION),temp_prefix:"loop".into(),parse:Arc::new(parse_payload) }) }
pub fn empty_loop_state(session_id:&str)->LoopState { LoopState { version:LOOP_STATE_VERSION,session_id:session_id.into(),entries:BTreeMap::new(),active_dynamic_id:None,updated_at:0.0 } }
pub fn encoded_session_id(reference:&LoopStoreRef)->String { sidecar::encoded_session_id(&reference.session_id) }
pub fn loop_state_file_path(reference:&LoopStoreRef)->String { store(reference).file_path().into() }
fn decode(value:Value)->Result<LoopState,LoopStoreError> { serde_json::from_value(value).map_err(|error|LoopStoreError::Invalid(error.to_string())) }
pub async fn read_loop_state(reference:&LoopStoreRef)->Result<Option<LoopState>,LoopStoreError> { store(reference).read().await.map_err(remap)?.map(decode).transpose() }
pub async fn load_loop_state(reference:&LoopStoreRef)->Result<LoopState,LoopStoreError> { Ok(read_loop_state(reference).await?.unwrap_or_else(||empty_loop_state(&reference.session_id))) }
pub fn snapshot_loop_state(reference:&LoopStoreRef)->Result<Option<LoopState>,LoopStoreError> { store(reference).snapshot().map(decode).transpose() }
pub async fn write_loop_state(reference:&LoopStoreRef,state:&LoopState)->Result<(),LoopStoreError> { let value=serde_json::to_value(state).map_err(|error|LoopStoreError::Invalid(error.to_string()))?; store(reference).write(&value).await.map_err(remap) }
pub async fn mutate_loop_state(reference:&LoopStoreRef,mutation:impl FnOnce(LoopState)->Result<LoopState,LoopStoreError>)->Result<LoopState,LoopStoreError> {
    let value=store(reference).mutate(|current| { let state=current.cloned().map(decode).transpose().map_err(|error|SidecarError::Invalid(error.to_string()))?.unwrap_or_else(||empty_loop_state(&reference.session_id)); let next=mutation(state).map_err(|error|SidecarError::Invalid(error.to_string()))?; serde_json::to_value(next).map_err(|error|SidecarError::Invalid(error.to_string())) }).await.map_err(remap)?; decode(value)
}
pub fn clear_loop_state_snapshot(reference:&LoopStoreRef) { store(reference).clear(); }
#[cfg(test)] mod tests {
    use super::*;
    fn reference(dir:&Path)->LoopStoreRef { LoopStoreRef { base_dir:dir.into(),session_id:"session/one".into() } }
    use std::path::Path;
    #[tokio::test] async fn missing_store_is_empty_without_creating_file() { let temp=tempfile::tempdir().unwrap(); let reference=reference(temp.path()); assert!(read_loop_state(&reference).await.unwrap().is_none()); let state=load_loop_state(&reference).await.unwrap(); assert!(state.entries.is_empty()); assert!(!Path::new(&loop_state_file_path(&reference)).exists()); }
    #[tokio::test] async fn writes_roundtrip_and_update_snapshot() { let temp=tempfile::tempdir().unwrap(); let reference=reference(temp.path()); let mut state=empty_loop_state(&reference.session_id); state.updated_at=5.0; write_loop_state(&reference,&state).await.unwrap(); assert_eq!(read_loop_state(&reference).await.unwrap(),Some(state.clone())); assert_eq!(snapshot_loop_state(&reference).unwrap(),Some(state)); clear_loop_state_snapshot(&reference); assert!(snapshot_loop_state(&reference).unwrap().is_none()); }
    #[tokio::test] async fn malformed_json_fails_closed() { let temp=tempfile::tempdir().unwrap(); let reference=reference(temp.path()); std::fs::write(loop_state_file_path(&reference),"{").unwrap(); let result=read_loop_state(&reference).await; assert!(matches!(result,Err(LoopStoreError::Invalid(_)))); }
    #[tokio::test] async fn wrong_version_remaps_shared_error() { let temp=tempfile::tempdir().unwrap(); let reference=reference(temp.path()); std::fs::write(loop_state_file_path(&reference),r#"{"version":2,"sessionId":"session/one"}"#).unwrap(); let result=read_loop_state(&reference).await; assert!(matches!(result,Err(LoopStoreError::UnsupportedVersion(message)) if message.contains("loop store"))); }
    #[tokio::test] async fn concurrent_mutations_serialize_per_file() { let temp=tempfile::tempdir().unwrap(); let reference=reference(temp.path()); let (first,second)=tokio::join!(mutate_loop_state(&reference,|mut state|{ state.updated_at+=1.0; Ok(state) }),mutate_loop_state(&reference,|mut state|{ state.updated_at+=1.0; Ok(state) })); first.unwrap(); second.unwrap(); let result=load_loop_state(&reference).await.unwrap(); assert_eq!(result.updated_at,2.0); }
}
