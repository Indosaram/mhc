use std::collections::{BTreeSet,VecDeque};
use serde_json::Value;
pub const MAX_PENDING_SESSION_EVENTS:usize=512;
pub const MAX_PENDING_SESSION_EVENT_BYTES:usize=1024*1024;
pub struct RpcSocketClient{stream:tokio::net::UnixStream,pub frames:RpcClientFrames,reader:crate::jsonl::JsonlLineReader,lines:VecDeque<crate::jsonl::LineRecord>}
impl RpcSocketClient{
    pub async fn connect(path:&std::path::Path)->std::io::Result<Self>{Ok(Self::from_stream(tokio::net::UnixStream::connect(path).await?))}
    pub async fn connect_authenticated(path:&std::path::Path,secret_path:&std::path::Path)->std::io::Result<Self>{
        let secret=crate::socket_transport::read_socket_secret(secret_path)?;
        let mut stream=tokio::net::UnixStream::connect(path).await?;
        crate::socket_transport::send_socket_handshake(&mut stream,&secret).await?;
        Ok(Self::from_stream(stream))
    }
    pub fn from_stream(stream:tokio::net::UnixStream)->Self{Self{stream,frames:RpcClientFrames::default(),reader:crate::jsonl::JsonlLineReader::default(),lines:VecDeque::new()}}
    pub async fn prompt(&mut self,message:&str,options:Value,on_event:impl FnMut(Value),mut disposition:impl FnMut(&str),mut preflight:impl FnMut(bool))->std::io::Result<()>{
        let mut command=options;
        command["type"]="prompt".into();command["message"]=message.into();
        let response=self.request(command,true,on_event,|response|{
            if response["success"]==true{disposition(response["data"]["disposition"].as_str().unwrap_or("handled"));preflight(true);}else{preflight(false);}
        }).await;
        match response{
            Err(error)=>{preflight(false);Err(error)},
            Ok(response) if response["success"]==true=>Ok(()),
            Ok(response)=>Err(std::io::Error::other(response["error"].as_str().unwrap_or_default())),
        }
    }
    pub async fn collect_events(&mut self,timeout:std::time::Duration)->std::io::Result<Vec<Value>>{
        tokio::time::timeout(timeout,async{
            let mut events=Vec::new();
            loop{match self.receive().await?{
                Some(ClientFrame::Event(event))=>{let settled=event["type"]=="agent_settled";events.push(event);if settled{return Ok(events);}},
                Some(ClientFrame::Response(_)|ClientFrame::Ignored)=>{},
                None=>return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe,"RPC transport is gone")),
            }}
        }).await.map_err(|_|std::io::Error::new(std::io::ErrorKind::TimedOut,"Timeout waiting for agent events. Stderr: "))?
    }
    pub async fn send(&mut self,command:Value,route:bool,expect_response:bool)->std::io::Result<Value>{
        use tokio::io::AsyncWriteExt;
        let command=self.frames.command(command,route,expect_response);
        let line=crate::jsonl::serialize_json_line(&command)?;
        if let Err(error)=self.stream.write_all(line.as_bytes()).await{
            if let Some(id)=command["id"].as_str(){self.frames.pending.remove(id);}
            return Err(error);
        }
        Ok(command)
    }
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
    pub async fn open_session(&mut self,mut options:Value,mut on_event:impl FnMut(Value))->std::io::Result<Value>{
        if self.frames.pending_open_session{return Err(std::io::Error::other("An open_session request is already in flight"));}
        self.frames.pending_open_session=true;
        options["type"]="open_session".into();
        let response=self.request(options,false,&mut on_event,|_|{}).await;
        self.frames.pending_open_session=false;
        match response{
            Ok(response) if response["success"]==true=>{
                let data=response["data"].clone();
                self.frames.session_id=data["sessionId"].as_str().map(str::to_owned);
                for event in self.frames.flush_pending_session_events(){on_event(event);}
                Ok(data)
            },
            response=>{
                self.frames.events.clear();self.frames.event_bytes=0;
                match response{Err(error)=>Err(error),Ok(response)=>Err(std::io::Error::other(response["error"].as_str().unwrap_or("RPC open_session failed"))) }
            },
        }
    }
    pub async fn close_session(&mut self,session_id:Option<&str>,on_event:impl FnMut(Value))->std::io::Result<()>{
        let Some(session_id)=session_id.map(str::to_owned).or_else(||self.frames.session_id.clone()).filter(|id|!id.is_empty())else{return Ok(());};
        if let Err(error)=self.request(serde_json::json!({"type":"close_session","sessionId":session_id}),false,on_event,|_|{}).await
            && !matches!(error.kind(),std::io::ErrorKind::BrokenPipe|std::io::ErrorKind::ConnectionReset|std::io::ErrorKind::NotConnected){return Err(error);}
        if self.frames.session_id.as_deref()==Some(&session_id){self.frames.session_id=None;}
        Ok(())
    }
}
#[derive(Debug,PartialEq)]pub enum ClientFrame{Response(Value),Event(Value),Ignored}

