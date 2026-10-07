use std::collections::HashMap;
pub const RPC_SESSION_IDLE_EVICTION_MS_ENV:&str="SENPI_RPC_SESSION_IDLE_EVICTION_MS";
pub const RPC_HOST_EMPTY_EXIT_MS_ENV:&str="SENPI_RPC_HOST_EMPTY_EXIT_MS";
pub const RPC_CLOSE_GRACE_MS_ENV:&str="SENPI_RPC_CLOSE_GRACE_MS";
pub const DEFAULT_SESSION_IDLE_EVICTION_MS:f64=30.*60_000.;
pub const DEFAULT_HOST_EMPTY_EXIT_MS:f64=15.*60_000.;
#[derive(Debug,PartialEq)]pub struct HostIdlePolicy{pub idle_eviction_ms:f64,pub empty_exit_ms:f64}
pub enum HostMonitorSample{Lag(crate::loop_lag_watchdog::LagSample),Memory(crate::host_memory_sampler::MemorySample)}
pub async fn run_host_monitors(env:&HashMap<String,String>,activity:&crate::session_attribution::SessionActivityRegistry,blocked:&std::sync::Mutex<crate::loop_blocked_time::LoopBlockedTime>,mut memory:impl FnMut()->(u64,u64,u64),mut now:impl FnMut()->f64,publish:impl FnMut(HostMonitorSample),stopped:tokio::sync::watch::Receiver<bool>){
    let publish=std::sync::Mutex::new(publish);
    let mut lag=crate::loop_lag_watchdog::LoopLagWatchdog::new(env);
    let mut rss=crate::host_memory_sampler::HostMemorySampler::new(env);
    let stall=lag.run_shared(activity,blocked,&mut now,|sample|{publish.lock().unwrap_or_else(std::sync::PoisonError::into_inner)(HostMonitorSample::Lag(sample));},stopped.clone());
    let pressure=rss.run_until_stopped(&mut memory,|sample|{publish.lock().unwrap_or_else(std::sync::PoisonError::into_inner)(HostMonitorSample::Memory(sample));},stopped);
    tokio::join!(stall,pressure);
}
pub fn resolve_host_idle_policy(env:&HashMap<String,String>,idle_eviction_ms:Option<f64>,empty_exit_ms:Option<f64>)->HostIdlePolicy{
    HostIdlePolicy{idle_eviction_ms:idle_eviction_ms.or_else(||crate::host_lifecycle::parse_idle_exit_ms(env.get(RPC_SESSION_IDLE_EVICTION_MS_ENV).map(String::as_str))).unwrap_or(DEFAULT_SESSION_IDLE_EVICTION_MS),empty_exit_ms:empty_exit_ms.or_else(||crate::host_lifecycle::parse_idle_exit_ms(env.get(RPC_HOST_EMPTY_EXIT_MS_ENV).map(String::as_str))).unwrap_or(DEFAULT_HOST_EMPTY_EXIT_MS)}
}
/**
 * Creates the runtime an `open_session` needs. Supplied by the caller: the DEFAULT factory that
 * builds one from CLI configuration is the owner-blocked piece (maho-core needs a canonical
 * `createAgentSessionRuntime`; which `CliRuntimeConfiguration` to build from is maho-cli's).
 * This seam is the contract lane 35 consumes.
 */
