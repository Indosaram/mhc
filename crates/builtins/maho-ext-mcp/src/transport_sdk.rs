use std::collections::BTreeMap;
use std::{process::Stdio,sync::{Arc,Mutex,atomic::{AtomicU64,Ordering}},time::Duration};
use serde_json::{Value,json};
use tokio::{io::{AsyncBufReadExt,AsyncWriteExt,BufReader},process::{Child,ChildStdin,Command},sync::{broadcast,oneshot},task::JoinHandle};
use crate::{errors::{McpError,McpErrorKind},log::McpLogger};
#[derive(Clone)]
pub enum McpTransportSpec {
    Stdio {command:String,args:Vec<String>,cwd:Option<String>,env:BTreeMap<String,String>},
    Http {url:url::Url,headers:BTreeMap<String,String>},
}
type Reply=oneshot::Sender<Result<Value,McpError>>;
pub struct McpClient {
    server:String,input:tokio::sync::Mutex<ChildStdin>,pending:Arc<Mutex<BTreeMap<u64,Reply>>>,next_id:AtomicU64,
    child:tokio::sync::Mutex<Child>,reader:JoinHandle<()>,stderr:JoinHandle<()>,
    pub notifications:broadcast::Sender<Value>,pub root_pid:Option<u32>,
    pub server_capabilities:tokio::sync::RwLock<Value>,pub server_info:tokio::sync::RwLock<Value>,pub instructions:tokio::sync::RwLock<Option<String>>,
}
fn failure(server:&str,kind:McpErrorKind,message:impl Into<String>,phase:&str)->McpError {
    let mut error=McpError::new(kind,message);error.server_name=Some(server.into());error.phase=Some(phase.into());error
}
impl McpClient {
    pub async fn materialize_stdio(server:&str,spec:&McpTransportSpec,logger:Arc<Mutex<McpLogger>>)->Result<Arc<Self>,McpError> {
        let McpTransportSpec::Stdio {command,args,cwd,env}=spec else{return Err(failure(server,McpErrorKind::Connect,"HTTP transport not materialized","create"));};
        let mut process=Command::new(command);process.args(args).env_clear();
        for key in ["HOME","LOGNAME","PATH","SHELL","TERM","USER"] {if let Some(value)=std::env::var_os(key){process.env(key,value);}}
        process.envs(env).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        if let Some(cwd)=cwd {process.current_dir(cwd);}
        let mut child=process.spawn().map_err(|e|failure(server,McpErrorKind::Connect,e.to_string(),"create"))?;let root_pid=child.id();
        let input=child.stdin.take().ok_or_else(||failure(server,McpErrorKind::Connect,"stdio input unavailable","create"))?;
        let output=child.stdout.take().ok_or_else(||failure(server,McpErrorKind::Connect,"stdio output unavailable","create"))?;
        let errors=child.stderr.take().ok_or_else(||failure(server,McpErrorKind::Connect,"stdio stderr unavailable","create"))?;
        let pending:Arc<Mutex<BTreeMap<u64,Reply>>>=Arc::new(Mutex::new(BTreeMap::new()));let replies=pending.clone();
        let (notifications,_)=broadcast::channel(256);let events=notifications.clone();let name=server.to_owned();
        let reader=tokio::spawn(async move {
            let mut lines=BufReader::new(output).lines();
            while let Ok(Some(line))=lines.next_line().await {
                let Ok(value)=serde_json::from_str::<Value>(&line) else{continue;};
                if let Some(id)=value.get("id").and_then(Value::as_u64) {
                    if let Some(sender)=replies.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&id) {
                        let result=if let Some(error)=value.get("error") {let mut failure=failure(&name,McpErrorKind::Protocol,error.get("message").and_then(Value::as_str).unwrap_or("MCP protocol error"),"request");failure.cause=Some(Box::new(error.clone()));Err(failure)}else{Ok(value.get("result").cloned().unwrap_or(Value::Null))};
                        let _=sender.send(result);
                    }else if value.get("method").is_some(){let _=events.send(value);}
                }else if value.get("method").is_some(){let _=events.send(value);}
            }
            for (_,sender) in std::mem::take(&mut *replies.lock().unwrap_or_else(std::sync::PoisonError::into_inner)) {let _=sender.send(Err(failure(&name,McpErrorKind::Connect,format!("MCP server {name} transport closed"),"close")));}
        });
        let stderr=tokio::spawn(async move {
            let mut lines=BufReader::new(errors).lines();while let Ok(Some(line))=lines.next_line().await {if !line.is_empty(){let _=logger.lock().unwrap_or_else(std::sync::PoisonError::into_inner).log("info",&line,None,Some("stderr"));}}
        });
        Ok(Arc::new(Self {server:server.into(),input:tokio::sync::Mutex::new(input),pending,next_id:AtomicU64::new(1),child:tokio::sync::Mutex::new(child),reader,stderr,notifications,root_pid,server_capabilities:tokio::sync::RwLock::new(Value::Null),server_info:tokio::sync::RwLock::new(Value::Null),instructions:tokio::sync::RwLock::new(None)}))
    }
    pub async fn request(&self,method:&str,params:Value,timeout:Duration)->Result<Value,McpError> {
        let id=self.next_id.fetch_add(1,Ordering::Relaxed);let (sender,receiver)=oneshot::channel();
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(id,sender);
        let result=async {
            self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})).await?;
            receiver.await.map_err(|_|failure(&self.server,McpErrorKind::Connect,"transport closed","request"))?
        };
        let result=tokio::time::timeout(timeout,result).await.unwrap_or_else(|_|Err(failure(&self.server,McpErrorKind::Timeout,format!("MCP request {method} timed out"),"request")));
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&id);result
    }
    async fn send(&self,value:&Value)->Result<(),McpError> {
        let mut text=value.to_string();text.push('\n');let mut input=self.input.lock().await;
        input.write_all(text.as_bytes()).await.map_err(|e|failure(&self.server,McpErrorKind::Connect,e.to_string(),"request"))?;
        input.flush().await.map_err(|e|failure(&self.server,McpErrorKind::Connect,e.to_string(),"request"))
    }
    pub async fn initialize(&self,timeout:Duration)->Result<(),McpError> {
        let response=self.request("initialize",json!({"protocolVersion":"2025-11-25","capabilities":{"elicitation":{}},"clientInfo":{"name":"senpi-mcp-client","version":"0.0.0"}}),timeout).await?;
        *self.server_capabilities.write().await=response.get("capabilities").cloned().unwrap_or(Value::Null);
        *self.server_info.write().await=response.get("serverInfo").cloned().unwrap_or(Value::Null);
        *self.instructions.write().await=response.get("instructions").and_then(Value::as_str).map(str::to_owned);
        self.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await
    }
    pub async fn close(&self)->Result<(),McpError> {
        let _=self.input.lock().await.shutdown().await;
        let mut child=self.child.lock().await;
        match tokio::time::timeout(Duration::from_millis(100),child.wait()).await {
            Ok(result)=>{result.map_err(|e|failure(&self.server,McpErrorKind::Connect,e.to_string(),"close"))?;}
            Err(_)=>{let _=child.start_kill();child.wait().await.map_err(|e|failure(&self.server,McpErrorKind::Connect,e.to_string(),"close"))?;}
        }
        Ok(())
    }
}
impl Drop for McpClient {fn drop(&mut self){self.reader.abort();self.stderr.abort();}}