/// A command the host refused; `error_code` carries the typed code when the command defines one.
#[derive(Debug,Clone,PartialEq,thiserror::Error)]
#[error("{message}")]
pub struct RpcCommandError{pub message:String,pub error_code:Option<String>,pub error_data:Option<Value>}
/// Transport-level failure kinds senpi's `isTransportGoneError` recognizes.
pub const RPC_TRANSPORT_GONE_CODE:&str="rpc_transport_gone";
/// Whether an error means the shared host is unreachable (a caller should surface it as
/// transport loss rather than a command refusal).
pub fn is_transport_gone_error(error:&std::io::Error)->bool{matches!(error.kind(),std::io::ErrorKind::BrokenPipe|std::io::ErrorKind::ConnectionReset|std::io::ErrorKind::NotConnected|std::io::ErrorKind::UnexpectedEof)}
/// Typed client-error surface: a transport failure or a refused command.
#[derive(Debug,thiserror::Error)]
pub enum RpcClientError{
    #[error(transparent)]Transport(#[from]std::io::Error),
    #[error(transparent)]Command(#[from]RpcCommandError),
}
impl RpcClientError{
    pub fn error_code(&self)->Option<&str>{match self{Self::Command(error)=>error.error_code.as_deref(),Self::Transport(_)=>Some(RPC_TRANSPORT_GONE_CODE)}}
    pub fn is_transport_gone(&self)->bool{matches!(self,Self::Transport(_))}
}
pub type RpcClientResult<T>=Result<T,RpcClientError>;

/** The event families the shared-host proxy discriminates on (senpi `RpcClientEvent`). */
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum RpcClientEventKind{
    AgentStart,AgentSettled,MessageStart,MessageUpdate,MessageEnd,EntryAppended,
    BashStart,BashEnd,BashExecutionUpdate,QueueUpdate,
    CompactionStart,CompactionEnd,AutoRetryStart,AutoRetryEnd,
    ModelChanged,ThinkingLevelChanged,ServiceTierChanged,SessionSettingsChanged,SessionInfoChanged,
    ExtensionUiRequest,ExtensionUiProgress,ExtensionEvent,ExtensionError,
    SessionReplaced,SessionParked,SessionClosed,CommandsChanged,LoadedSurfacesChanged,
    AuthAccountsChanged,AccountFailover,AuthLoginUrl,AuthLoginEnd
}
/// Classify one wire record into the client-event families, or `None` for a record the
/// proxy ignores (responses, unknown additive records).
pub fn classify_rpc_client_event(value:&Value)->Option<RpcClientEventKind>{
    use RpcClientEventKind as K;
    Some(match value["type"].as_str()?{
        "agent_start"=>K::AgentStart,"agent_settled"=>K::AgentSettled,"message_start"=>K::MessageStart,"message_update"=>K::MessageUpdate,"message_end"=>K::MessageEnd,"entry_appended"=>K::EntryAppended,
        "bash_start"=>K::BashStart,"bash_end"=>K::BashEnd,"bash_execution_update"=>K::BashExecutionUpdate,"queue_update"=>K::QueueUpdate,
        "compaction_start"=>K::CompactionStart,"compaction_end"=>K::CompactionEnd,"auto_retry_start"=>K::AutoRetryStart,"auto_retry_end"=>K::AutoRetryEnd,
        "model_changed"=>K::ModelChanged,"thinking_level_changed"=>K::ThinkingLevelChanged,"service_tier_changed"=>K::ServiceTierChanged,"session_settings_changed"=>K::SessionSettingsChanged,"session_info_changed"=>K::SessionInfoChanged,
        "extension_ui_request"=>K::ExtensionUiRequest,"extension_ui_progress"=>K::ExtensionUiProgress,"extension_event"=>K::ExtensionEvent,"extension_error"=>K::ExtensionError,
        "session_replaced"=>K::SessionReplaced,"session_parked"=>K::SessionParked,"session_closed"=>K::SessionClosed,"commands_changed"=>K::CommandsChanged,"loaded_surfaces_changed"=>K::LoadedSurfacesChanged,
        "auth_accounts_changed"=>K::AuthAccountsChanged,"account_failover"=>K::AccountFailover,"auth_login_url"=>K::AuthLoginUrl,"auth_login_end"=>K::AuthLoginEnd,
        _=>return None,
    })
}
/** Provider-account records are connection-level, not part of the agent event stream. */
pub fn is_provider_account_event(value:&Value)->bool{matches!(value["type"].as_str(),Some("auth_accounts_changed"|"account_failover"))}

