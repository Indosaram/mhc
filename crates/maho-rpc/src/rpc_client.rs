use std::collections::{BTreeSet,VecDeque};
use serde_json::Value;
pub const MAX_PENDING_SESSION_EVENTS:usize=512;
pub const MAX_PENDING_SESSION_EVENT_BYTES:usize=1024*1024;
pub struct RpcSocketClient{stream:tokio::net::UnixStream,pub frames:RpcClientFrames,reader:crate::jsonl::JsonlLineReader,lines:VecDeque<crate::jsonl::LineRecord>}
impl RpcSocketClient{
    pub async fn connect(path:&std::path::Path)->std::io::Result<Self>{Ok(Self::from_stream(tokio::net::UnixStream::connect(path).await?))}
    pub fn from_stream(stream:tokio::net::UnixStream)->Self{Self{stream,frames:RpcClientFrames::default(),reader:crate::jsonl::JsonlLineReader::default(),lines:VecDeque::new()}}
    pub async fn send(&mut self,command:Value,route:bool,expect_response:bool)->std::io::Result<Value>{use tokio::io::AsyncWriteExt;let command=self.frames.command(command,route,expect_response);let line=crate::jsonl::serialize_json_line(&command)?;self.stream.write_all(line.as_bytes()).await?;Ok(command)}
    pub async fn request(&mut self,command:Value,route:bool,mut on_event:impl FnMut(Value),on_response:impl FnOnce(&Value))->std::io::Result<Value>{
        let kind=command["type"].as_str().unwrap_or_default().to_owned();
        let command=self.send(command,route,true).await?;
        let id=command["id"].as_str().expect("assigned request id").to_owned();
        let response=tokio::time::timeout(std::time::Duration::from_secs(30),async{
            loop{match self.receive().await?{
                Some(ClientFrame::Response(response)) if response["id"].as_str()==Some(&id)=>return Ok(response),
                Some(ClientFrame::Event(event))=>on_event(event),
                Some(_)=>{},
                None=>return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe,"RPC transport is gone")),
            }}
        }).await;
        self.frames.pending.remove(&id);
        let response=response.map_err(|_|std::io::Error::new(std::io::ErrorKind::TimedOut,format!("Timeout waiting for response to {kind}. Stderr: ")))??;
        on_response(&response);
        Ok(response)
    }
    pub async fn receive(&mut self)->std::io::Result<Option<ClientFrame>>{use tokio::io::AsyncReadExt;loop{
        if let Some(record)=self.lines.pop_front(){if let crate::jsonl::LineRecord::Line(line)=record{return Ok(Some(self.frames.handle_line(&line)));}continue;}
        let mut bytes=[0;8192];let count=self.stream.read(&mut bytes).await?;
        if count==0{self.lines.extend(self.reader.finish());if self.lines.is_empty(){self.frames.reject_pending();return Ok(None);}continue;}
        self.lines.extend(self.reader.push(&bytes[..count]));
    }}
}
#[derive(Debug,PartialEq)]pub enum ClientFrame{Response(Value),Event(Value),Ignored}
#[derive(Default)]pub struct RpcClientFrames{request_id:u64,pending:BTreeSet<String>,pub session_id:Option<String>,pub pending_open_session:bool,events:VecDeque<(String,Value,usize)>,event_bytes:usize}
impl RpcClientFrames{
    pub fn command(&mut self,mut command:Value,route:bool,expect_response:bool)->Value{
        let own_id=matches!(command["type"].as_str(),Some("extension_ui_response"|"extension_ui_progress")).then(||command["id"].as_str().map(str::to_owned)).flatten();
        let id=own_id.clone().unwrap_or_else(||{self.request_id+=1;format!("req_{}",self.request_id)});
        if route&&command.get("sessionId").is_none()&&let Some(session)=&self.session_id{command["sessionId"]=session.clone().into();}
        if own_id.is_none(){command["id"]=id.clone().into();}
        if expect_response{self.pending.insert(id);}
        command
    }
    pub fn handle_line(&mut self,line:&str)->ClientFrame{
        let Ok(value)=serde_json::from_str::<Value>(line)else{return ClientFrame::Ignored;};
        if value["type"]=="response"&&let Some(id)=value["id"].as_str()&&self.pending.remove(id){return ClientFrame::Response(value);}
        if let Some(session)=value["sessionId"].as_str()&&Some(session)!=self.session_id.as_deref(){
            if self.pending_open_session{let bytes=line.len();self.events.push_back((session.into(),value,bytes));self.event_bytes+=bytes;while self.events.len()>MAX_PENDING_SESSION_EVENTS||self.event_bytes>MAX_PENDING_SESSION_EVENT_BYTES{if let Some((_,_,bytes))=self.events.pop_front(){self.event_bytes-=bytes;}}}
            return ClientFrame::Ignored;
        }
        ClientFrame::Event(value)
    }
    pub fn flush_pending_session_events(&mut self)->Vec<Value>{self.event_bytes=0;self.events.drain(..).filter(|(session,_,_)|Some(session)==self.session_id.as_ref()).map(|(_,value,_)|value).collect()}
    pub fn reject_pending(&mut self)->Vec<String>{std::mem::take(&mut self.pending).into_iter().collect()}
}
#[cfg(test)]mod tests{
    use super::*;use serde_json::json;
    #[test]fn ui_reply_keeps_host_id_and_session_route_is_not_overwritten(){let mut client=RpcClientFrames{session_id:Some("s".into()),..Default::default()};let reply=client.command(json!({"type":"extension_ui_response","id":"ui"}),true,false);assert_eq!(reply["id"],"ui");assert_eq!(reply["sessionId"],"s");let command=client.command(json!({"type":"abort","sessionId":"other"}),true,true);assert_eq!(command["id"],"req_1");assert_eq!(command["sessionId"],"other");assert!(matches!(client.handle_line(r#"{"type":"response","id":"req_1","success":true}"#),ClientFrame::Response(_)));}
    #[test]fn startup_events_replay_only_for_selected_lease(){let mut client=RpcClientFrames{pending_open_session:true,..Default::default()};client.handle_line(r#"{"type":"agent_start","sessionId":"s"}"#);client.handle_line(r#"{"type":"agent_start","sessionId":"other"}"#);client.session_id=Some("s".into());assert_eq!(client.flush_pending_session_events().len(),1);assert!(client.flush_pending_session_events().is_empty());}
    #[test]fn retention_drops_oldest_at_record_budget(){let mut client=RpcClientFrames{pending_open_session:true,..Default::default()};for index in 0..513{client.handle_line(&json!({"sessionId":"s","index":index}).to_string());}client.session_id=Some("s".into());let events=client.flush_pending_session_events();assert_eq!(events.len(),512);assert_eq!(events[0]["index"],1);}
}
