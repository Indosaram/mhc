use serde::{Serialize,Deserialize};
pub use crate::session_path_reservations::SessionWriteGrant;
pub struct SessionWorkerLimits{pub workers:usize,pub requests:usize,pub control_requests:usize,pub control_bytes:usize,pub request_bytes:usize,pub output_bytes:usize,pub reservations:usize,pub open_ms:u64,pub control_ms:u64}
pub const SESSION_WORKER_LIMITS:SessionWorkerLimits=SessionWorkerLimits{workers:20,requests:64,control_requests:4,control_bytes:1024*1024,request_bytes:16*1024*1024,output_bytes:16*1024*1024,reservations:64,open_ms:30000,control_ms:5000};
pub const fn worker_credit_code(grant:SessionWriteGrant)->i32{match grant{SessionWriteGrant::Granted=>1,SessionWriteGrant::Conflict=>2,SessionWriteGrant::Limit=>3}}
#[derive(Debug,Clone,Serialize,Deserialize,PartialEq)]
pub struct WorkerDisplay{pub revision:f64,pub width:f64,pub rendered:bool,pub capabilities:Vec<String>}
#[derive(Debug,Clone,Serialize,Deserialize,PartialEq)]
#[serde(rename_all="camelCase")]
pub struct WorkerSnapshot{pub state:serde_json::Value,#[serde(skip_serializing_if="Option::is_none")]pub session_path:Option<String>,pub live_session_paths:Vec<String>,pub busy:bool,#[serde(skip_serializing_if="Option::is_none")]pub handoff_busy:Option<bool>,pub streaming:bool}
#[cfg(test)]mod tests{use super::*;#[test]fn credit_codes_and_optional_snapshot_fields(){assert_eq!(worker_credit_code(SessionWriteGrant::Granted),1);assert_eq!(worker_credit_code(SessionWriteGrant::Conflict),2);assert_eq!(worker_credit_code(SessionWriteGrant::Limit),3);let snapshot=WorkerSnapshot{state:serde_json::json!({}),session_path:None,live_session_paths:vec![],busy:false,handoff_busy:None,streaming:false};let wire=serde_json::to_value(snapshot).unwrap();assert!(wire.get("sessionPath").is_none());assert!(wire.get("handoffBusy").is_none());assert_eq!(wire["liveSessionPaths"],serde_json::json!([]));}}