pub type RpcEventListener=std::sync::Arc<dyn Fn(&Value)+Send+Sync>;
/// Handle that removes one listener when dropped, matching senpi's returned unsubscribe fn.
pub struct RpcClientUnsubscribe{listeners:std::sync::Arc<std::sync::Mutex<Vec<(u64,RpcEventListener)>>>,id:u64}
impl Drop for RpcClientUnsubscribe{fn drop(&mut self){self.listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|(id,_)|*id!=self.id);}}
fn dispatch_listeners(listeners:&std::sync::Mutex<Vec<(u64,RpcEventListener)>>,event:Value){for (_,listener) in listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter(){listener(&event);}}
/// The success `data` of one response, or a typed refusal.
pub fn get_data(response:&Value)->RpcClientResult<Value>{
    if response["success"]!=true{
        return Err(RpcCommandError{message:response["error"].as_str().unwrap_or_default().into(),error_code:response["errorCode"].as_str().map(str::to_owned),error_data:response.get("errorData").cloned()}.into());
    }
    Ok(response.get("data").cloned().unwrap_or(Value::Null))
}
/** The shared host's default endpoint for an agent directory (senpi `unix://` resolution). */
pub fn default_rpc_socket_path(agent_dir:&std::path::Path)->std::path::PathBuf{agent_dir.join("rpc").join("rpc.sock")}
/** How a client reaches a host: an existing socket, or the endpoint `ensure_host` starts. */
#[derive(Debug,Clone,Default)]
pub struct RpcClientOptions{
    pub socket_path:Option<std::path::PathBuf>,
    pub agent_dir:Option<std::path::PathBuf>,
    pub cwd:Option<String>,
    pub env:std::collections::HashMap<String,String>,
    pub provider:Option<String>,
    pub model:Option<String>,
    pub args:Vec<String>,
}
/// High-level client over the shared-host JSONL protocol: one connection, typed command
/// methods, and the event union the interactive proxy matches on (senpi `RpcClient`).
pub struct RpcClient{ inner:Option<RpcSocketClient>, listeners:std::sync::Arc<std::sync::Mutex<Vec<(u64,RpcEventListener)>>>, next_listener:u64, options:RpcClientOptions }
impl RpcClient{
    pub fn new(options:RpcClientOptions)->Self{Self{inner:None,listeners:Default::default(),next_listener:0,options}}
    /// Connect to an endpoint directly.
    pub async fn connect(path:&std::path::Path)->std::io::Result<Self>{Ok(Self{inner:Some(RpcSocketClient::connect(path).await?),listeners:Default::default(),next_listener:0,options:RpcClientOptions::default()})}
    pub fn from_stream(stream:tokio::net::UnixStream,options:RpcClientOptions)->Self{Self{inner:Some(RpcSocketClient::from_stream(stream)),listeners:Default::default(),next_listener:0,options}}
    /// Bring the transport up: connect to `options.socket_path`, or ensure a host serves the
    /// default endpoint for the agent directory.
    pub async fn start(&mut self)->RpcClientResult<()>{
        if self.inner.is_some(){return Err(std::io::Error::other("Client already started").into());}
        let path=match &self.options.socket_path{Some(path)=>path.clone(),None=>{
            let agent_dir=self.options.agent_dir.clone().unwrap_or_else(||std::path::PathBuf::from(maho_core::config::get_agent_dir()));
            let socket=default_rpc_socket_path(&agent_dir);
            let ensured=crate::host_ensure::ensure_host(crate::host_ensure::EnsureHostOptions{socket:socket.to_string_lossy().into_owned(),agent_dir:Some(agent_dir),host_args:self.options.args.clone(),..Default::default()}).await.map_err(|error|RpcClientError::Transport(std::io::Error::other(error.to_string())))?;
            std::path::PathBuf::from(ensured.socket)
        }};
        self.inner=Some(RpcSocketClient::connect(&path).await?);
        Ok(())
    }
    /// Drop the transport. Listeners are retained so a caller can `start` again.
    pub async fn stop(&mut self){self.inner=None;}
    /// Subscribe to every subsequent event record.
    pub fn on_event(&mut self,listener:RpcEventListener)->RpcClientUnsubscribe{self.next_listener+=1;let id=self.next_listener;self.listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((id,listener));RpcClientUnsubscribe{listeners:self.listeners.clone(),id}}
    pub fn session_id(&self)->Option<&str>{self.inner.as_ref().and_then(|inner|inner.frames.session_id.as_deref())}
    fn transport(&mut self)->RpcClientResult<&mut RpcSocketClient>{self.inner.as_mut().ok_or_else(||std::io::Error::other("RPC transport is not writable.").into())}
    async fn request_value(&mut self,command:Value,route:bool)->RpcClientResult<Value>{
        let listeners=self.listeners.clone();
        let response=self.transport()?.request(command,route,|event|dispatch_listeners(&listeners,event),|_|{}).await?;
        get_data(&response)
    }
    async fn fire_and_forget(&mut self,command:Value,route:bool)->RpcClientResult<()>{self.transport()?.send(command,route,false).await?;Ok(())}

