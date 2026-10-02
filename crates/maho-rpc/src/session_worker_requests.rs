use std::collections::BTreeMap;
use serde_json::Value;
use crate::session_worker_protocol::SESSION_WORKER_LIMITS;
struct Pending{bytes:usize,control:bool,deadline:Option<u64>,reply:Option<tokio::sync::oneshot::Sender<Result<Value,String>>>}
#[derive(Default)]
pub struct SessionWorkerRequests{pending:BTreeMap<u64,Pending>,serial:u64,closed:bool,opening_deadline:Option<u64>}
#[derive(Debug,thiserror::Error,PartialEq,Eq)]
pub enum WorkerRequestError{
    #[error("session_closing")]SessionClosing,
    #[error("session_worker_request_limit")]RequestLimit,
    #[error("{0}")]Serialization(String),
}
impl SessionWorkerRequests{
    pub fn active_count(&self)->usize{self.pending.len()}
    pub fn request(&mut self,message:&Value,now:u64)->Result<Value,WorkerRequestError>{
        if self.closed{return Err(WorkerRequestError::SessionClosing);}
        let command=message.get("type").and_then(Value::as_str)==Some("command");
        let control=command&&message.get("command").and_then(|command|command.get("type")).and_then(Value::as_str).is_some_and(|kind|matches!(kind,"abort"|"abort_bash"|"extension_ui_response"|"extension_ui_progress"));
        let bytes=serde_json::to_vec(message).map_err(|error|WorkerRequestError::Serialization(error.to_string()))?.len();
        let debt=self.pending.values().filter(|pending|pending.control==control).collect::<Vec<_>>();
        let (max_count,max_bytes)=if control{(SESSION_WORKER_LIMITS.control_requests,SESSION_WORKER_LIMITS.control_bytes)}else{(SESSION_WORKER_LIMITS.requests,SESSION_WORKER_LIMITS.request_bytes)};
        if debt.len()>=max_count||debt.iter().map(|pending|pending.bytes).sum::<usize>()+bytes>max_bytes{return Err(WorkerRequestError::RequestLimit);}
        self.serial+=1;
        let deadline=if command&&!control{None}else if control{Some(now+SESSION_WORKER_LIMITS.control_ms)}else{Some(*self.opening_deadline.get_or_insert(now+SESSION_WORKER_LIMITS.open_ms))};
        self.pending.insert(self.serial,Pending{bytes,control,deadline,reply:None});
        let mut request=message.clone();request["request"]=self.serial.into();Ok(request)
    }
    pub fn send_failed(&mut self,request:u64){self.pending.remove(&request);}
    pub fn submit(&mut self,message:&Value,now:u64,send:impl FnOnce(Value)->Result<(),String>)->Result<tokio::sync::oneshot::Receiver<Result<Value,String>>,WorkerRequestError>{
        let request=self.request(message,now)?;let id=request["request"].as_u64().expect("assigned worker request");
        let(sender,receiver)=tokio::sync::oneshot::channel();self.pending.get_mut(&id).expect("admitted request").reply=Some(sender);
        if let Err(error)=send(request)&&let Some(pending)=self.pending.remove(&id)&&let Some(reply)=pending.reply{let _=reply.send(Err(error));}
        Ok(receiver)
    }
    pub fn receive(&mut self,message:&Value)->Option<Result<Value,String>>{
        let request=message.get("request")?.as_u64()?;let pending=self.pending.remove(&request)?;
        let result=if message.get("type").and_then(Value::as_str)==Some("result")&&let Some(error)=message.get("error").and_then(Value::as_str).filter(|error|!error.is_empty()){Err(error.into())}else{Ok(message.clone())};
        if let Some(reply)=pending.reply{let _=reply.send(result.clone());}
        Some(result)
    }
    pub fn timed_out_requests(&self,now:u64)->Vec<u64>{self.pending.iter().filter(|(_,pending)|pending.deadline.is_some_and(|deadline|now>=deadline)).map(|(request,_)|*request).collect()}
    pub fn close(&mut self)->Vec<u64>{self.close_with_error("session_closing")}
    pub fn close_with_error(&mut self,error:&str)->Vec<u64>{self.closed=true;let pending=std::mem::take(&mut self.pending);let ids=pending.keys().copied().collect();for pending in pending.into_values(){if let Some(reply)=pending.reply{let _=reply.send(Err(error.into()));}}ids}
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn opening_budget_starts_on_use_and_does_not_reset(){let mut requests=SessionWorkerRequests::default();let first=requests.request(&serde_json::json!({"type":"commit"}),100000).unwrap();assert!(requests.timed_out_requests(129999).is_empty());requests.receive(&serde_json::json!({"type":"ready","request":first["request"]}));let next=requests.request(&serde_json::json!({"type":"bind"}),129000).unwrap();assert_eq!(requests.timed_out_requests(130000),vec![next["request"].as_u64().unwrap()]);}
    #[test]fn control_debt_is_independent_and_commands_have_no_deadline(){let mut requests=SessionWorkerRequests::default();for _ in 0..64{requests.request(&serde_json::json!({"type":"command","command":{"type":"prompt"}}),0).unwrap();}assert_eq!(requests.request(&serde_json::json!({"type":"command","command":{"type":"prompt"}}),0).unwrap_err(),WorkerRequestError::RequestLimit);for _ in 0..4{requests.request(&serde_json::json!({"type":"command","command":{"type":"abort"}}),0).unwrap();}assert_eq!(requests.timed_out_requests(5000).len(),4);assert_eq!(requests.active_count(),68);}
    #[test]fn replies_and_failed_sends_retire_debt(){let mut requests=SessionWorkerRequests::default();requests.request(&serde_json::json!({"type":"commit"}),0).unwrap();assert_eq!(requests.receive(&serde_json::json!({"type":"result","request":1,"error":"failed"})),Some(Err("failed".into())));assert!(requests.receive(&serde_json::json!({"type":"result","request":1})).is_none());requests.request(&serde_json::json!({"type":"commit"}),1).unwrap();requests.send_failed(2);assert_eq!(requests.active_count(),0);requests.close();assert_eq!(requests.request(&serde_json::json!({"type":"commit"}),2).unwrap_err(),WorkerRequestError::SessionClosing);}
}
