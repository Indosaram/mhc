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
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct WorkerActivity{pub busy:bool,#[serde(skip_serializing_if="Option::is_none")]pub handoff_busy:Option<bool>,pub streaming:bool}
#[cfg(test)]mod tests{use super::*;#[test]fn credit_codes_struct_matches_the_grant_encoding(){assert_eq!(WORKER_CREDIT_CODES,WorkerCreditCodes{granted:1,conflict:2,limit:3});assert_eq!(worker_credit_code(SessionWriteGrant::Granted),WORKER_CREDIT_CODES.granted);assert_eq!(worker_credit_code(SessionWriteGrant::Conflict),WORKER_CREDIT_CODES.conflict);assert_eq!(worker_credit_code(SessionWriteGrant::Limit),WORKER_CREDIT_CODES.limit);}#[test]fn optional_snapshot_fields_are_omitted(){let snapshot=WorkerSnapshot{state:serde_json::json!({}),session_path:None,live_session_paths:vec![],busy:false,handoff_busy:None,streaming:false};let wire=serde_json::to_value(snapshot).unwrap();assert!(wire.get("sessionPath").is_none());assert!(wire.get("handoffBusy").is_none());assert_eq!(wire["liveSessionPaths"],serde_json::json!([]));}}