    // -- lifecycle / client info -------------------------------------------------
    pub async fn set_client_info(&mut self,width:f64,capabilities:Option<Vec<String>>)->RpcClientResult<()>{
        let route=self.session_id().is_some();
        let mut command=serde_json::json!({"type":"set_client_info","width":width});
        if let Some(capabilities)=capabilities{command["capabilities"]=serde_json::json!(capabilities);}
        self.fire_and_forget(command,route).await
    }
    // -- sessions ----------------------------------------------------------------
    pub async fn open_session(&mut self,options:Value)->RpcClientResult<Value>{let listeners=self.listeners.clone();self.transport()?.open_session(options,|event|dispatch_listeners(&listeners,event)).await.map_err(Into::into)}
    pub async fn close_session(&mut self,session_id:Option<&str>)->RpcClientResult<()>{let listeners=self.listeners.clone();self.transport()?.close_session(session_id,|event|dispatch_listeners(&listeners,event)).await.map_err(Into::into)}
    pub async fn list_sessions(&mut self)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"list_sessions"}),false).await}
    pub async fn new_session(&mut self,parent_session:Option<&str>)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"new_session","parentSession":parent_session}),true).await}
    pub async fn switch_session(&mut self,session_path:&str,cwd_override:Option<&str>)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"switch_session","sessionPath":session_path,"cwdOverride":cwd_override}),true).await}
    pub async fn fork(&mut self,entry_id:&str,position:Option<&str>)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"fork","entryId":entry_id,"position":position}),true).await}
    pub async fn clone(&mut self)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"clone"}),true).await}
    pub async fn import_jsonl(&mut self,input_path:&str,cwd_override:Option<&str>)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"import_jsonl","inputPath":input_path,"cwdOverride":cwd_override}),true).await}
    pub async fn append_session_entry(&mut self,entry:Value)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"append_session_entry","entry":entry}),true).await}
    pub async fn append_user_message(&mut self,content:Value)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"append_user_message","content":content}),true).await}
    pub async fn send_custom_message(&mut self,message:Value,options:Option<Value>)->RpcClientResult<()>{
        let mut command=message;
        command["type"]="send_custom_message".into();
        if let Some(options)=options.and_then(|options|options.as_object().cloned()){for (key,value) in options{command[key]=value;}}
        self.fire_and_forget(command,true).await
    }
    pub async fn get_state(&mut self)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"get_state"}),true).await}
    pub async fn get_messages(&mut self)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"get_messages"}),true).await}
    pub async fn get_session_stats(&mut self)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"get_session_stats"}),true).await}
    pub async fn get_fork_messages(&mut self)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"get_fork_messages"}),true).await}
    pub async fn get_entries(&mut self,since:Option<&str>)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"get_entries","since":since}),true).await}
    pub async fn get_tree(&mut self)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"get_tree"}),true).await}
    pub async fn get_last_assistant_text(&mut self)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"get_last_assistant_text"}),true).await}
    pub async fn get_commands(&mut self)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"get_commands"}),true).await}
    pub async fn get_media(&mut self,tool_call_id:&str,content_index:f64)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"get_media","toolCallId":tool_call_id,"contentIndex":content_index}),true).await}
    pub async fn request_extension(&mut self,name:&str,data:Option<Value>)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"extension_request","name":name,"data":data}),true).await}
    pub async fn get_provider_accounts(&mut self,provider:&str)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"get_provider_accounts","provider":provider}),true).await}
    pub async fn pin_provider_account(&mut self,provider:&str,name:Option<&str>)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"account_pin","provider":provider,"name":name}),true).await}
    pub async fn remove_provider_account(&mut self,provider:&str,name:&str)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"account_remove","provider":provider,"name":name}),true).await}
    // -- turn / queue ------------------------------------------------------------
    pub async fn prompt(&mut self,message:&str,options:Value,disposition:impl FnMut(&str),preflight:impl FnMut(bool))->RpcClientResult<()>{
        let listeners=self.listeners.clone();
        self.transport()?.prompt(message,options,|event|dispatch_listeners(&listeners,event),disposition,preflight).await.map_err(Into::into)
    }
    pub async fn steer(&mut self,message:&str,images:Option<Value>,enqueue_order:Option<f64>)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"steer","message":message,"images":images,"enqueueOrder":enqueue_order}),true).await}
    pub async fn follow_up(&mut self,message:&str,images:Option<Value>,enqueue_order:Option<f64>)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"follow_up","message":message,"images":images,"enqueueOrder":enqueue_order}),true).await}
    pub async fn abort(&mut self)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"abort"}),true).await}
    pub async fn abort_compaction(&mut self)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"abort_compaction"}),true).await}
    pub async fn abort_branch_summary(&mut self)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"abort_branch_summary"}),true).await}
    pub async fn abort_retry(&mut self)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"abort_retry"}),true).await}
    pub async fn abort_bash(&mut self)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"abort_bash"}),true).await}
    pub async fn clear_queue(&mut self,abort_will_follow:Option<bool>)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"clear_queue","abortWillFollow":abort_will_follow}),true).await}
    pub async fn get_steering_messages(&mut self)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"get_steering_messages"}),true).await}
    pub async fn get_follow_up_messages(&mut self)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"get_follow_up_messages"}),true).await}
    pub async fn record_bash_result(&mut self,command:&str,result:Value,exclude_from_context:Option<bool>)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"record_bash_result","command":command,"result":result,"excludeFromContext":exclude_from_context}),true).await}
    pub async fn set_label(&mut self,entry_id:&str,label:Option<&str>)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"set_label","entryId":entry_id,"label":label}),true).await}
    pub async fn send_extension_ui_response(&mut self,response:Value)->RpcClientResult<()>{self.fire_and_forget(response,true).await}
    pub async fn send_extension_ui_progress(&mut self,progress:Value)->RpcClientResult<()>{self.fire_and_forget(progress,true).await}
    // -- model / settings --------------------------------------------------------
    pub async fn set_model(&mut self,provider:&str,model_id:&str)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"set_model","provider":provider,"modelId":model_id}),true).await}
    pub async fn cycle_model(&mut self,direction:&str)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"cycle_model","direction":direction}),true).await}
    pub async fn get_available_models(&mut self)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"get_available_models"}),true).await}
    pub async fn set_thinking_level(&mut self,level:&str,scope:Option<&str>)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"set_thinking_level","level":level,"scope":scope}),true).await}
    pub async fn cycle_thinking_level(&mut self)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"cycle_thinking_level"}),true).await}
    pub async fn get_available_thinking_levels(&mut self)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"get_available_thinking_levels"}),true).await}
    pub async fn set_fast_mode(&mut self,enabled:bool)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"set_fast_mode","enabled":enabled}),true).await}
    pub async fn get_fast_mode(&mut self)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"get_fast_mode"}),true).await}
    pub async fn set_steering_mode(&mut self,mode:&str)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"set_steering_mode","mode":mode}),true).await}
    pub async fn set_follow_up_mode(&mut self,mode:&str)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"set_follow_up_mode","mode":mode}),true).await}
    pub async fn set_auto_compaction(&mut self,enabled:bool)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"set_auto_compaction","enabled":enabled}),true).await}
    pub async fn set_auto_retry(&mut self,enabled:bool)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"set_auto_retry","enabled":enabled}),true).await}
    pub async fn set_favorite_models(&mut self,models:Value)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"set_favorite_models","models":models}),true).await}
    pub async fn set_scoped_models(&mut self,models:Value)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"set_scoped_models","models":models}),true).await}
    pub async fn set_session_name(&mut self,name:&str)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"set_session_name","name":name}),true).await}
    pub async fn reload(&mut self)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"reload"}),true).await}
    pub async fn check_reload_veto(&mut self)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"check_reload_veto"}),true).await}
    // -- compaction / edit / navigation ------------------------------------------..
    pub async fn compact(&mut self,custom_instructions:Option<&str>)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"compact","customInstructions":custom_instructions}),true).await}
    pub async fn navigate_tree(&mut self,target_id:&str,options:Value)->RpcClientResult<Value>{let mut command=options;command["type"]="navigate_tree".into();command["targetId"]=target_id.into();self.request_value(command,true).await}
    pub async fn edit_assistant_message(&mut self,entry_id:&str,text:&str,options:Value)->RpcClientResult<Value>{let mut command=options;command["type"]="edit_assistant_message".into();command["entryId"]=entry_id.into();command["text"]=text.into();self.request_value(command,true).await}
    pub async fn edit_user_message(&mut self,entry_id:&str,text:&str,options:Value)->RpcClientResult<Value>{let mut command=options;command["type"]="edit_user_message".into();command["entryId"]=entry_id.into();command["text"]=text.into();self.request_value(command,true).await}
    // -- bash / export -----------------------------------------------------------
    pub async fn bash(&mut self,command:&str,options:Value)->RpcClientResult<Value>{let mut request=options;request["type"]="bash".into();request["command"]=command.into();self.request_value(request,true).await}
    pub async fn cleanup_bash_output(&mut self,path:&str)->RpcClientResult<()>{self.fire_and_forget(serde_json::json!({"type":"cleanup_bash_output","path":path}),true).await}
    pub async fn export_jsonl(&mut self,output_path:Option<&str>)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"export_jsonl","outputPath":output_path}),true).await}
    pub async fn export_html(&mut self,output_path:Option<&str>,theme_name:Option<&str>)->RpcClientResult<Value>{self.request_value(serde_json::json!({"type":"export_html","outputPath":output_path,"themeName":theme_name}),true).await}
    // -- event waits -------------------------------------------------------------
    /// Wait for `agent_settled`, bounded by `timeout`.
    pub async fn wait_for_idle(&mut self,timeout:std::time::Duration)->RpcClientResult<()>{
        let listeners=self.listeners.clone();
        tokio::time::timeout(timeout,async{
            loop{match self.transport()?.receive().await?{
                Some(ClientFrame::Event(event))=>{dispatch_listeners(&listeners,event.clone());if event["type"]=="agent_settled"{return Ok(());}}
                Some(_)=>{},
                None=>return Err(RpcClientError::Transport(std::io::Error::new(std::io::ErrorKind::BrokenPipe,"RPC transport is gone"))),
            }}
        }).await.map_err(|_|RpcClientError::Transport(std::io::Error::new(std::io::ErrorKind::TimedOut,"Timeout waiting for agent to become idle")))?
    }
    /// Collect agent events until `agent_settled`, dropping connection-level records.
    pub async fn collect_events(&mut self,timeout:std::time::Duration)->RpcClientResult<Vec<Value>>{
        let listeners=self.listeners.clone();
        tokio::time::timeout(timeout,async{
            let mut events=Vec::new();
            loop{match self.transport()?.receive().await?{
                Some(ClientFrame::Event(event))=>{dispatch_listeners(&listeners,event.clone());
                    if is_provider_account_event(&event)||matches!(event["type"].as_str(),Some("extension_event"|"bash_start"|"bash_end"|"extension_ui_request"|"session_replaced"|"session_parked")){continue;}
                    let settled=event["type"]=="agent_settled";events.push(event);if settled{return Ok(events);}}
                Some(_)=>{},
                None=>return Err(RpcClientError::Transport(std::io::Error::new(std::io::ErrorKind::BrokenPipe,"RPC transport is gone"))),
            }}
        }).await.map_err(|_|RpcClientError::Transport(std::io::Error::new(std::io::ErrorKind::TimedOut,"Timeout collecting events")))?
    }
    /// Send a prompt and return every event up to settle.
    pub async fn prompt_and_wait(&mut self,message:&str,images:Option<Value>,timeout:std::time::Duration)->RpcClientResult<Vec<Value>>{
        let listeners=self.listeners.clone();
        tokio::time::timeout(timeout,async{
            let mut events=Vec::new();
            let response=self.transport()?.request(serde_json::json!({"type":"prompt","message":message,"images":images}),true,|event|{
                dispatch_listeners(&listeners,event.clone());
                if classify_rpc_client_event(&event).is_some()&&!is_provider_account_event(&event){events.push(event);}
            },|_|{}).await?;
            get_data(&response)?;
            if events.iter().any(|event|event["type"]=="agent_settled"){return Ok(events);}
            loop{match self.transport()?.receive().await?{
                Some(ClientFrame::Event(event))=>{
                    dispatch_listeners(&listeners,event.clone());
                    if is_provider_account_event(&event)||matches!(event["type"].as_str(),Some("extension_event"|"bash_start"|"bash_end"|"extension_ui_request"|"session_replaced"|"session_parked")){continue;}
                    let settled=event["type"]=="agent_settled";events.push(event);if settled{return Ok(events);}
                },
                Some(_)=>{},None=>return Err(RpcClientError::Transport(std::io::Error::new(std::io::ErrorKind::BrokenPipe,"RPC transport is gone"))),
            }}
        }).await.map_err(|_|RpcClientError::Transport(std::io::Error::new(std::io::ErrorKind::TimedOut,"Timeout collecting events")))?
    }
}

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
    #[test]fn startup_retention_counts_utf8_bytes_and_releases_debt_after_replay(){
        let mut client=RpcClientFrames{pending_open_session:true,..Default::default()};
        let payload="\u{1f600}".repeat(MAX_PENDING_SESSION_EVENT_BYTES/8);
        let first=json!({"sessionId":"s","index":1,"payload":payload}).to_string();
        let second=json!({"sessionId":"s","index":2,"payload":payload}).to_string();
        assert!(first.len()+second.len()>MAX_PENDING_SESSION_EVENT_BYTES);
        assert!(first.chars().count()+second.chars().count()<MAX_PENDING_SESSION_EVENT_BYTES);
        assert_eq!(client.handle_line(&first),ClientFrame::Ignored);
        assert_eq!(client.handle_line(&second),ClientFrame::Ignored);
        client.session_id=Some("s".into());
        let events=client.flush_pending_session_events();
        assert_eq!(events.len(),1);assert_eq!(events[0]["index"],2);
        assert_eq!(client.event_bytes,0);assert!(client.events.is_empty());
    }
    #[test]fn client_event_union_classifies_known_records_only(){
        use RpcClientEventKind as K;
        for (kind, expected) in [("agent_start",K::AgentStart),("agent_settled",K::AgentSettled),("message_update",K::MessageUpdate),("session_replaced",K::SessionReplaced),("session_parked",K::SessionParked),("extension_ui_request",K::ExtensionUiRequest),("bash_execution_update",K::BashExecutionUpdate),("queue_update",K::QueueUpdate),("session_settings_changed",K::SessionSettingsChanged),("thinking_level_changed",K::ThinkingLevelChanged)]{assert_eq!(classify_rpc_client_event(&json!({"type":kind})),Some(expected));}
        assert_eq!(classify_rpc_client_event(&json!({"type":"response","success":true})),None);assert_eq!(classify_rpc_client_event(&json!({"type":"some_future_record"})),None);assert!(is_provider_account_event(&json!({"type":"account_failover"})));assert!(!is_provider_account_event(&json!({"type":"agent_start"})));
    }
    #[test]fn get_data_surfaces_the_typed_refusal_and_success_data(){
        let refused=get_data(&json!({"type":"response","success":false,"error":"stale","errorCode":"stale_leaf","errorData":{"leafId":"one"}})).unwrap_err();
        assert_eq!(refused.error_code(),Some("stale_leaf"));assert!(!refused.is_transport_gone());
        let data=get_data(&json!({"type":"response","success":true,"data":{"level":"high"}})).unwrap();assert_eq!(data["level"],"high");assert_eq!(get_data(&json!({"type":"response","success":true})).unwrap(),Value::Null);
    }
    #[test]fn transport_gone_and_default_endpoint(){
        assert!(is_transport_gone_error(&std::io::Error::new(std::io::ErrorKind::BrokenPipe,"RPC socket closed")));assert!(!is_transport_gone_error(&std::io::Error::new(std::io::ErrorKind::InvalidInput,"bad")));
        assert_eq!(default_rpc_socket_path(std::path::Path::new("/agent")),std::path::Path::new("/agent/rpc/rpc.sock"));
        assert!(RpcClientError::Transport(std::io::Error::other("x")).is_transport_gone());
    }
    #[tokio::test]async fn request_value_returns_data_and_dispatches_events_to_listeners(){
        use tokio::io::{AsyncBufReadExt,AsyncWriteExt,BufReader};
        let (client_stream,host)=tokio::net::UnixStream::pair().unwrap();
        let mut client=RpcClient::from_stream(client_stream,RpcClientOptions::default());
        let seen=std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured=seen.clone();
        let _subscription=client.on_event(std::sync::Arc::new(move|event|captured.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(event.clone())));
        let serve=async move{
            let (read,mut write)=host.into_split();let mut lines=BufReader::new(read).lines();
            let request:Value=serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();assert_eq!(request["type"],"get_state");
            let id=request["id"].as_str().unwrap();
            write.write_all(format!("{}\n",serde_json::json!({"type":"agent_start"})).as_bytes()).await.unwrap();
            write.write_all(format!("{}\n",serde_json::json!({"type":"response","id":id,"success":true,"data":{"isStreaming":false}})).as_bytes()).await.unwrap();
        };
        let (state,()) = tokio::join!(client.get_state(),serve);
        assert_eq!(state.unwrap()["isStreaming"],false);
        assert_eq!(seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len(),1);
        assert_eq!(classify_rpc_client_event(&seen.lock().unwrap()[0]),Some(RpcClientEventKind::AgentStart));
    }
    #[tokio::test]async fn unsubscribe_stops_delivery_and_stopped_client_reports_transport_gone(){
        let (client_stream,_host)=tokio::net::UnixStream::pair().unwrap();
        let mut client=RpcClient::from_stream(client_stream,RpcClientOptions::default());
        let seen=std::sync::Arc::new(std::sync::Mutex::new(0usize));
        let counter=seen.clone();
        let subscription=client.on_event(std::sync::Arc::new(move|_|{*counter.lock().unwrap_or_else(std::sync::PoisonError::into_inner)+=1;}));
        drop(subscription);
        client.stop().await;
        assert_eq!(*seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner),0);
        assert!(client.get_state().await.unwrap_err().is_transport_gone());
    }
}
