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
    server:String,io:ClientTransport,pending:Arc<Mutex<BTreeMap<u64,Reply>>>,next_id:AtomicU64,
    pub notifications:broadcast::Sender<Value>,pub closed:tokio::sync::watch::Sender<bool>,pub root_pid:Option<u32>,
    pub server_capabilities:tokio::sync::RwLock<Value>,pub server_info:tokio::sync::RwLock<Value>,pub instructions:tokio::sync::RwLock<Option<String>>,
    /// Upstream `StreamableHTTPClientTransport._protocolVersion`: unset until `initialize` negotiates it,
    /// then sent as `mcp-protocol-version` on every later HTTP request.
    protocol_version:std::sync::RwLock<Option<String>>,
    auth:tokio::sync::RwLock<Option<Arc<crate::auth::oauth_refresh::McpRefreshManager>>>,
    auth_challenge_handler:tokio::sync::RwLock<Option<Arc<dyn crate::auth::challenge::McpAuthChallengeHandler>>>,
    http_stream:Mutex<Option<JoinHandle<()>>>,
    pub resource_subscriptions:tokio::sync::Mutex<std::collections::BTreeSet<String>>,
}
enum ClientTransport {
    Stdio {input:tokio::sync::Mutex<Option<ChildStdin>>,child:Box<tokio::sync::Mutex<Child>>,reader:JoinHandle<()>,stderr:JoinHandle<()>},
    Http {client:reqwest::Client,url:url::Url,headers:BTreeMap<String,String>,session:tokio::sync::RwLock<Option<String>>},
}
fn take_sse_line(buffer:&mut Vec<u8>,skip_lf:&mut bool,first_line:&mut bool)->Option<String> {
    if *skip_lf && !buffer.is_empty(){if buffer[0]==b'\n'{buffer.remove(0);}*skip_lf=false;}
    let end=buffer.iter().position(|byte|matches!(*byte,b'\n'|b'\r'))?;
    *skip_lf=buffer[end]==b'\r';
    let line=buffer.drain(..=end).collect::<Vec<_>>();
    let mut line=String::from_utf8_lossy(&line[..line.len()-1]).into_owned();
    if std::mem::take(first_line) && line.starts_with('\u{feff}') {line.remove(0);}
    Some(line)
}
/// Step-up attempts (reauthorize + renew cycles) one logical HTTP request may spend across renewed clients.
const STEP_UP_ATTEMPTS:u32=3;
fn net_text(error:&reqwest::Error)->String {crate::log::redact_urls_in_text(&error.to_string())}
fn failure(server:&str,kind:McpErrorKind,message:impl Into<String>,phase:&str)->McpError {
    let mut error=McpError::new(kind,message);error.server_name=Some(server.into());error.phase=Some(phase.into());error
}
impl McpClient {
    pub async fn materialize_stdio(server:&str,spec:&McpTransportSpec,logger:Arc<Mutex<McpLogger>>)->Result<Arc<Self>,McpError> {
        Self::materialize_stdio_with_policy(server,spec,logger,false).await
    }
    pub async fn materialize_stdio_with_policy(server:&str,spec:&McpTransportSpec,logger:Arc<Mutex<McpLogger>>,project_policy:bool)->Result<Arc<Self>,McpError> {
        let McpTransportSpec::Stdio {command,args,cwd,env}=spec else{return Err(failure(server,McpErrorKind::Connect,"HTTP transport not materialized","create"));};
        let mut process=Command::new(command);process.args(args).env_clear();
        if !project_policy {for key in ["HOME","LOGNAME","PATH","SHELL","TERM","USER","USERPROFILE","SystemRoot","ComSpec","PATHEXT","TEMP","TMP","USERNAME"] {if let Some(value)=std::env::var_os(key) && !value.to_string_lossy().starts_with("()"){process.env(key,value);}}}
        process.envs(env).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        if let Some(cwd)=cwd {process.current_dir(cwd);}
        let mut child=process.spawn().map_err(|e|failure(server,McpErrorKind::Connect,e.to_string(),"create"))?;let root_pid=child.id();
        let input=child.stdin.take().ok_or_else(||failure(server,McpErrorKind::Connect,"stdio input unavailable","create"))?;
        let output=child.stdout.take().ok_or_else(||failure(server,McpErrorKind::Connect,"stdio output unavailable","create"))?;
        let errors=child.stderr.take().ok_or_else(||failure(server,McpErrorKind::Connect,"stdio stderr unavailable","create"))?;
        let pending:Arc<Mutex<BTreeMap<u64,Reply>>>=Arc::new(Mutex::new(BTreeMap::new()));let replies=pending.clone();
        let (notifications,_)=broadcast::channel(256);let events=notifications.clone();let name=server.to_owned();
        let (closed,_)=tokio::sync::watch::channel(false);let close_signal=closed.clone();
        let reader=tokio::spawn(async move {
            let mut lines=BufReader::new(output).lines();
            while let Ok(Some(line))=lines.next_line().await {
                let Ok(value)=serde_json::from_str::<Value>(&line) else{continue;};
                if value.get("id").is_some() && value.get("method").is_some(){let _=events.send(value);continue;}
                if let Some(id)=value.get("id").and_then(Value::as_u64) {
                    if let Some(sender)=replies.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&id) {
                        let result=if let Some(error)=value.get("error") {let mut failure=failure(&name,McpErrorKind::Protocol,error.get("message").and_then(Value::as_str).unwrap_or("MCP protocol error"),"request");failure.cause=Some(Box::new(error.clone()));Err(failure)}else{Ok(value.get("result").cloned().unwrap_or(Value::Null))};
                        let _=sender.send(result);
                    }else if value.get("method").is_some(){let _=events.send(value);}
                }else if value.get("method").is_some(){let _=events.send(value);}
            }
            for (_,sender) in std::mem::take(&mut *replies.lock().unwrap_or_else(std::sync::PoisonError::into_inner)) {let _=sender.send(Err(failure(&name,McpErrorKind::Connect,format!("MCP server {name} transport closed"),"close")));}
            close_signal.send_replace(true);
        });
        let stderr=tokio::spawn(async move {
            let mut lines=BufReader::new(errors).lines();while let Ok(Some(line))=lines.next_line().await {if !line.is_empty(){let _=logger.lock().unwrap_or_else(std::sync::PoisonError::into_inner).log("info",&line,None,Some("stderr"));}}
        });
        Ok(Arc::new(Self {server:server.into(),io:ClientTransport::Stdio {input:tokio::sync::Mutex::new(Some(input)),child:Box::new(tokio::sync::Mutex::new(child)),reader,stderr},pending,next_id:AtomicU64::new(1),notifications,closed,root_pid,server_capabilities:tokio::sync::RwLock::new(Value::Null),server_info:tokio::sync::RwLock::new(Value::Null),instructions:tokio::sync::RwLock::new(None),protocol_version:std::sync::RwLock::new(None),auth:tokio::sync::RwLock::new(None),auth_challenge_handler:tokio::sync::RwLock::new(None),http_stream:Mutex::new(None),resource_subscriptions:tokio::sync::Mutex::new(Default::default())}))
    }
    pub async fn materialize(server:&str,spec:&McpTransportSpec,logger:Arc<Mutex<McpLogger>>)->Result<Arc<Self>,McpError> {
        Self::materialize_with_policy(server,spec,logger,false).await
    }
    pub async fn materialize_with_policy(server:&str,spec:&McpTransportSpec,logger:Arc<Mutex<McpLogger>>,project_policy:bool)->Result<Arc<Self>,McpError> {
        match spec {
            McpTransportSpec::Stdio {..}=>Self::materialize_stdio_with_policy(server,spec,logger,project_policy).await,
            McpTransportSpec::Http {url,headers}=>{
                let client=reqwest::Client::builder().build().map_err(|e|failure(server,McpErrorKind::Connect,e.to_string(),"create"))?;
                let (notifications,_)=broadcast::channel(256);
                let (closed,_)=tokio::sync::watch::channel(false);
                Ok(Arc::new(Self {server:server.into(),io:ClientTransport::Http {client,url:url.clone(),headers:headers.clone(),session:tokio::sync::RwLock::new(None)},pending:Arc::new(Mutex::new(BTreeMap::new())),next_id:AtomicU64::new(1),notifications,closed,root_pid:None,server_capabilities:tokio::sync::RwLock::new(Value::Null),server_info:tokio::sync::RwLock::new(Value::Null),instructions:tokio::sync::RwLock::new(None),protocol_version:std::sync::RwLock::new(None),auth:tokio::sync::RwLock::new(None),auth_challenge_handler:tokio::sync::RwLock::new(None),http_stream:Mutex::new(None),resource_subscriptions:tokio::sync::Mutex::new(Default::default())}))
            }
        }
    }
    pub async fn request(&self,method:&str,params:Value,timeout:Duration)->Result<Value,McpError> {
        self.request_inner(method,params,timeout,None).await
    }
    pub async fn request_with_signal(&self,method:&str,params:Value,timeout:Duration,signal:&maho_ext_api::AbortSignal)->Result<Value,McpError> {
        self.request_inner(method,params,timeout,Some(signal)).await
    }
    async fn request_inner(&self,method:&str,params:Value,timeout:Duration,signal:Option<&maho_ext_api::AbortSignal>)->Result<Value,McpError> {
        let id=self.next_id.fetch_add(1,Ordering::Relaxed);let (sender,receiver)=oneshot::channel();
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(id,sender);
        struct PendingRequest {pending:Arc<Mutex<BTreeMap<u64,Reply>>>,id:u64}
        impl Drop for PendingRequest {fn drop(&mut self){self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&self.id);}}
        let _pending=PendingRequest {pending:self.pending.clone(),id};
        let result=async {
            let message=json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
            if matches!(self.io,ClientTransport::Http {..}) {return self.http_send(&message).await;}
            self.send(&message).await?;
            receiver.await.map_err(|_|failure(&self.server,McpErrorKind::Connect,"transport closed","request"))?
        };
        let result=tokio::select! {
            biased;
            ()=async {match signal {Some(signal)=>signal.cancelled().await,None=>std::future::pending().await}}=>{
                self.send(&json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id,"reason":"Request cancelled"}})).await?;
                Err(failure(&self.server,McpErrorKind::ToolExec,"Request cancelled","request"))
            }
            result=tokio::time::timeout(timeout,result)=>result.unwrap_or_else(|_|Err(failure(&self.server,McpErrorKind::Timeout,format!("MCP request {method} timed out"),"request"))),
        };
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&id);result
    }
    pub async fn set_auth(&self,refresh:Arc<crate::auth::oauth_refresh::McpRefreshManager>) {*self.auth.write().await=Some(refresh);}
    pub async fn set_auth_challenge_handler(&self,handler:Arc<dyn crate::auth::challenge::McpAuthChallengeHandler>) {*self.auth_challenge_handler.write().await=Some(handler);}
    async fn send(&self,value:&Value)->Result<(),McpError> {
        let ClientTransport::Stdio {input,..}=&self.io else{self.http_send(value).await?;return Ok(());};
        let mut text=value.to_string();text.push('\n');let mut input=input.lock().await;let Some(input)=input.as_mut() else{return Err(failure(&self.server,McpErrorKind::Connect,"stdio input closed","request"));};
        input.write_all(text.as_bytes()).await.map_err(|e|failure(&self.server,McpErrorKind::Connect,e.to_string(),"request"))?;
        input.flush().await.map_err(|e|failure(&self.server,McpErrorKind::Connect,e.to_string(),"request"))
    }
    fn with_protocol_version(&self,request:reqwest::RequestBuilder)->reqwest::RequestBuilder {
        match self.protocol_version.read().unwrap_or_else(std::sync::PoisonError::into_inner).as_deref() {Some(version)=>request.header("mcp-protocol-version",version),None=>request}
    }
    async fn http_request(&self,value:&Value)->Result<reqwest::Response,McpError> {
        let ClientTransport::Http {client,url,headers,session}=&self.io else{return Err(failure(&self.server,McpErrorKind::Protocol,"not HTTP","request"));};
        let mut request=self.with_protocol_version(client.post(url.clone()).header("accept","application/json, text/event-stream")).json(value);
        for (name,value) in headers {request=request.header(name,value);}
        if let Some(refresh)=self.auth.read().await.as_ref() {
            let tokens=refresh.ensure_fresh().await.map_err(|error|{let terminal=crate::needs_auth::is_oauth_needs_auth_error(&error);let mut result=failure(&self.server,if terminal{McpErrorKind::Auth}else{McpErrorKind::Connect},error.to_string(),"request");result.retriable = !terminal;result})?;
            let tokens=tokens.ok_or_else(||failure(&self.server,McpErrorKind::Auth,format!("MCP server {} requires authorization",self.server),"request"))?;
            request=request.bearer_auth(tokens.access_token);
        }
        if let Some(id)=session.read().await.as_ref(){request=request.header("mcp-session-id",id);}
        request.send().await.map_err(|e|failure(&self.server,McpErrorKind::Connect,net_text(&e),"request"))
    }
    fn http_status_error(&self,status:u16,www_authenticate:Option<&str>)->McpError {
        let mut error=failure(&self.server,if status==401{McpErrorKind::Auth}else{McpErrorKind::Protocol},format!("HTTP {status}"),"request");
        let mut cause=json!({"status":status});
        if let Some(raw)=www_authenticate {
            cause["wwwAuthenticate"]=Value::String(raw.into());
            if let Some(challenge)=crate::auth::challenge::parse_www_authenticate(raw) && !challenge.scopes.is_empty() {cause["requiredScopes"]=json!(challenge.scopes);}
        }
        error.cause=Some(Box::new(cause));error
    }
    async fn http_send(&self,value:&Value)->Result<Value,McpError> {self.http_send_budgeted(value,STEP_UP_ATTEMPTS).await}
    /// `budget` is the number of step-up cycles still available to this logical request; it is shared
    /// with every renewed client the request is retried on.
    fn http_send_budgeted<'a>(&'a self,value:&'a Value,budget:u32)->futures::future::BoxFuture<'a,Result<Value,McpError>> {Box::pin(async move {
        let ClientTransport::Http {session,..}=&self.io else{return Err(failure(&self.server,McpErrorKind::Protocol,"not HTTP","request"));};
        let mut stepped=false;let mut forced=false;
        let mut response=loop {
            let response=self.http_request(value).await?;
            if response.status().is_success() {break response;}
            let status=response.status().as_u16();
            let www=response.headers().get("www-authenticate").and_then(|v|v.to_str().ok()).map(str::to_owned);
            drop(response);
            let auth=self.auth.read().await.clone();
            if let Some(challenge)=crate::auth::challenge::step_up_required(status,www.as_deref()) && !stepped {
                stepped=true;
                let required=match &auth {
                    Some(auth)=>{
                        let provider=auth.provider();
                        provider.escalate_scopes(&challenge.scopes);
                        provider.effective_scopes().unwrap_or_else(||challenge.scopes.clone())
                    }
                    None=>challenge.scopes.clone(),
                };
                let handler=self.auth_challenge_handler.read().await.clone();
                match handler {
                    Some(handler)=>{
                        if budget>0 && handler.reauthorize(required).await.is_ok() && let Ok(renewed)=handler.renew().await {
                            return renewed.http_send_budgeted(value,budget-1).await;
                        }
                    }
                    None if auth.is_some()=>continue,
                    None=>(),
                }
            }
            if matches!(status,401|403) && !forced && let Some(auth)=&auth {
                forced=true;
                if auth.force_refresh().await.is_ok() {continue;}
            }
            return Err(self.http_status_error(status,www.as_deref()));
        };
        if let Some(id)=response.headers().get("mcp-session-id").and_then(|v|v.to_str().ok()) {*session.write().await=Some(id.into());}
        if response.status()==reqwest::StatusCode::ACCEPTED || response.status()==reqwest::StatusCode::NO_CONTENT {return Ok(Value::Null);}
        let is_sse=response.headers().get("content-type").and_then(|v|v.to_str().ok()).is_some_and(|s|s.starts_with("text/event-stream"));
        if !is_sse {
            let reply=response.json::<Value>().await.map_err(|e|failure(&self.server,McpErrorKind::Protocol,net_text(&e),"request"))?;
            return self.http_reply(reply,value.get("id"));
        }
        let mut buffer=Vec::new();let mut data=String::new();let mut event_type=String::new();let mut skip_lf=false;let mut first_line=true;
        while let Some(chunk)=response.chunk().await.map_err(|e|failure(&self.server,McpErrorKind::Connect,net_text(&e),"request"))? {
            buffer.extend_from_slice(&chunk);
            while let Some(line)=take_sse_line(&mut buffer,&mut skip_lf,&mut first_line) {
                if let Some(part)=line.strip_prefix("data:"){if !data.is_empty(){data.push('\n');}data.push_str(part.strip_prefix(' ').unwrap_or(part));}
                if let Some(kind)=line.strip_prefix("event:"){event_type=kind.strip_prefix(' ').unwrap_or(kind).into();}
                if line=="event" {event_type.clear();}
                if line.is_empty() {
                    let is_message=event_type.is_empty() || event_type=="message";event_type.clear();
                    if !is_message || data.is_empty(){data.clear();continue;}
                    let parsed=serde_json::from_str::<Value>(&data);data.clear();
                    let event=match parsed {Ok(event)=>event,Err(error)=>{maho_ext_api::diagnostic(&format!("MCP {} SSE message parse failed: {error}",self.server));continue;}};
                    if event.get("id")==value.get("id") && event.get("method").is_none(){return self.http_reply(event,value.get("id"));}
                    let _=self.notifications.send(event);
                }
            }
        }
        Err(failure(&self.server,McpErrorKind::Connect,"SSE transport closed before response","request"))
    })}
    fn http_reply(&self,reply:Value,id:Option<&Value>)->Result<Value,McpError> {
        if reply.get("id")!=id{return Err(failure(&self.server,McpErrorKind::Protocol,"MCP response id mismatch","request"));}
        if let Some(error)=reply.get("error") {let mut result=failure(&self.server,McpErrorKind::Protocol,error.get("message").and_then(Value::as_str).unwrap_or("MCP protocol error"),"request");result.cause=Some(Box::new(error.clone()));Err(result)}else{Ok(reply.get("result").cloned().unwrap_or(Value::Null))}
    }
    pub async fn initialize(self:&Arc<Self>,timeout:Duration)->Result<(),McpError> {
        let response=self.request("initialize",json!({"protocolVersion":"2025-11-25","capabilities":{"elicitation":{}},"clientInfo":{"name":"senpi-mcp-client","version":"0.0.0"}}),timeout).await?;
        *self.server_capabilities.write().await=response.get("capabilities").cloned().unwrap_or(Value::Null);
        *self.server_info.write().await=response.get("serverInfo").cloned().unwrap_or(Value::Null);
        *self.instructions.write().await=response.get("instructions").and_then(Value::as_str).map(str::to_owned);
        *self.protocol_version.write().unwrap_or_else(std::sync::PoisonError::into_inner)=response.get("protocolVersion").and_then(Value::as_str).map(str::to_owned);
        self.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await?;
        if matches!(self.io,ClientTransport::Http {..}) {self.start_http_notifications();}
        Ok(())
    }
    fn start_http_notifications(self:&Arc<Self>) {
        let mut stream=self.http_stream.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if stream.is_some(){return;}
        let weak=Arc::downgrade(self);
        *stream=Some(tokio::spawn(async move {
            let mut last_event_id=None;let mut retry_ms=1000u64;let mut failed_attempts=0u32;
            loop {
                let Some(owner)=weak.upgrade() else{return;};
                let ClientTransport::Http {client,url,headers,session}=&owner.io else{return;};
                let mut request=owner.with_protocol_version(client.get(url.clone()).header("accept","text/event-stream"));
                for (name,value) in headers {request=request.header(name,value);}
                if let Some(id)=session.read().await.as_ref(){request=request.header("mcp-session-id",id);}
                if let Some(id)=&last_event_id {request=request.header("last-event-id",id);}
                if let Some(refresh)=owner.auth.read().await.as_ref() {
                    match refresh.ensure_fresh().await {
                        Ok(Some(tokens))=>request=request.bearer_auth(tokens.access_token),
                        _=>return,
                    }
                }
                let notifications=owner.notifications.clone();drop(owner);
                match request.send().await {
                    Ok(response) if response.status()==reqwest::StatusCode::METHOD_NOT_ALLOWED=>return,
                    Ok(mut response) if response.status().is_success()=>{
                        failed_attempts=0;
                        let mut buffer=Vec::new();let mut data=String::new();let mut event_type=String::new();let mut skip_lf=false;let mut first_line=true;
                        while let Ok(Some(chunk))=response.chunk().await {
                            buffer.extend_from_slice(&chunk);
                            while let Some(line)=take_sse_line(&mut buffer,&mut skip_lf,&mut first_line) {
                                if let Some(part)=line.strip_prefix("data:"){if !data.is_empty(){data.push('\n');}data.push_str(part.strip_prefix(' ').unwrap_or(part));}
                                if let Some(kind)=line.strip_prefix("event:"){event_type=kind.strip_prefix(' ').unwrap_or(kind).into();}
                                if line=="event" {event_type.clear();}
                                if let Some(id)=line.strip_prefix("id:"){last_event_id=Some(id.trim_start_matches(' ').to_owned());}
                                if let Some(retry)=line.strip_prefix("retry:").and_then(|retry|retry.trim().parse::<u64>().ok()){retry_ms=retry;}
                                if line.is_empty() {
                                    if (event_type.is_empty() || event_type=="message") && !data.is_empty() && let Ok(value)=serde_json::from_str::<Value>(&data){let _=notifications.send(value);}
                                    data.clear();event_type.clear();
                                }
                            }
                        }
                    }
                    _=>{failed_attempts+=1;if failed_attempts>2{return;}}
                }
                let delay=retry_ms.saturating_mul(3u64.saturating_pow(failed_attempts))/2u64.saturating_pow(failed_attempts);
                tokio::time::sleep(Duration::from_millis(delay.min(30000))).await;
            }
        }));
    }
    pub fn install_elicitation(self:&Arc<Self>,ui:Option<Arc<dyn maho_ext_api::ExtensionUi>>)->tokio::task::JoinHandle<()> {
        let mut requests=self.notifications.subscribe();let weak=Arc::downgrade(self);
        tokio::spawn(async move {loop {
            let value=match requests.recv().await {Ok(value)=>value,Err(broadcast::error::RecvError::Lagged(_))=>continue,Err(_)=>return};
            let (Some(id),Some(method))=(value.get("id"),value.get("method").and_then(Value::as_str)) else{continue;};
            let Some(client)=weak.upgrade() else{return;};
            let response=if method=="elicitation/create" {
                let result=crate::elicitation::handle_mcp_elicitation(ui.as_deref(),value.get("params").unwrap_or(&Value::Null),Duration::from_millis(crate::elicitation::MCP_ELICITATION_TIMEOUT_MS)).await;
                json!({"jsonrpc":"2.0","id":id,"result":result})
            }else{json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}})};
            if let Err(error)=client.send(&response).await {maho_ext_api::diagnostic(&format!("MCP server request response failed: {error}"));}
        }})
    }
    pub async fn close(&self)->Result<(),McpError> {
        if let Some(task)=self.http_stream.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take(){task.abort();}
        let ClientTransport::Stdio {input,child,..}=&self.io else{
            if let ClientTransport::Http {client,url,headers,session}=&self.io && let Some(id)=session.write().await.take() {
                let mut request=self.with_protocol_version(client.delete(url.clone()).header("mcp-session-id",id));for (name,value) in headers {request=request.header(name,value);}let _=request.timeout(Duration::from_millis(400)).send().await;
            }
            return Ok(());
        };
        // Dropping the pipe is what delivers EOF; shutdown() on a ChildStdin only flushes.
        if let Some(mut stdin)=input.lock().await.take(){let _=stdin.shutdown().await;drop(stdin);}
        let mut child=child.lock().await;
        match tokio::time::timeout(Duration::from_secs(2),child.wait()).await {
            Ok(result)=>{result.map_err(|e|failure(&self.server,McpErrorKind::Connect,e.to_string(),"close"))?;}
            Err(_)=>{
                #[cfg(unix)]
                {if let Some(pid)=child.id().and_then(|pid|i32::try_from(pid).ok()).and_then(rustix::process::Pid::from_raw){let _=rustix::process::kill_process(pid,rustix::process::Signal::TERM);}}
                if tokio::time::timeout(Duration::from_secs(2),child.wait()).await.is_err(){let _=child.start_kill();}
                child.wait().await.map_err(|e|failure(&self.server,McpErrorKind::Connect,e.to_string(),"close"))?;
            }
        }
        Ok(())
    }
}
impl Drop for McpClient {fn drop(&mut self){
    if let Some(task)=self.http_stream.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take(){task.abort();}
    if let ClientTransport::Stdio {reader,stderr,..}=&self.io{reader.abort();stderr.abort();}
}}