pub type HostRuntimeFactory=std::sync::Arc<dyn Fn(crate::session_registry::RpcSessionLaunchProfile)->std::pin::Pin<Box<dyn std::future::Future<Output=Result<maho_core::agent_session_runtime::AgentSessionRuntime,String>>+Send>>+Send+Sync>;
/// Everything one shared host needs to serve sessions.
pub struct HostCoreOptions{
    pub agent_dir:std::path::PathBuf,
    pub cwd:String,
    pub create_runtime:HostRuntimeFactory,
    pub capabilities:Vec<String>,
    pub close_grace_ms:u64,
}
/// Process-local lifecycle owner for multi-session RPC runtimes (senpi's `createHostCore`
/// result): one registry, one binding per session, and one record writer per connection.
pub struct HostCore{
    pub registry:tokio::sync::Mutex<crate::session_registry::RpcSessionRegistry>,
    pub default_writer:std::sync::Arc<crate::session_event_writer::SessionWriterActor>,
    pub attachments:std::sync::Mutex<crate::session_command_router::ConnectionAttachments>,
    writers:std::sync::Mutex<std::collections::BTreeMap<String,std::sync::Arc<crate::session_event_writer::SessionWriterActor>>>,
    bindings:tokio::sync::Mutex<std::collections::BTreeMap<String,std::sync::Arc<crate::session_binding::RpcSessionBinding>>>,
    options:HostCoreOptions,
    /// Set only where a SIGUSR1 drain handler is actually installed (senpi `runSocketHost`): the
    /// `generation_handoff` capability is the proof a handler exists and is advertised with it.
    handoff_enabled:std::sync::atomic::AtomicBool,
    drain:std::sync::Mutex<crate::session_command_router::HandoffDrain>,
    /// Sessions a drain has already claimed, so a later sweep never parks one twice.
    handoff_closed:std::sync::Mutex<std::collections::BTreeSet<String>>,
    /// In-flight parks, so the drain exit waits for them (senpi `handoffParks`).
    handoff_parks:std::sync::atomic::AtomicUsize,
}
impl HostCore{
    pub fn new(options:HostCoreOptions,default_writer:std::sync::Arc<crate::session_event_writer::SessionWriterActor>)->Self{
        Self{registry:tokio::sync::Mutex::new(crate::session_registry::RpcSessionRegistry::default()),default_writer,attachments:Default::default(),writers:Default::default(),bindings:tokio::sync::Mutex::new(std::collections::BTreeMap::new()),options,handoff_enabled:std::sync::atomic::AtomicBool::new(false),drain:std::sync::Mutex::new(crate::session_command_router::HandoffDrain::default()),handoff_closed:std::sync::Mutex::new(std::collections::BTreeSet::new()),handoff_parks:std::sync::atomic::AtomicUsize::new(0)}
    }
    /// Registers the writer one connection's responses go to.
    pub fn register_connection(&self,connection:&str,writer:std::sync::Arc<crate::session_event_writer::SessionWriterActor>){self.writers.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(connection.into(),writer);}
    /// Releases a connection's writer and its session attachments.
    pub fn unregister_connection(&self,connection:&str){self.writers.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(connection);}
    /// Advertises `generation_handoff` only once a SIGUSR1 handler is actually installed (senpi
    /// `runSocketHost`): the capability IS the proof a handler exists, and a host without it is
    /// never signalled - SIGUSR1 would simply terminate it, sessions and all.
    pub fn enable_handoff(&self){self.handoff_enabled.store(true,std::sync::atomic::Ordering::SeqCst);}
    fn advertised_capabilities(&self)->Vec<String>{
        let mut capabilities=self.options.capabilities.clone();
        if self.handoff_enabled.load(std::sync::atomic::Ordering::SeqCst)&&!capabilities.iter().any(|capability|capability==crate::host_decision::GENERATION_HANDOFF_CAPABILITY){capabilities.push(crate::host_decision::GENERATION_HANDOFF_CAPABILITY.into());}
        capabilities
    }
    /// Broadcasts one host-lifecycle record to every registered connection (senpi
    /// `SessionEventWriter.broadcastHostRecord`); a host with no connection answers on its default writer.
    pub fn broadcast_record(&self,record:&serde_json::Value){
        let writers=self.writers.lock().unwrap_or_else(std::sync::PoisonError::into_inner).values().cloned().collect::<Vec<_>>();
        if writers.is_empty(){let _=self.default_writer.enqueue("",record.clone());}
        else{for writer in writers{let _=writer.enqueue("",record.clone());}}
    }
    /// Starts the drain this host parks itself with (senpi `SessionCommandRouter.beginDrain`).
    pub async fn begin_drain(self:&std::sync::Arc<Self>){self.drain.lock().unwrap_or_else(std::sync::PoisonError::into_inner).begin();let _=self.drain_sweep().await;}
    /// One drain pass (senpi `sweepDrain`): park every open session whose work has settled, then
    /// report whether the host holds nothing and the generation may leave. A busy accepted open
    /// blocks the pass exactly as senpi's `activeRequests.has(undefined)` guard does.
    pub async fn drain_sweep(self:&std::sync::Arc<Self>)->bool{
        if !self.drain.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_draining(){return false;}
        let(active_open,open)={let registry=self.registry.lock().await;
            let rows=registry.list_sessions(true);
            let active_open=rows.iter().any(|row|row["status"]=="opening");
            let open=rows.iter().filter(|row|row["status"]=="open").filter_map(|row|row["sessionId"].as_str().map(str::to_owned)).collect::<Vec<_>>();
            (active_open,open)};
        if !active_open{
            for handle in &open{
                if self.handoff_closed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(handle){continue;}
                let busy={let registry=self.registry.lock().await;registry.get(handle).and_then(|entry|entry.runtime.as_ref()).is_some_and(|runtime|crate::handoff_activity::is_handoff_busy(runtime.session().activity_snapshot()))};
                if busy{continue;}
                self.handoff_closed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(handle.clone());
                self.handoff_parks.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                let core=std::sync::Arc::clone(self);
                let handle=handle.clone();
                tokio::spawn(async move{
                    core.park_session(&handle).await;
                    core.handoff_parks.fetch_sub(1,std::sync::atomic::Ordering::SeqCst);
                    if core.registry.lock().await.get(&handle).is_some(){core.handoff_closed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&handle);}
                });
            }
        }
        let size={self.registry.lock().await.size()};
        let parks=self.handoff_parks.load(std::sync::atomic::Ordering::SeqCst);
        let step={self.drain.lock().unwrap_or_else(std::sync::PoisonError::into_inner).step(active_open,size,parks)};
        matches!(step,crate::session_command_router::DrainStep::Exit)
    }
    /// Parks one settled session for a handoff (senpi `evictIdleSession(sessionId,"handoff_parked")`):
    /// the routing handle ends exactly as a close ends it, the terminal record names the reason (and
    /// the file a retained session reopens by) so a client is not told the session vanished, and
    /// every attachment is drained because a drain ends the shared session.
    async fn park_session(&self,session_id:&str){
        let retained={let mut registry=self.registry.lock().await;
            let Some(entry)=registry.get_mut(session_id)else{return;};
            if entry.close.state!=crate::session_registry::RpcSessionState::Open{return;}
            let retained=if entry.close.retain_on_disconnect{entry.session_path.clone()}else{None};
            entry.close.attachments=0;entry.close.state=crate::session_registry::RpcSessionState::Closing;
            retained};
        if let Some(binding)=self.bindings.lock().await.remove(session_id){binding.cancel_pending_extension_ui_requests();binding.dispose().await;}
        {let mut registry=self.registry.lock().await;let _=registry.close_marked(session_id,self.options.close_grace_ms).await;}
        if let Ok(value)=serde_json::to_value(crate::rpc_types::RpcSessionLifecycleEvent::SessionClosed{session_id:session_id.into(),reason:Some("handoff_parked".into()),session_path:retained}){self.broadcast_record(&value);}
        self.default_writer.forget_session(session_id);
    }
    fn writer_for(&self,connection:Option<&str>)->std::sync::Arc<crate::session_event_writer::SessionWriterActor>{
        connection.and_then(|connection|self.writers.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(connection).cloned()).unwrap_or_else(||self.default_writer.clone())
    }
    fn respond(&self,connection:Option<&str>,response:&crate::rpc_types::RpcResponse){
        if let Ok(value)=serde_json::to_value(response){let _=self.writer_for(connection).enqueue(response.session_id.as_deref().unwrap_or(""),value);}
    }
    fn error(&self,connection:Option<&str>,id:Option<String>,command:&str,message:impl Into<String>,session_id:Option<String>){
        self.respond(connection,&crate::rpc_types::RpcResponse{id,record_type:crate::rpc_types::ResponseRecordType::Response,command:command.into(),session_id,result:crate::rpc_types::RpcResponseResult::Error{error:message.into(),error_code:None,error_data:None}});
    }
    fn success(&self,connection:Option<&str>,id:Option<String>,command:&str,data:Option<serde_json::Value>,session_id:Option<String>){
        self.respond(connection,&crate::rpc_types::RpcResponse{id,record_type:crate::rpc_types::ResponseRecordType::Response,command:command.into(),session_id,result:crate::rpc_types::RpcResponseResult::Success{data}});
    }
    /// Feed one inbound JSONL line on one connection (senpi's `handle` inside `createHostCore`).
    pub async fn handle(&self,connection:Option<&str>,line:&str)->Result<(),String>{
        let Ok(value)=serde_json::from_str::<serde_json::Value>(line)else{self.error(connection,None,"parse",format!("Failed to parse command: {}",crate::connection_handler::json_parse_error_message(line)),None);return Ok(());};
        if let Some(error)=crate::rpc_input_validation::rpc_command_shape_error(&value){self.error(connection,value["id"].as_str().map(str::to_owned),"parse",error,None);return Ok(());}
        let command=match serde_json::from_value::<crate::rpc_types::RpcCommand>(value.clone()){
            Ok(command)=>command,
            Err(error)=>{let kind=value["type"].as_str().unwrap_or_default();let message=if error.to_string().starts_with(&format!("unknown variant `{kind}`,")){format!("Unknown command: {kind}")}else{error.to_string()};self.error(connection,value["id"].as_str().map(str::to_owned),kind,message,None);return Ok(());}
        };
        if let Some(error)=crate::rpc_input_validation::rpc_command_payload_error(&value).map(str::to_owned).or_else(||crate::rpc_input_validation::rpc_message_length_error(&value)){self.error(connection,command.id.clone(),value["type"].as_str().unwrap_or_default(),error,command.session_id.clone());return Ok(());}
        match &command.body{
            crate::rpc_types::RpcCommandBody::GetProtocolInfo=>{
                let argv=std::env::args().skip(2).collect::<Vec<_>>();
                let mut data=crate::protocol_identity::host_launch_profile(&argv,&self.options.cwd).ok().map_or_else(||serde_json::json!({}),|profile|crate::protocol_identity::protocol_identity(profile,&std::env::vars().collect()));
                data["protocolVersion"]=crate::host_decision::HOST_PROTOCOL_VERSION.into();
                data["serverVersion"]=maho_core::engine_build_identity::engine_build_identity().text.clone().into();
                data["capabilities"]=serde_json::json!(self.advertised_capabilities());
                data["mode"]="multi".into();
                self.success(connection,command.id.clone(),"get_protocol_info",Some(data),None);
            },
            crate::rpc_types::RpcCommandBody::SetClientInfo{..}=>self.success(connection,command.id.clone(),"set_client_info",None,command.session_id.clone()),
            crate::rpc_types::RpcCommandBody::ListSessions{include_workers}=>{
                let sessions=self.registry.lock().await.list_sessions(include_workers.unwrap_or(false));
                self.success(connection,command.id.clone(),"list_sessions",Some(serde_json::json!({"sessions":sessions})),None);
            },
            crate::rpc_types::RpcCommandBody::OpenSession{session_path,cwd,provider,model_id,thinking_level,permission_preset,retain_on_disconnect,kind,context,auto_title,durable_session_id}=>{
                let profile=crate::session_registry::RpcSessionLaunchProfile{
                    runtime:maho_core::agent_session_runtime::AgentSessionLaunchProfile{cwd:cwd.clone().unwrap_or_else(||self.options.cwd.clone()),permission_preset:permission_preset.clone(),creation_model:provider.clone().zip(model_id.clone()),initial_thinking_level:thinking_level.clone(),auto_title:*auto_title},
                    session_path:session_path.clone(),durable_session_id:durable_session_id.clone(),
                    session_kind:kind.map(|kind|match kind{crate::rpc_types::SessionKind::Worker=>maho_ext_api::SessionKind::Worker,crate::rpc_types::SessionKind::Interactive=>maho_ext_api::SessionKind::Interactive}),
                    session_context:context.clone(),
                };
                let admitted={let mut registry=self.registry.lock().await;registry.admit_open(profile.clone(),retain_on_disconnect.unwrap_or(false),crate::connection_handler::now_ms())};
                match admitted{
                    Err(error)=>self.error(connection,command.id.clone(),"open_session",error.to_string(),None),
                    Ok(crate::session_registry::OpenAdmission::Attached(handle))=>{
                        let state=self.session_state(&handle).await;
                        self.success(connection,command.id.clone(),"open_session",Some(serde_json::json!({"sessionId":handle,"state":state,"attached":true})),None);
                    },
                    Ok(crate::session_registry::OpenAdmission::Create(handle))=>{
                        let created={let mut registry=self.registry.lock().await;registry.create_admitted_runtime(&handle,(self.options.create_runtime)(profile)).await};
                        match created{
                            Err(error)=>self.error(connection,command.id.clone(),"open_session",error.to_string(),None),
                            Ok(())=>{
                                let state=self.session_state(&handle).await;
                                if let Some(connection_id)=connection{self.attach_binding(connection,connection_id,&handle).await;}
                                self.success(connection,command.id.clone(),"open_session",Some(serde_json::json!({"sessionId":handle,"state":state})),None);
                            }
                        }
                    }
                }
            },
            crate::rpc_types::RpcCommandBody::CloseSession=>{
                let Some(session_id)=command.session_id.clone().or(self.only_session().await)else{self.error(connection,command.id.clone(),"close_session",crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION,None);return Ok(());};
                let admitted={let mut registry=self.registry.lock().await;let mut attachments=self.attachments.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    match registry.get_mut(&session_id){Some(entry)=>attachments.admit_close((connection,&session_id),&serde_json::json!({}),&mut entry.close,false,&self.default_writer),None=>Err(crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION.to_owned())}};
                match admitted{
                    Err(error)=>self.error(connection,command.id.clone(),"close_session",error,Some(session_id)),
                    Ok(_)=>{if let Some(binding)=self.bindings.lock().await.remove(&session_id){binding.dispose().await;}
                        match self.registry.lock().await.close_marked(&session_id,self.options.close_grace_ms).await{Err(error)=>self.error(connection,command.id.clone(),"close_session",error.to_string(),Some(session_id)),Ok(_)=>self.success(connection,command.id.clone(),"close_session",None,Some(session_id))}}
                }
            },
            _=>{
                let Some(session_id)=command.session_id.clone()else{self.error(connection,command.id.clone(),value["type"].as_str().unwrap_or_default(),crate::rpc_types::RPC_ERROR_MISSING_SESSION_ID,None);return Ok(());};
                let binding=self.bindings.lock().await.get(&session_id).cloned();
                match binding{Some(binding)=>binding.handle(&value).await?,None=>self.error(connection,command.id.clone(),value["type"].as_str().unwrap_or_default(),crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION,Some(session_id))}
            }
        }
        Ok(())
    }
    async fn session_state(&self,handle:&str)->serde_json::Value{
        let registry=self.registry.lock().await;
        registry.get(handle).and_then(|entry|entry.runtime.as_ref()).map_or_else(||serde_json::json!({}),|runtime|crate::connection_handler::build_rpc_session_state(runtime.session(),None))
    }
    async fn only_session(&self)->Option<String>{let registry=self.registry.lock().await;if registry.size()==1{registry.list_sessions(true).first().and_then(|row|row["sessionId"].as_str().map(str::to_owned))}else{None}}
    async fn attach_binding(&self,writer_connection:Option<&str>,attachment:&str,session_id:&str){
        let runtime={let registry=self.registry.lock().await;registry.get(session_id).and_then(|entry|entry.runtime.as_ref().map(std::sync::Arc::clone))};
        let Some(runtime)=runtime else{return;};
        let writer=self.writer_for(writer_connection);
        let binding=std::sync::Arc::new(crate::session_binding::create_rpc_session_binding(session_id.to_owned(),runtime.session().clone(),writer,self.options.capabilities.clone()).await);
        self.attachments.lock().unwrap_or_else(std::sync::PoisonError::into_inner).attach(attachment,session_id);
        self.bindings.lock().await.insert(session_id.to_owned(),binding);
    }
}
#[cfg(test)]mod handoff_tests{use super::*;
    fn idle_core()->std::sync::Arc<HostCore>{
        let create_runtime:HostRuntimeFactory=std::sync::Arc::new(|_profile:crate::session_registry::RpcSessionLaunchProfile|->std::pin::Pin<Box<dyn std::future::Future<Output=Result<maho_core::agent_session_runtime::AgentSessionRuntime,String>>+Send>>{Box::pin(async{Err("this host opens no session in this test".to_owned())})});
        let writer=std::sync::Arc::new(crate::session_event_writer::SessionWriterActor::new(tokio::io::sink()));
        std::sync::Arc::new(HostCore::new(HostCoreOptions{agent_dir:std::path::PathBuf::from("/tmp"),cwd:"/tmp".to_owned(),create_runtime,capabilities:vec![crate::custom_capability::EXTENSION_EVENTS_CAPABILITY.to_owned()],close_grace_ms:10_000},writer))
    }
    #[tokio::test]async fn handoff_is_advertised_only_after_its_handler_is_installed(){let core=idle_core();assert!(!core.advertised_capabilities().iter().any(|capability|capability==crate::host_decision::GENERATION_HANDOFF_CAPABILITY));core.enable_handoff();assert!(core.advertised_capabilities().iter().any(|capability|capability==crate::host_decision::GENERATION_HANDOFF_CAPABILITY));}
    #[tokio::test]async fn an_empty_host_drains_to_exit_once(){let core=idle_core();assert!(!core.drain_sweep().await);core.begin_drain().await;assert!(core.drain_sweep().await);}
}
/// The supervisor-lifetime and empty-exit triggers one shared host reacts to (senpi
/// `runSocketHost`'s watchdog binding and its `canExitWhenEmpty` window).
#[derive(Debug,Default)]
pub struct HostShutdownSignals{
    pub watchdog:Option<crate::host_watchdog::HostWatchdogConfig>,
    pub empty_exit_ms:Option<f64>,
}
/// The shutdown triggers a supervised host reads from its own environment: the canonical (or
/// branded) watchdog binding the supervisor set, and the empty-exit window from the idle policy.
pub fn host_shutdown_signals_from_env()->HostShutdownSignals{
    let env:HashMap<String,String>=std::env::vars().collect();
    let policy=resolve_host_idle_policy(&env,None,None);
    HostShutdownSignals{watchdog:crate::host_watchdog::read_host_watchdog_config_from_brand_env(&env),empty_exit_ms:Some(policy.empty_exit_ms)}
}
/// Serve the shared host: stdio when no socket was named, otherwise the public Unix socket
/// (senpi `runMultiSessionHost`).
pub async fn run_multi_session_host(core:std::sync::Arc<HostCore>,listen:Option<String>)->std::io::Result<()>{
    run_multi_session_host_with_signals(core,listen,host_shutdown_signals_from_env()).await
}
/// Serve the shared host with explicit shutdown triggers: the seam the CLI entry and its tests use.
pub async fn run_multi_session_host_with_signals(core:std::sync::Arc<HostCore>,listen:Option<String>,signals:HostShutdownSignals)->std::io::Result<()>{
    match listen.as_deref(){None|Some("stdio://")=>run_stdio_host(core).await,Some(path)=>run_socket_host(core,path.to_owned(),signals).await}
}
async fn run_stdio_host(core:std::sync::Arc<HostCore>)->std::io::Result<()>{
    use tokio::io::AsyncReadExt;
    let mut input=tokio::io::stdin();
    let mut reader=crate::jsonl::JsonlLineReader::new(crate::jsonl::MAX_RPC_LINE_CHARACTERS).map_err(std::io::Error::other)?;
    let mut chunk=[0u8;8192];
    let mut commands=Vec::new();
    loop{
        let count=tokio::select!{
            result=poll_host_commands(&mut commands),if !commands.is_empty()=>{result.map_err(std::io::Error::other)?;continue;},
            count=input.read(&mut chunk)=>count?,
        };
        let records=if count==0{reader.finish()}else{reader.push(&chunk[..count])};
        for record in records{let crate::jsonl::LineRecord::Line(line)=record else{continue};let core=core.clone();commands.push(Box::pin(async move{core.handle(None,&line).await}) as HostCommand);}
        if count==0{drop(commands);core.default_writer.flush().await.map_err(std::io::Error::other)?;return Ok(());}
    }
}
type HostCommand=std::pin::Pin<Box<dyn std::future::Future<Output=Result<(),String>>+Send>>;
async fn poll_host_commands(commands:&mut Vec<HostCommand>)->Result<(),String>{
    let(index,result)=std::future::poll_fn(|cx|{
        for(index,command)in commands.iter_mut().enumerate(){if let std::task::Poll::Ready(result)=command.as_mut().poll(cx){return std::task::Poll::Ready((index,result));}}
        std::task::Poll::Pending
    }).await;
    drop(commands.remove(index));result
}
async fn run_socket_host(core:std::sync::Arc<HostCore>,listen:String,signals:HostShutdownSignals)->std::io::Result<()>{
    let path=crate::host_ensure::normalize_socket_path(&listen).to_owned();
    if let Some(parent)=std::path::Path::new(&path).parent(){crate::host_daemon_paths::create_private_directory(parent)?;}
    let HostShutdownSignals{watchdog,empty_exit_ms}=signals;
    let public_socket=watchdog.as_ref().and_then(|config|config.public_socket.clone());
    // The public entry's ownership token lives in the supervisor's private directory, which the
    // watchdog cleanup removes - so it is read BEFORE that cleanup (senpi `beforeCleanup`).
    let public_owner:std::sync::Arc<std::sync::Mutex<Option<crate::socket_ownership::SocketFileIdentity>>>=std::sync::Arc::new(std::sync::Mutex::new(None));
    let owner_file=watchdog.as_ref().and_then(|config|config.scratch_dir.as_ref()).map(|dir|dir.join(crate::socket_ownership::PUBLIC_SOCKET_IDENTITY_FILE));
    let capture_slot=public_owner.clone();
    let capture=async move{
        if let Some(path)=owner_file&&let Ok(Some(identity))=crate::socket_ownership::read_socket_identity_file(&path){*capture_slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=Some(identity);}
        Ok(())
    };
    let callback_owner=public_owner.clone();
    let (reason_tx,reason_rx)=tokio::sync::watch::channel::<Option<String>>(None);
    // An absent watchdog is inert, not a shutdown signal when its task returns.
    let _reason_sender=reason_tx.clone();
    // A socket host installs the SIGUSR1 drain below, so it advertises `generation_handoff` and can
    // be handed off to a newer generation instead of being killed (senpi multi-session-host.ts:268):
    // the capability IS the proof a handler exists, and a host without it is never signalled.
    let handoff_public_socket=public_socket.clone();
    let (drain_tx,drain_rx)=tokio::sync::watch::channel(false);
    let _drain_sender=drain_tx.clone();
    #[cfg(unix)]
    {
        // Register the handler FIRST; only a successful registration may advertise the capability.
        let mut user1=tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())?;
        core.enable_handoff();
        let drain_tx=drain_tx.clone();
        tokio::spawn(async move {
            loop {
                if user1.recv().await.is_none() { break; }
                if drain_tx.send(true).is_err() { break; }
            }
        });
    }
    // Armed BEFORE the listen: a supervisor death during the listen transition must still end this
    // host and clean its private endpoint (senpi multi-session-host.ts:470-490).
    let watchdog_task=tokio::spawn(crate::host_watchdog::arm_host_watchdog(watchdog,capture,move|reason|{
        eprintln!("senpi rpc host: {reason}; shutting down");
        if let Some(path)=public_socket.as_deref(){
            let identity=*callback_owner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            crate::socket_ownership::unlink_owned_socket(path,identity,if cfg!(windows){"win32"}else{std::env::consts::OS},|_|{});
        }
        let _=reason_tx.send(Some(reason));
    }));
    let listener=tokio::net::UnixListener::bind(&path)?;
    {use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o600))?;}
    let bound=crate::socket_ownership::stat_socket_identity(std::path::Path::new(&path))?;
    let connections=std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut next=0u64;
    let mut draining=false;
    let mut empty_since:Option<tokio::time::Instant>=None;
    let mut reason_rx=reason_rx;
    let mut terminate=tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hangup=tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let mut signal_code=None;
    let mut drain_rx=drain_rx;
    let mut tick=std::time::Duration::from_millis(250);
    loop{
        tokio::select!{
            biased;
            changed=reason_rx.changed()=>{if changed.is_err()||reason_rx.borrow().is_some(){break;}},
            changed=drain_rx.changed()=>{
                if changed.is_ok()&&*drain_rx.borrow()&&!draining{
                    draining=true;
                    tick=std::time::Duration::from_millis(crate::session_command_router::DRAIN_SWEEP_MS);
                    eprintln!("senpi rpc host: draining for a generation handoff");
                    let superseded=endpoint_superseded(&path,&bound,handoff_public_socket.as_deref(),&public_owner);
                    let successor=superseded.then(||crate::rpc_types::RpcHostSuccessor{socket:handoff_public_socket.clone().unwrap_or_else(||path.clone())});
                    if let Ok(value)=serde_json::to_value(crate::rpc_types::RpcHostLifecycleEvent::HostSuperseded{instance_id:crate::protocol_identity::host_instance_id().to_owned(),generation:crate::protocol_identity::host_generation(&std::env::vars().collect::<std::collections::HashMap<String,String>>()),successor}){core.broadcast_record(&value);}
                    core.begin_drain().await;
                }
            },
            _=terminate.recv()=>{signal_code=Some(143);break;},
            _=hangup.recv()=>{signal_code=Some(129);break;},
            accepted=listener.accept()=>{
                let(socket,_)=accepted?;
                if draining{drop(socket);continue;}
                next+=1;let connection=format!("socket-{next}");
                connections.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                empty_since=None;
                let core=core.clone();let counter=connections.clone();
                tokio::spawn(async move{serve_socket_connection(core,connection,socket).await;counter.fetch_sub(1,std::sync::atomic::Ordering::SeqCst);});
            }
            ()=tokio::time::sleep(tick)=>{
                if draining&&core.drain_sweep().await{break;}
                let empty=(draining||connections.load(std::sync::atomic::Ordering::SeqCst)==0)&&core.registry.lock().await.size()==0;
                match empty_exit_ms.filter(|window|window.is_finite()){
                    Some(window) if empty=>{let since=*empty_since.get_or_insert(tokio::time::Instant::now());if since.elapsed().as_secs_f64()*1000.>=window{break;}},
                    _=>empty_since=None,
                }
            }
        }
    }
    watchdog_task.abort();
    // Ownership-checked: only the entry THIS process bound is removed (senpi `unlinkOwnedSocket`).
    crate::socket_ownership::unlink_owned_socket(&path,bound,if cfg!(windows){"win32"}else{std::env::consts::OS},|_|{});
    if let Some(code)=signal_code{std::process::exit(code);}
    Ok(())
}
/// Whether the endpoint this host serves is held by another socket entry now (senpi
/// `runSocketHost`'s `endpointSuperseded`): a supervised host answers for the PUBLIC path its
/// supervisor bound, a bare host for the path it bound itself.
fn endpoint_superseded(own_path:&str,own:&Option<crate::socket_ownership::SocketFileIdentity>,public_socket:Option<&str>,public_owner:&std::sync::Mutex<Option<crate::socket_ownership::SocketFileIdentity>>)->bool{
    let platform=if cfg!(windows){"win32"}else{std::env::consts::OS};
    match public_socket{
        Some(path)=>{let identity=*public_owner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);crate::socket_ownership::socket_entry_replaced(path,identity,platform)}
        None=>crate::socket_ownership::socket_entry_replaced(own_path,*own,platform),
    }
}
/// One accepted connection's lifetime: register its writer, pump its commands, release it.
async fn serve_socket_connection(core:std::sync::Arc<HostCore>,connection:String,socket:tokio::net::UnixStream){
    let(read,write)=socket.into_split();
    let writer=std::sync::Arc::new(crate::session_event_writer::SessionWriterActor::new(write));
    core.register_connection(&connection,writer);
    let mut lines=tokio::io::BufReader::new(read);
    use tokio::io::AsyncBufReadExt;
    let mut buffer=String::new();
    let mut commands=Vec::new();
    loop{
        buffer.clear();
        let read=tokio::select!{
            result=poll_host_commands(&mut commands),if !commands.is_empty()=>{if result.is_err(){break;}continue;},
            read=lines.read_line(&mut buffer)=>read,
        };
        match read{Ok(0)|Err(_)=>break,Ok(_)=>{}}
        let line=buffer.trim_end_matches('\n').trim_end_matches('\r').to_owned();
        let core=core.clone();let tag=connection.clone();
        commands.push(Box::pin(async move{core.handle(Some(&tag),&line).await}) as HostCommand);
    }
    drop(commands);
    core.unregister_connection(&connection);
}
#[cfg(test)]mod tests{use super::*;#[test]fn explicit_policy_overrides_environment_and_bad_env_uses_defaults(){let env=HashMap::from([(RPC_SESSION_IDLE_EVICTION_MS_ENV.into(),"100".into()),(RPC_HOST_EMPTY_EXIT_MS_ENV.into(),"invalid".into())]);assert_eq!(resolve_host_idle_policy(&env,None,None),HostIdlePolicy{idle_eviction_ms:100.,empty_exit_ms:DEFAULT_HOST_EMPTY_EXIT_MS});assert_eq!(resolve_host_idle_policy(&env,Some(f64::INFINITY),Some(0.)),HostIdlePolicy{idle_eviction_ms:f64::INFINITY,empty_exit_ms:0.});}}
