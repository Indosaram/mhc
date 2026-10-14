use serde::{Serialize,Deserialize};
pub use crate::session_path_reservations::SessionWriteGrant;
pub struct SessionWorkerLimits{pub workers:usize,pub requests:usize,pub control_requests:usize,pub control_bytes:usize,pub request_bytes:usize,pub output_bytes:usize,pub reservations:usize,pub open_ms:u64,pub control_ms:u64}
pub const SESSION_WORKER_LIMITS:SessionWorkerLimits=SessionWorkerLimits{workers:20,requests:64,control_requests:4,control_bytes:1024*1024,request_bytes:16*1024*1024,output_bytes:16*1024*1024,reservations:64,open_ms:30000,control_ms:5000};
pub const fn worker_credit_code(grant:SessionWriteGrant)->i32{match grant{SessionWriteGrant::Granted=>1,SessionWriteGrant::Conflict=>2,SessionWriteGrant::Limit=>3}}
/// Wire values the host writes into a worker's wait signal; `conflict` is the generic denial.
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub struct WorkerCreditCodes{pub granted:i32,pub conflict:i32,pub limit:i32}
pub const WORKER_CREDIT_CODES:WorkerCreditCodes=WorkerCreditCodes{granted:worker_credit_code(SessionWriteGrant::Granted),conflict:worker_credit_code(SessionWriteGrant::Conflict),limit:worker_credit_code(SessionWriteGrant::Limit)};
#[derive(Debug,Clone,Serialize,Deserialize,PartialEq)]
pub struct WorkerDisplay{pub revision:f64,pub width:f64,pub rendered:bool,pub capabilities:Vec<String>}
#[derive(Debug,Clone,Serialize,Deserialize,PartialEq)]
#[serde(rename_all="camelCase")]
pub struct WorkerSnapshot{pub state:serde_json::Value,#[serde(skip_serializing_if="Option::is_none")]pub session_path:Option<String>,pub live_session_paths:Vec<String>,pub busy:bool,#[serde(skip_serializing_if="Option::is_none")]pub handoff_busy:Option<bool>,pub streaming:bool}
#[derive(Debug,Clone,Serialize,Deserialize,PartialEq)]
#[serde(tag="type",rename_all="snake_case",rename_all_fields="camelCase")]
pub enum HostToSessionWorker{
    Prepare{request:u64,configuration:serde_json::Value,profile:serde_json::Value},
    Commit{request:u64},
    Bind{request:u64,session_id:String,display:WorkerDisplay,#[serde(skip_serializing_if="Option::is_none")]connection:Option<String>},
    Command{request:u64,command:serde_json::Value,#[serde(skip_serializing_if="Option::is_none")]connection:Option<String>,display:WorkerDisplay},
    Display{display:WorkerDisplay},CancelUi,Close,
    /// The host's answer to one blocking worker exchange (native stand-in for senpi's
    /// `SharedArrayBuffer` wait signal: the worker is a process, so the grant crosses the pipe).
    Credit{signal:u64,code:i32,#[serde(default,skip_serializing_if="Option::is_none")]display:Option<WorkerDisplay>},
}
/// The grant a credit code encodes (inverse of `worker_credit_code`); anything else is a timeout.
pub const fn grant_from_credit_code(code:i32)->Option<SessionWriteGrant>{match code{1=>Some(SessionWriteGrant::Granted),2=>Some(SessionWriteGrant::Conflict),3=>Some(SessionWriteGrant::Limit),_=>None}}
/// `SessionWorkerToHost` on the process wire: every wait signal is named by an id the host echoes
/// back in `HostToSessionWorker::Credit`.
#[derive(Debug,Clone,Serialize,Deserialize,PartialEq)]
#[serde(tag="type",rename_all="snake_case",rename_all_fields="camelCase")]
pub enum SessionWorkerWire{
    Prepared{request:u64,session_path:String},
    Ready{request:u64,snapshot:WorkerSnapshot},
    Result{request:u64,#[serde(default,skip_serializing_if="Option::is_none")]error:Option<String>},
    Reserve{path:String,signal:u64},
    Snapshot{snapshot:WorkerSnapshot,signal:u64,#[serde(default,skip_serializing_if="Option::is_none")]settled:Option<bool>},
    ControlDone{control:WorkerControlKind},
    Output{record:serde_json::Value,#[serde(default,skip_serializing_if="Option::is_none")]connection:Option<String>,signal:u64,activity:WorkerActivity,#[serde(default,skip_serializing_if="Option::is_none")]snapshot:Option<WorkerSnapshot>},
    Width{#[serde(default,skip_serializing_if="Option::is_none")]connection:Option<String>,width:f64,signal:u64},
    Capabilities{#[serde(default,skip_serializing_if="Option::is_none")]connection:Option<String>,capabilities:Vec<String>,signal:u64},
    RequestClose,Failure{error:String},
}
#[derive(Clone)]
pub enum SessionWorkerToHost{
    Prepared{request:u64,session_path:String},
    Ready{request:u64,snapshot:WorkerSnapshot},
    Result{request:u64,error:Option<String>},
    Reserve{path:String,signal:crate::session_worker_signals::WorkerSignal},
    Snapshot{snapshot:WorkerSnapshot,signal:crate::session_worker_signals::WorkerSignal,settled:Option<bool>},
    ControlDone{control:WorkerControlKind},
    Output{record:serde_json::Value,connection:Option<String>,signal:crate::session_worker_signals::WorkerSignal,activity:WorkerActivity,snapshot:Option<WorkerSnapshot>},
    Width{connection:Option<String>,width:f64,signal:crate::session_worker_signals::WorkerSignal},
    Capabilities{connection:Option<String>,capabilities:Vec<String>,signal:crate::session_worker_signals::WorkerSignal},
    RequestClose,Failure{error:String},
}
#[derive(Debug,Clone,Copy,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum WorkerControlKind{Display,CancelUi}
/// The launch profile on the worker wire (senpi posts the `RpcSessionLaunchProfile` object itself).
pub fn profile_to_wire(profile:&crate::session_registry::RpcSessionLaunchProfile)->serde_json::Value{
    let mut value=serde_json::json!({"cwd":profile.runtime.cwd});
    if let Some(preset)=&profile.runtime.permission_preset{value["permissionPreset"]=preset.clone().into();}
    if let Some((provider,model_id))=&profile.runtime.creation_model{value["creationModel"]=serde_json::json!({"provider":provider,"modelId":model_id});}
    if let Some(level)=&profile.runtime.initial_thinking_level{value["initialThinkingLevel"]=level.clone().into();}
    if let Some(auto_title)=profile.runtime.auto_title{value["autoTitle"]=auto_title.into();}
    if let Some(path)=&profile.session_path{value["sessionPath"]=path.clone().into();}
    if let Some(id)=&profile.durable_session_id{value["durableSessionId"]=id.clone().into();}
    if let Some(kind)=profile.session_kind{value["sessionKind"]=if kind==maho_ext_api::SessionKind::Worker{"worker"}else{"interactive"}.into();}
    if let Some(context)=&profile.session_context{value["sessionContext"]=serde_json::json!(context);}
    value
}
pub fn profile_from_wire(value:&serde_json::Value)->Result<crate::session_registry::RpcSessionLaunchProfile,String>{
    let text=|key:&str|value.get(key).and_then(serde_json::Value::as_str).map(str::to_owned);
    let cwd=text("cwd").ok_or("Invalid worker launch profile: cwd")?;
    let creation_model=value.get("creationModel").and_then(|model|Some((model.get("provider")?.as_str()?.to_owned(),model.get("modelId")?.as_str()?.to_owned())));
    let session_context=match value.get("sessionContext"){Some(context)=>Some(serde_json::from_value(context.clone()).map_err(|error|error.to_string())?),None=>None};
    Ok(crate::session_registry::RpcSessionLaunchProfile{
        runtime:maho_core::agent_session_runtime::AgentSessionLaunchProfile{cwd,permission_preset:text("permissionPreset"),creation_model,initial_thinking_level:text("initialThinkingLevel"),auto_title:value.get("autoTitle").and_then(serde_json::Value::as_bool)},
        session_path:text("sessionPath"),durable_session_id:text("durableSessionId"),
        session_kind:text("sessionKind").map(|kind|if kind=="worker"{maho_ext_api::SessionKind::Worker}else{maho_ext_api::SessionKind::Interactive}),
        session_context,
    })
}
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct WorkerActivity{pub busy:bool,#[serde(skip_serializing_if="Option::is_none")]pub handoff_busy:Option<bool>,pub streaming:bool}
#[cfg(test)]mod tests{use super::*;#[test]fn credit_codes_struct_matches_the_grant_encoding(){assert_eq!(WORKER_CREDIT_CODES,WorkerCreditCodes{granted:1,conflict:2,limit:3});assert_eq!(worker_credit_code(SessionWriteGrant::Granted),WORKER_CREDIT_CODES.granted);assert_eq!(worker_credit_code(SessionWriteGrant::Conflict),WORKER_CREDIT_CODES.conflict);assert_eq!(worker_credit_code(SessionWriteGrant::Limit),WORKER_CREDIT_CODES.limit);}#[test]fn optional_snapshot_fields_are_omitted(){let snapshot=WorkerSnapshot{state:serde_json::json!({}),session_path:None,live_session_paths:vec![],busy:false,handoff_busy:None,streaming:false};let wire=serde_json::to_value(snapshot).unwrap();assert!(wire.get("sessionPath").is_none());assert!(wire.get("handoffBusy").is_none());assert_eq!(wire["liveSessionPaths"],serde_json::json!([]));}
#[test]fn credit_codes_round_trip_to_grants(){for grant in [SessionWriteGrant::Granted,SessionWriteGrant::Conflict,SessionWriteGrant::Limit]{assert_eq!(grant_from_credit_code(worker_credit_code(grant)),Some(grant));}assert_eq!(grant_from_credit_code(0),None);}
#[test]fn process_wire_envelopes_keep_tags_and_signal_ids(){for value in [serde_json::json!({"type":"reserve","path":"/s","signal":7}),serde_json::json!({"type":"output","record":{"type":"agent_end"},"connection":"peer","signal":8,"activity":{"busy":true,"streaming":false}}),serde_json::json!({"type":"result","request":3}),serde_json::json!({"type":"request_close"})]{let parsed:SessionWorkerWire=serde_json::from_value(value.clone()).unwrap();assert_eq!(serde_json::to_value(parsed).unwrap(),value);}let credit=serde_json::json!({"type":"credit","signal":7,"code":1});let parsed:HostToSessionWorker=serde_json::from_value(credit.clone()).unwrap();assert_eq!(parsed,HostToSessionWorker::Credit{signal:7,code:1,display:None});assert_eq!(serde_json::to_value(parsed).unwrap(),credit);}
#[test]fn launch_profile_round_trips_over_the_worker_wire(){let profile=crate::session_registry::RpcSessionLaunchProfile{runtime:maho_core::agent_session_runtime::AgentSessionLaunchProfile{cwd:"/w".into(),permission_preset:Some("p".into()),creation_model:Some(("faux".into(),"m".into())),initial_thinking_level:Some("high".into()),auto_title:Some(false)},session_path:Some("/w/s.jsonl".into()),durable_session_id:Some("d".into()),session_kind:Some(maho_ext_api::SessionKind::Worker),session_context:Some(std::collections::BTreeMap::from([("k".into(),"v".into())]))};assert_eq!(profile_from_wire(&profile_to_wire(&profile)).unwrap(),profile);let bare=crate::session_registry::RpcSessionLaunchProfile{runtime:maho_core::agent_session_runtime::AgentSessionLaunchProfile{cwd:"/w".into(),..Default::default()},session_path:None,durable_session_id:None,session_kind:None,session_context:None};assert_eq!(profile_from_wire(&profile_to_wire(&bare)).unwrap(),bare);assert!(profile_from_wire(&serde_json::json!({})).is_err());}}
