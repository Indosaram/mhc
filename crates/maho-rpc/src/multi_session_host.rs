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
/// result): one registry, one binding per session, and one event fanout every connection is
/// registered with (senpi `SessionEventWriter`'s `SessionEventFanout`).
pub struct HostCore{
    pub registry:tokio::sync::Mutex<crate::session_registry::RpcSessionRegistry>,
    pub default_writer:std::sync::Arc<crate::session_event_writer::SessionWriterActor>,
    pub attachments:std::sync::Mutex<crate::session_command_router::ConnectionAttachments>,
    /// Per-connection delivery: attachment sets, capabilities, late-joiner snapshots.
    pub fanout:std::sync::Arc<std::sync::Mutex<crate::session_event_fanout::SessionEventFanout>>,
    /// Loop time the stall watchdog recorded; socket sinks extend their drain deadline by it.
    pub blocked:std::sync::Arc<std::sync::Mutex<crate::loop_blocked_time::LoopBlockedTime>>,
    widths:std::sync::Mutex<crate::session_command_router::SharedSessionWidths>,
    /// Session attribution the stall watchdog blames a blocked loop on (senpi `runWithSessionAttribution`).
    pub activity:crate::session_attribution::SessionActivityRegistry,
    /// Raised by the memory sampler: idle sessions park at half the window (senpi `setMemoryPressure`).
    memory_pressure:std::sync::atomic::AtomicBool,
    /// `set_client_info` capabilities a connection sent before it attached (senpi `pendingCapabilities`).
    pending_capabilities:std::sync::Mutex<std::collections::BTreeMap<String,Vec<String>>>,
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
    /// Set when the host runs every session in its own worker process (senpi `createHostCore` selects
    /// `WorkerSessionRegistry` when a `workerConfiguration` is passed); `None` is the in-process registry.
    workers:Option<std::sync::Arc<crate::worker_session_registry::WorkerSessionRegistry>>,
}
impl HostCore{
    pub fn new(options:HostCoreOptions,default_writer:std::sync::Arc<crate::session_event_writer::SessionWriterActor>)->Self{
        Self{registry:tokio::sync::Mutex::new(crate::session_registry::RpcSessionRegistry::default()),default_writer,attachments:Default::default(),fanout:Default::default(),blocked:Default::default(),widths:Default::default(),activity:Default::default(),memory_pressure:std::sync::atomic::AtomicBool::new(false),pending_capabilities:Default::default(),bindings:tokio::sync::Mutex::new(std::collections::BTreeMap::new()),options,handoff_enabled:std::sync::atomic::AtomicBool::new(false),drain:std::sync::Mutex::new(crate::session_command_router::HandoffDrain::default()),handoff_closed:std::sync::Mutex::new(std::collections::BTreeSet::new()),handoff_parks:std::sync::atomic::AtomicUsize::new(0),workers:None}
    }
    /// Selects the worker runtime: every opened session runs in an isolated worker process.
    pub fn with_worker_registry(mut self,workers:crate::worker_session_registry::WorkerSessionRegistry)->Self{self.workers=Some(std::sync::Arc::new(workers));self}
    pub fn uses_worker_runtime(&self)->bool{self.workers.is_some()}
    /// Sessions the host holds in whichever registry serves them.
    pub async fn session_count(&self)->usize{match &self.workers{Some(workers)=>workers.size(),None=>self.registry.lock().await.size()}}
    /// Registers one connection's sink with the fanout (senpi `SessionEventWriter.registerConnection`).
    pub fn register_connection(&self,connection:&str,actor:crate::socket_event_fanout::SocketEventSinkActor){self.fanout.lock().unwrap_or_else(std::sync::PoisonError::into_inner).register(connection,actor);}
    /// Unregisters a connection's sink, its capabilities and its render widths.
    pub fn unregister_connection(&self,connection:&str){
        self.fanout.lock().unwrap_or_else(std::sync::PoisonError::into_inner).unregister(connection);
        self.pending_capabilities.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(connection);
        self.widths.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear_connection(connection);
    }
    /// Advertises `generation_handoff` only once a SIGUSR1 handler is actually installed (senpi
    /// `runSocketHost`): the capability IS the proof a handler exists, and a host without it is
    /// never signalled - SIGUSR1 would simply terminate it, sessions and all.
    pub fn enable_handoff(&self){self.handoff_enabled.store(true,std::sync::atomic::Ordering::SeqCst);}
    /// Host capabilities first, then the launch's client capabilities, deduplicated in order
    /// (senpi `SessionCommandRouter.dispatch` `get_protocol_info`).
    fn advertised_capabilities(&self)->Vec<String>{
        use crate::custom_capability::*;
        let mut capabilities=Vec::<String>::new();
        for capability in ["multi_session",AUTO_TITLE_SESSIONS_CAPABILITY,MEDIA_PLACEHOLDERS_CAPABILITY,RETAIN_ON_DISCONNECT_CAPABILITY,SESSION_CONTEXT_CAPABILITY,SESSION_KIND_CAPABILITY,AUTO_TITLE_PER_SESSION_CAPABILITY,DURABLE_SESSION_ID_CAPABILITY].into_iter().chain(self.options.capabilities.iter().map(String::as_str)){
            if !capabilities.iter().any(|existing|existing==capability){capabilities.push(capability.to_owned());}
        }
        if self.handoff_enabled.load(std::sync::atomic::Ordering::SeqCst)&&!capabilities.iter().any(|capability|capability==crate::host_decision::GENERATION_HANDOFF_CAPABILITY){capabilities.push(crate::host_decision::GENERATION_HANDOFF_CAPABILITY.into());}
        capabilities
    }
    /// Broadcasts one host-lifecycle record to every registered connection (senpi
    /// `SessionEventWriter.broadcastHostRecord`); a host with no connection answers on its default writer.
    pub fn broadcast_record(&self,record:&serde_json::Value){
        let stdio=self.fanout.lock().unwrap_or_else(std::sync::PoisonError::into_inner).broadcast_host_record(record);
        match stdio{Ok(Some(record))=>{let _=self.default_writer.enqueue("",record);},Ok(None)=>{},Err(error)=>eprintln!("senpi rpc host record failed: {error}"),}
    }
    /// Routes one session record through the fanout (senpi `SessionEventWriter.enqueue`).
    fn route(&self,session_id:&str,owner:Option<&str>,record:serde_json::Value){route_session_record(&self.fanout,&self.default_writer,session_id,owner,record);}
    /// Drops every per-connection record of a handle the host ended (senpi `forgetSessionOwnership`).
    fn forget_session(&self,session_id:&str){
        self.fanout.lock().unwrap_or_else(std::sync::PoisonError::into_inner).forget_session(session_id);
        self.attachments.lock().unwrap_or_else(std::sync::PoisonError::into_inner).forget_session(session_id);
        self.widths.lock().unwrap_or_else(std::sync::PoisonError::into_inner).forget_session(session_id);
        self.default_writer.forget_session(session_id);
    }
    /// Starts the drain this host parks itself with (senpi `SessionCommandRouter.beginDrain`).
    pub async fn begin_drain(self:&std::sync::Arc<Self>){self.drain.lock().unwrap_or_else(std::sync::PoisonError::into_inner).begin();let _=self.drain_sweep().await;}
    /// One drain pass (senpi `sweepDrain`): park every open session whose work has settled, then
    /// report whether the host holds nothing and the generation may leave. A busy accepted open
    /// blocks the pass exactly as senpi's `activeRequests.has(undefined)` guard does.
    pub async fn drain_sweep(self:&std::sync::Arc<Self>)->bool{
        if !self.drain.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_draining(){return false;}
        let(active_open,open)={let rows=self.list_rows(true).await;
            let active_open=rows.iter().any(|row|row["status"]=="opening");
            let open=rows.iter().filter(|row|row["status"]=="open").filter_map(|row|row["sessionId"].as_str().map(str::to_owned)).collect::<Vec<_>>();
            (active_open,open)};
        if !active_open{
            for handle in &open{
                if self.handoff_closed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(handle){continue;}
                let busy=match &self.workers{Some(workers)=>workers.worker(handle).is_some_and(|worker|worker.handoff_busy()),None=>{let registry=self.registry.lock().await;registry.get(handle).and_then(|entry|entry.runtime.as_ref()).is_some_and(|runtime|crate::handoff_activity::is_handoff_busy(runtime.session().activity_snapshot()))}};
                if busy{continue;}
                self.handoff_closed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(handle.clone());
                self.handoff_parks.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                let core=std::sync::Arc::clone(self);
                let handle=handle.clone();
                tokio::spawn(async move{
                    core.park_session(&handle).await;
                    core.handoff_parks.fetch_sub(1,std::sync::atomic::Ordering::SeqCst);
                    if core.has_session(&handle).await{core.handoff_closed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&handle);}
                });
            }
        }
        let size=self.session_count().await;
        let parks=self.handoff_parks.load(std::sync::atomic::Ordering::SeqCst);
        let step={self.drain.lock().unwrap_or_else(std::sync::PoisonError::into_inner).step(active_open,size,parks)};
        matches!(step,crate::session_command_router::DrainStep::Exit)
    }
    /// Parks one settled session for a handoff (senpi `evictIdleSession(sessionId,"handoff_parked")`):
    /// the routing handle ends exactly as a close ends it, the terminal record names the reason (and
    /// the file a retained session reopens by) so a client is not told the session vanished, and
    /// every attachment is drained because a drain ends the shared session.
    async fn park_session(&self,session_id:&str){
        if let Some(workers)=&self.workers{
            let Some(retained)=workers.force_close(session_id)else{return;};
            let _=workers.close_marked(session_id).await;
            if let Ok(value)=serde_json::to_value(crate::rpc_types::RpcSessionLifecycleEvent::SessionClosed{session_id:session_id.into(),reason:Some("handoff_parked".into()),session_path:retained}){self.route(session_id,None,value);}
            self.forget_session(session_id);
            return;
        }
        let retained={let mut registry=self.registry.lock().await;
            let Some(entry)=registry.get_mut(session_id)else{return;};
            if entry.close.state!=crate::session_registry::RpcSessionState::Open{return;}
            let retained=if entry.close.retain_on_disconnect{entry.session_path.clone()}else{None};
            entry.close.attachments=0;entry.close.state=crate::session_registry::RpcSessionState::Closing;
            retained};
        if let Some(binding)=self.bindings.lock().await.remove(session_id){binding.cancel_pending_extension_ui_requests();binding.dispose().await;}
        {let mut registry=self.registry.lock().await;let _=registry.close_marked(session_id,self.options.close_grace_ms).await;}
        if let Ok(value)=serde_json::to_value(crate::rpc_types::RpcSessionLifecycleEvent::SessionClosed{session_id:session_id.into(),reason:Some("handoff_parked".into()),session_path:retained}){self.route(session_id,None,value);}
        self.forget_session(session_id);
    }
    /// Releases the RETAINED sessions a dropped connection still owned (senpi `releaseConnection`
    /// -> `releaseOwnedSession` for `retainOnDisconnect` entries): a retained session is never closed
    /// by a drop, so each attachment is released at once, and the release that leaves the session
    /// with no attachment parks it and tells its extensions `session_parked`.
    pub async fn release_retained_attachments(&self,connection:&str){
        let owned=self.attachments.lock().unwrap_or_else(std::sync::PoisonError::into_inner).owned_by(connection);
        if let Some(workers)=&self.workers{
            for(session_id,count)in owned{
                for _ in 0..count{
                    let retained=workers.with_entry(&session_id,|entry|entry.close.state==crate::session_registry::RpcSessionState::Open&&entry.close.retain_on_disconnect).unwrap_or(false);
                    if !retained||workers.mark_close(&session_id,true).is_err(){break;}
                    self.attachments.lock().unwrap_or_else(std::sync::PoisonError::into_inner).release(connection,&session_id);
                }
            }
            return;
        }
        for(session_id,count)in owned{
            for _ in 0..count{
                let parked={let mut registry=self.registry.lock().await;
                    let Some(entry)=registry.get(&session_id)else{break;};
                    if entry.close.state!=crate::session_registry::RpcSessionState::Open||!entry.close.retain_on_disconnect{break;}
                    let parks=entry.close.attachments==1;
                    let parked=if parks{entry.runtime.clone()}else{None};
                    // A failed claim means the entry is already closed or owned by another path.
                    if registry.mark_close(&session_id,true).is_err(){break;}
                    parked};
                self.attachments.lock().unwrap_or_else(std::sync::PoisonError::into_inner).release(connection,&session_id);
                if let Some(runtime)=parked{runtime.emit_attachment_event(maho_core::agent_session_runtime::AttachmentEvent::SessionParked).await;}
            }
        }
    }
    fn respond(&self,connection:Option<&str>,response:&crate::rpc_types::RpcResponse){
        let Ok(value)=serde_json::to_value(response)else{return;};
        if let Some(connection)=connection{
            match self.fanout.lock().unwrap_or_else(std::sync::PoisonError::into_inner).enqueue_control(connection,&value){Ok(true)=>return,Ok(false)=>{},Err(error)=>{eprintln!("senpi rpc connection {connection} failed: {error}");return;}}
        }
        let _=self.default_writer.enqueue(response.session_id.as_deref().unwrap_or(""),value);
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
                // senpi `process.argv.slice(2)`: every CLI argument after the executable.
                let argv=std::env::args().skip(1).collect::<Vec<_>>();
                let mut data=crate::protocol_identity::host_launch_profile(&argv,&self.options.cwd).ok().map_or_else(||serde_json::json!({}),|profile|crate::protocol_identity::protocol_identity(profile,&std::env::vars().collect()));
                data["protocolVersion"]=crate::host_decision::HOST_PROTOCOL_VERSION.into();
                data["serverVersion"]=maho_core::engine_build_identity::engine_build_identity().text.clone().into();
                data["capabilities"]=serde_json::json!(self.advertised_capabilities());
                data["mode"]="multi".into();
                self.success(connection,command.id.clone(),"get_protocol_info",Some(data),None);
            },
            crate::rpc_types::RpcCommandBody::SetClientInfo{width,capabilities}=>{
                // Session-less: the connection's capabilities (senpi router `set_client_info`); with a
                // handle, also this connection's render width for that session (senpi `sharedWidth`).
                if let Some(connection)=connection{
                    let capabilities=capabilities.clone().unwrap_or_default();
                    self.pending_capabilities.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(connection.to_owned(),capabilities.clone());
                    if let Err(error)=self.fanout.lock().unwrap_or_else(std::sync::PoisonError::into_inner).set_capabilities(connection,&capabilities){eprintln!("senpi rpc connection {connection} failed: {error}");}
                    if let Some(session_id)=&command.session_id{self.widths.lock().unwrap_or_else(std::sync::PoisonError::into_inner).set_width(session_id,Some(connection),*width);}
                }
                self.success(connection,command.id.clone(),"set_client_info",None,command.session_id.clone());
            },
            crate::rpc_types::RpcCommandBody::ListSessions{include_workers}=>{
                let sessions=self.list_rows(include_workers.unwrap_or(false)).await;
                self.success(connection,command.id.clone(),"list_sessions",Some(serde_json::json!({"sessions":sessions})),None);
            },
            crate::rpc_types::RpcCommandBody::OpenSession{session_path,cwd,provider,model_id,thinking_level,permission_preset,retain_on_disconnect,kind,context,auto_title,durable_session_id}=>{
                let profile=crate::session_registry::RpcSessionLaunchProfile{
                    runtime:maho_core::agent_session_runtime::AgentSessionLaunchProfile{cwd:cwd.clone().unwrap_or_else(||self.options.cwd.clone()),permission_preset:permission_preset.clone(),creation_model:provider.clone().zip(model_id.clone()),initial_thinking_level:thinking_level.clone(),auto_title:*auto_title},
                    session_path:session_path.clone(),durable_session_id:durable_session_id.clone(),
                    session_kind:kind.map(|kind|match kind{crate::rpc_types::SessionKind::Worker=>maho_ext_api::SessionKind::Worker,crate::rpc_types::SessionKind::Interactive=>maho_ext_api::SessionKind::Interactive}),
                    session_context:context.clone(),
                };
                if let Some(workers)=self.workers.clone(){self.open_worker_session(&workers,connection,command.id.clone(),profile,retain_on_disconnect.unwrap_or(false)).await;return Ok(());}
                let admitted={let mut registry=self.registry.lock().await;registry.admit_open(profile.clone(),retain_on_disconnect.unwrap_or(false),crate::connection_handler::now_ms())};
                match admitted{
                    Err(error)=>self.error(connection,command.id.clone(),"open_session",error.to_string(),None),
                    Ok(crate::session_registry::OpenAdmission::Attached(handle))=>{
                        // senpi session-registry.ts: an attach that ends a retained session's parked
                        // (zero-attachment) window tells its extensions `session_resumed`.
                        let resumed={let registry=self.registry.lock().await;registry.get(&handle).filter(|entry|entry.close.retain_on_disconnect&&entry.close.attachments==1).and_then(|entry|entry.runtime.clone())};
                        if let Some(runtime)=resumed{runtime.emit_attachment_event(maho_core::agent_session_runtime::AttachmentEvent::SessionResumed).await;}
                        // A second client joins the live session: register it with the fanout so it
                        // receives the session's records and the in-flight snapshot (senpi `open`).
                        self.attach_connection(connection,&handle).await;
                        let state=self.session_state(&handle).await;
                        self.success(connection,command.id.clone(),"open_session",Some(serde_json::json!({"sessionId":handle,"state":state,"attached":true})),None);
                    },
                    Ok(crate::session_registry::OpenAdmission::Create(handle))=>{
                        let created={let mut registry=self.registry.lock().await;registry.create_admitted_runtime(&handle,(self.options.create_runtime)(profile)).await};
                        match created{
                            Err(error)=>self.error(connection,command.id.clone(),"open_session",error.to_string(),None),
                            Ok(())=>{
                                let state=self.session_state(&handle).await;
                                self.attach_connection(connection,&handle).await;
                                self.success(connection,command.id.clone(),"open_session",Some(serde_json::json!({"sessionId":handle,"state":state})),None);
                            }
                        }
                    }
                }
            },
            crate::rpc_types::RpcCommandBody::CloseSession=>{
                let Some(session_id)=command.session_id.clone().or(self.only_session().await)else{self.error(connection,command.id.clone(),"close_session",crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION,None);return Ok(());};
                if let Some(workers)=self.workers.clone(){self.close_worker_session(&workers,connection,command.id.clone(),session_id).await;return Ok(());}
                let admitted={let mut registry=self.registry.lock().await;let mut attachments=self.attachments.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    match registry.get_mut(&session_id){Some(entry)=>attachments.admit_close((connection,&session_id),&serde_json::json!({}),&mut entry.close,false,&self.default_writer),None=>Err(crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION.to_owned())}};
                match admitted{
                    Err(error)=>self.error(connection,command.id.clone(),"close_session",error,Some(session_id)),
                    Ok(None)=>{},
                    Ok(Some((reservation,claim)))=>{
                        self.default_writer.release_close_response(reservation);
                        if let Some(connection)=connection{self.release_owner_attachment(connection,&session_id);}
                        // Only the LAST attachment tears the session down; a surviving attachment keeps
                        // the shared binding and its event stream (senpi `close`/`claimClose`).
                        if claim.finalizer!=Some(true){self.success(connection,command.id.clone(),"close_session",None,Some(session_id));return Ok(());}
                        if let Some(binding)=self.bindings.lock().await.remove(&session_id){binding.dispose().await;}
                        let completion=self.registry.lock().await.close_marked(&session_id,self.options.close_grace_ms).await;
                        let result=match completion{
                            Err(error)=>Err(error.to_string()),
                            Ok(Some(crate::session_teardown::RuntimeCloseCompletion::Settled(result)))=>result.map_err(|error|error.to_string()).and_then(|result|result.map_err(|error|error.to_string())),
                            Ok(Some(crate::session_teardown::RuntimeCloseCompletion::Draining(task)))=>task.await.map_err(|error|error.to_string()).and_then(|result|result.map_err(|error|error.to_string())),
                            Ok(None)=>Ok(()),
                        };
                        // The terminal record precedes the close answer, and follows the lifecycle
                        // visibility rule (senpi `reply.complete(true,"client_close")`).
                        if let Ok(value)=serde_json::to_value(crate::rpc_types::RpcSessionLifecycleEvent::SessionClosed{session_id:session_id.clone(),reason:Some("client_close".into()),session_path:None}){self.route(&session_id,None,value);}
                        self.forget_session(&session_id);
                        match result{Ok(())=>self.success(connection,command.id.clone(),"close_session",None,Some(session_id)),Err(error)=>self.error(connection,command.id.clone(),"close_session",error,Some(session_id))}}
                }
            },
            _=>{
                let Some(session_id)=command.session_id.clone()else{self.error(connection,command.id.clone(),value["type"].as_str().unwrap_or_default(),crate::rpc_types::RPC_ERROR_MISSING_SESSION_ID,None);return Ok(());};
                if let Some(workers)=&self.workers{
                    let kind=value["type"].as_str().unwrap_or_default().to_owned();
                    match workers.get_for_command(&session_id,&kind,crate::connection_handler::now_ms()){
                        Err(error)=>self.error(connection,command.id.clone(),&kind,error.to_string(),Some(session_id)),
                        Ok(worker)=>{let result=crate::session_attribution::run_with_session_attribution(&self.activity,crate::session_attribution::SessionAttribution{session_id:Some(session_id.clone()),tool:None},worker.command(value.clone(),connection.map(str::to_owned))).await;if let Err(error)=result{self.error(connection,command.id.clone(),&kind,error,Some(session_id));}}
                    }
                    return Ok(());
                }
                // A routed command restarts the session's idle window (senpi `registry.getForCommand`).
                if let Some(entry)=self.registry.lock().await.get_mut(&session_id){entry.last_command_at=crate::connection_handler::now_ms();}
                let binding=self.bindings.lock().await.get(&session_id).cloned();
                // Every routed command runs inside its session's attribution scope (senpi router
                // `handle`) and its connection's scope, so a response reaches only its requester.
                match binding{Some(binding)=>crate::session_attribution::run_with_session_attribution(&self.activity,crate::session_attribution::SessionAttribution{session_id:Some(session_id.clone()),tool:None},CURRENT_CONNECTION.scope(connection.map(str::to_owned),binding.handle(&value))).await?,None=>self.error(connection,command.id.clone(),value["type"].as_str().unwrap_or_default(),crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION,Some(session_id))}
            }
        }
        Ok(())
    }
    async fn session_state(&self,handle:&str)->serde_json::Value{
        let registry=self.registry.lock().await;
        registry.get(handle).and_then(|entry|entry.runtime.as_ref()).map_or_else(||serde_json::json!({}),|runtime|crate::connection_handler::build_rpc_session_state(runtime.session(),None))
    }
    async fn only_session(&self)->Option<String>{if self.session_count().await==1{self.list_rows(true).await.first().and_then(|row|row["sessionId"].as_str().map(str::to_owned))}else{None}}
    async fn list_rows(&self,include_workers:bool)->Vec<serde_json::Value>{match &self.workers{Some(workers)=>workers.list_sessions(include_workers),None=>self.registry.lock().await.list_sessions(include_workers)}}
    async fn has_session(&self,handle:&str)->bool{match &self.workers{Some(workers)=>workers.worker(handle).is_some(),None=>self.registry.lock().await.get(handle).is_some()}}
    /// The routes a worker's output and lifecycle take back into this host's fanout.
    fn worker_routes(&self,workers:&std::sync::Arc<crate::worker_session_registry::WorkerSessionRegistry>,handle:&str)->crate::worker_session_registry::WorkerRoutes{
        let(fanout,writer)=(std::sync::Arc::clone(&self.fanout),std::sync::Arc::clone(&self.default_writer));
        let(failure_fanout,failure_writer)=(std::sync::Arc::clone(&self.fanout),std::sync::Arc::clone(&self.default_writer));
        let(terminal_fanout,terminal_writer)=(std::sync::Arc::clone(&self.fanout),std::sync::Arc::clone(&self.default_writer));
        let(close_fanout,close_writer)=(std::sync::Arc::clone(&self.fanout),std::sync::Arc::clone(&self.default_writer));
        let(tag,failure_tag,terminal_tag,close_tag)=(handle.to_owned(),handle.to_owned(),handle.to_owned(),handle.to_owned());
        let registry=std::sync::Arc::downgrade(workers);
        crate::worker_session_registry::WorkerRoutes{
            output:Box::new(move|record,owner|route_session_record(&fanout,&writer,&tag,owner.as_deref(),record)),
            failure:Box::new(move|error|route_session_record(&failure_fanout,&failure_writer,&failure_tag,None,serde_json::json!({"type":"session_error","error":error}))),
            terminal_failure:Box::new(move|error|{
                if let Ok(value)=serde_json::to_value(crate::rpc_types::RpcSessionLifecycleEvent::SessionClosed{session_id:terminal_tag.clone(),reason:Some("error".into()),session_path:None}){route_session_record(&terminal_fanout,&terminal_writer,&terminal_tag,None,value);}
                eprintln!("senpi rpc session {terminal_tag} closed: {error}");
                terminal_fanout.lock().unwrap_or_else(std::sync::PoisonError::into_inner).forget_session(&terminal_tag);terminal_writer.forget_session(&terminal_tag);
            }),
            // senpi `requestClose` = `router.close({ type: "close_session" })` for the worker's own shutdown.
            request_close:Box::new(move||{
                let Some(registry)=registry.upgrade()else{return};
                if registry.force_close(&close_tag).is_none(){return;}
                let(fanout,writer,tag)=(std::sync::Arc::clone(&close_fanout),std::sync::Arc::clone(&close_writer),close_tag.clone());
                tokio::spawn(async move{
                    let _=registry.close_marked(&tag).await;
                    if let Ok(value)=serde_json::to_value(crate::rpc_types::RpcSessionLifecycleEvent::SessionClosed{session_id:tag.clone(),reason:Some("client_close".into()),session_path:None}){route_session_record(&fanout,&writer,&tag,None,value);}
                    fanout.lock().unwrap_or_else(std::sync::PoisonError::into_inner).forget_session(&tag);writer.forget_session(&tag);
                });
            }),
        }
    }
    /// `open_session` on the worker runtime (senpi `WorkerSessionRegistry.openSession` + the router's
    /// `open`): a new handle spawns and binds its own worker; a known path attaches to the live one.
    async fn open_worker_session(&self,workers:&std::sync::Arc<crate::worker_session_registry::WorkerSessionRegistry>,connection:Option<&str>,id:Option<String>,profile:crate::session_registry::RpcSessionLaunchProfile,retain:bool){
        let opened=workers.open_session(profile,retain,crate::connection_handler::now_ms(),|handle|self.worker_routes(workers,handle)).await;
        let(handle,attached)=match opened{Err(error)=>{self.error(connection,id,"open_session",error.to_string(),None);return;},Ok(crate::worker_session_registry::WorkerOpen::Created(handle))=>(handle,false),Ok(crate::worker_session_registry::WorkerOpen::Attached(handle))=>(handle,true)};
        let Some(worker)=workers.worker(&handle)else{self.error(connection,id,"open_session",crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION,None);return;};
        let kind=workers.with_entry(&handle,|entry|entry.identity.kind).unwrap_or_default();
        let capabilities=connection.and_then(|connection|self.pending_capabilities.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(connection).cloned()).unwrap_or_else(||self.options.capabilities.clone());
        {let mut fanout=self.fanout.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            fanout.set_session_kind(&handle,kind);
            if let Some(connection)=connection{
                if fanout.targets.capabilities(connection).is_none()&&let Err(error)=fanout.set_capabilities(connection,&capabilities){eprintln!("senpi rpc connection {connection} failed: {error}");}
                if let Err(error)=fanout.attach(connection,&handle,crate::connection_handler::now_ms()){eprintln!("senpi rpc connection {connection} failed: {error}");}
            }}
        if let Some(connection)=connection{self.attachments.lock().unwrap_or_else(std::sync::PoisonError::into_inner).attach(connection,&handle);}
        if !attached&&let Err(error)=worker.bind(&handle,capabilities,connection.map(str::to_owned)).await{
            if let Some(connection)=connection{self.release_owner_attachment(connection,&handle);}
            if workers.force_close(&handle).is_some(){let _=workers.close_marked(&handle).await;}
            self.forget_session(&handle);
            self.error(connection,id,"open_session",format!("open_failed: {error}"),None);return;
        }
        let state=worker.snapshot().map_or_else(||serde_json::json!({}),|snapshot|snapshot.state);
        let mut data=serde_json::json!({"sessionId":handle,"state":state});if attached{data["attached"]=true.into();}
        self.success(connection,id,"open_session",Some(data),None);
    }
    /// `close_session` on the worker runtime: only the LAST attachment ends the worker, whose actual
    /// exit releases its reservations (senpi `WorkerSessionRegistry.beginClose` + `closeMarked`).
    async fn close_worker_session(&self,workers:&std::sync::Arc<crate::worker_session_registry::WorkerSessionRegistry>,connection:Option<&str>,id:Option<String>,session_id:String){
        if !self.attachments.lock().unwrap_or_else(std::sync::PoisonError::into_inner).owns(connection,&session_id){self.error(connection,id,"close_session",crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION,Some(session_id));return;}
        let claim=match workers.mark_close(&session_id,false){Ok(claim)=>claim,Err(error)=>{self.error(connection,id,"close_session",error.to_string(),Some(session_id));return;}};
        if let Some(connection)=connection{self.attachments.lock().unwrap_or_else(std::sync::PoisonError::into_inner).release(connection,&session_id);self.release_owner_attachment(connection,&session_id);}
        if claim.finalizer!=Some(true){self.success(connection,id,"close_session",None,Some(session_id));return;}
        if let Some(worker)=workers.worker(&session_id){worker.dispose();}
        let result=workers.close_marked(&session_id).await;
        if let Ok(value)=serde_json::to_value(crate::rpc_types::RpcSessionLifecycleEvent::SessionClosed{session_id:session_id.clone(),reason:Some("client_close".into()),session_path:None}){self.route(&session_id,None,value);}
        self.forget_session(&session_id);
        match result{Ok(())=>self.success(connection,id,"close_session",None,Some(session_id)),Err(error)=>self.error(connection,id,"close_session",error.to_string(),Some(session_id))}
    }
    /// Attaches one opening connection to a session (senpi `open`): ownership, the fanout target
    /// with its capabilities and late-joiner replay, the session's visibility class, and the shared
    /// binding the first opener creates. A stdio open owns no connection and keeps no binding.
    async fn attach_connection(&self,connection:Option<&str>,session_id:&str){
        let(runtime,kind)={let registry=self.registry.lock().await;match registry.get(session_id){Some(entry)=>(entry.runtime.as_ref().map(std::sync::Arc::clone),entry.identity.kind),None=>return}};
        let capabilities=connection.and_then(|connection|self.pending_capabilities.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(connection).cloned()).unwrap_or_else(||self.options.capabilities.clone());
        {let mut fanout=self.fanout.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            fanout.set_session_kind(session_id,kind);
            if let Some(connection)=connection{
                if fanout.targets.capabilities(connection).is_none()&&let Err(error)=fanout.set_capabilities(connection,&capabilities){eprintln!("senpi rpc connection {connection} failed: {error}");}
                if let Err(error)=fanout.attach(connection,session_id,crate::connection_handler::now_ms()){eprintln!("senpi rpc connection {connection} failed: {error}");}
            }}
        let Some(connection)=connection else{return;};
        self.attachments.lock().unwrap_or_else(std::sync::PoisonError::into_inner).attach(connection,session_id);
        let Some(runtime)=runtime else{return;};
        let mut bindings=self.bindings.lock().await;
        if bindings.contains_key(session_id){return;}
        let fanout=std::sync::Arc::clone(&self.fanout);let writer=std::sync::Arc::clone(&self.default_writer);let tag=session_id.to_owned();
        let sink:std::sync::Arc<dyn Fn(serde_json::Value)+Send+Sync>=std::sync::Arc::new(move|value:serde_json::Value|{let owner=CURRENT_CONNECTION.try_with(Clone::clone).ok().flatten();route_session_record(&fanout,&writer,&tag,owner.as_deref(),value);});
        let binding=std::sync::Arc::new(crate::session_binding::create_rpc_session_binding(session_id.to_owned(),runtime.session().clone(),sink,capabilities).await);
        bindings.insert(session_id.to_owned(),binding);
    }
    /// Releases every session a dropped connection still owned (senpi `releaseConnection`). Each
    /// attachment goes through the refcounted detach a close uses, so a session another connection
    /// still holds keeps running, a retained one stays open at zero attachments, and only the last
    /// owner tears it down. A turn in flight runs to settlement first; a drain owns teardown itself.
    pub async fn release_connection(self:&std::sync::Arc<Self>,connection:&str){
        let owned=self.attachments.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take(connection);
        if self.drain.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_draining(){return;}
        if let Some(workers)=self.workers.clone(){
            for(session_id,count)in owned{
                for _ in 0..count{
                    let retained=workers.with_entry(&session_id,|entry|entry.close.retain_on_disconnect).unwrap_or(false);
                    // A turn in flight runs to settlement first (the worker reports `settled` snapshots).
                    if !retained&&let Some(worker)=workers.worker(&session_id).filter(|worker|worker.busy()){
                        let mut settled=worker.settled();let core=std::sync::Arc::clone(self);let session_id=session_id.clone();
                        tokio::spawn(async move{while worker.busy()&&!worker.is_stopped(){if settled.changed().await.is_err(){break;}}core.release_owned_session(&session_id).await;});
                        continue;
                    }
                    self.release_owned_session(&session_id).await;
                }
            }
            return;
        }
        for(session_id,count)in owned{
            for _ in 0..count{
                let(retained,session)={let registry=self.registry.lock().await;match registry.get(&session_id){
                    Some(entry)=>(entry.close.retain_on_disconnect,entry.runtime.as_ref().filter(|_|entry.close.state==crate::session_registry::RpcSessionState::Open).map(|runtime|runtime.session().clone())),
                    None=>(false,None)}};
                if !retained&&let Some(session)=session.filter(maho_core::agent_session::AgentSession::is_streaming){
                    let(settled_tx,settled_rx)=tokio::sync::oneshot::channel::<()>();
                    let settled=std::sync::Arc::new(std::sync::Mutex::new(Some(settled_tx)));
                    let notify=std::sync::Arc::clone(&settled);
                    let subscription=session.subscribe(std::sync::Arc::new(move|event:&maho_ext_api::AgentSessionEvent|{if matches!(event,maho_ext_api::AgentSessionEvent::AgentSettled|maho_ext_api::AgentSessionEvent::AgentIdle)&&let Some(sender)=notify.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take(){let _=sender.send(());}}));
                    // The turn may have settled between the check and the subscription.
                    if !session.is_streaming()&&let Some(sender)=settled.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take(){let _=sender.send(());}
                    let core=std::sync::Arc::clone(self);let session_id=session_id.clone();
                    tokio::spawn(async move{let _=settled_rx.await;drop(subscription);core.release_owned_session(&session_id).await;});
                    continue;
                }
                self.release_owned_session(&session_id).await;
            }
        }
    }
    /// One owned handle's refcounted detach (senpi `releaseOwnedSession`): tolerant of a lost race,
    /// and only the LAST attachment disposes the binding and closes the runtime.
    async fn release_owned_session(&self,session_id:&str){
        if let Some(workers)=&self.workers{
            let Ok(claim)=workers.mark_close(session_id,true)else{return;};
            if claim.finalizer!=Some(true){return;}
            if let Some(worker)=workers.worker(session_id){worker.cancel_pending_extension_ui_requests();}
            if let Err(error)=workers.close_marked(session_id).await{eprintln!("senpi rpc close for session {session_id} failed: {error}");}
            self.forget_session(session_id);
            return;
        }
        let claim={let mut registry=self.registry.lock().await;registry.mark_close(session_id,true)};
        let Ok(claim)=claim else{return;};
        if claim.finalizer!=Some(true){return;}
        if let Some(binding)=self.bindings.lock().await.remove(session_id){binding.cancel_pending_extension_ui_requests();binding.dispose().await;}
        let closed={let mut registry=self.registry.lock().await;registry.close_marked(session_id,self.options.close_grace_ms).await};
        if let Err(error)=closed{eprintln!("senpi rpc close for session {session_id} failed: {error}");}
        self.forget_session(session_id);
    }
    /// Idle eviction (senpi `evictIdleSession`): drains every attachment because eviction ends the
    /// shared session. A RETAINED session with a file is PARKED - `session_parked` names the path it
    /// reopens by - and anything else ends as `session_closed { reason: "idle_evicted" }`.
    async fn evict_idle_session(&self,session_id:&str){
        if let Some(workers)=&self.workers{
            let Some(parked)=workers.force_close(session_id)else{return;};
            if let Err(error)=workers.close_marked(session_id).await{eprintln!("senpi rpc close for session {session_id} failed: {error}");}
            self.publish_eviction(session_id,parked);
            return;
        }
        let parked={let mut registry=self.registry.lock().await;
            let Some(entry)=registry.get_mut(session_id)else{return;};
            if entry.close.state!=crate::session_registry::RpcSessionState::Open{return;}
            let parked=if entry.close.retain_on_disconnect{entry.session_path.clone()}else{None};
            entry.close.attachments=0;entry.close.state=crate::session_registry::RpcSessionState::Closing;
            parked};
        if let Some(binding)=self.bindings.lock().await.remove(session_id){binding.cancel_pending_extension_ui_requests();binding.dispose().await;}
        let closed={let mut registry=self.registry.lock().await;registry.close_marked(session_id,self.options.close_grace_ms).await};
        if let Err(error)=closed{eprintln!("senpi rpc close for session {session_id} failed: {error}");}
        self.publish_eviction(session_id,parked);
    }
    /// The eviction's terminal record: a retained file parks, anything else closes as `idle_evicted`.
    fn publish_eviction(&self,session_id:&str,parked:Option<String>){
        let record=match parked{
            Some(session_path)=>serde_json::to_value(crate::rpc_types::RpcSessionLifecycleEvent::SessionParked{session_id:session_id.into(),session_path}),
            None=>serde_json::to_value(crate::rpc_types::RpcSessionLifecycleEvent::SessionClosed{session_id:session_id.into(),reason:Some("idle_evicted".into()),session_path:None}),
        };
        if let Ok(value)=record{self.route(session_id,None,value);}
        self.forget_session(session_id);
    }
    /// The occupancy sweep (senpi `sweepIdleSessions` on the router's interval): evicts open
    /// sessions idle past the window (halved under memory pressure; busy work restarts the clock),
    /// and answers `true` once the registry stayed empty for the empty-exit window with the exit
    /// permitted. `false` means the host stopped it.
    pub async fn run_idle_policy(self:&std::sync::Arc<Self>,policy:HostIdlePolicy,can_exit:impl Fn()->bool,mut stopped:tokio::sync::watch::Receiver<bool>)->bool{
        let mut idle=crate::session_command_router::SessionIdlePolicy::new(policy.idle_eviction_ms,policy.empty_exit_ms);
        if !policy.idle_eviction_ms.is_finite()&&!policy.empty_exit_ms.is_finite(){let _=stopped.wait_for(|stop|*stop).await;return false;}
        let period=std::time::Duration::from_secs_f64((policy.idle_eviction_ms.min(policy.empty_exit_ms)/4.).clamp(20.,5000.)/1000.);
        let mut timer=tokio::time::interval_at(tokio::time::Instant::now()+period,period);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop{
            if *stopped.borrow(){return false;}
            tokio::select!{
                changed=stopped.changed()=>{if changed.is_err(){return false;}},
                _=timer.tick()=>{
                    idle.memory_pressure=self.memory_pressure.load(std::sync::atomic::Ordering::SeqCst);
                    let now=crate::connection_handler::now_ms();
                    let(mut sessions,size)=if let Some(workers)=&self.workers{
                        let sessions=workers.list_sessions(true).iter().filter_map(|row|{let id=row["sessionId"].as_str()?;workers.with_entry(id,|entry|crate::session_command_router::IdleSession{session_id:id.to_owned(),open:entry.close.state==crate::session_registry::RpcSessionState::Open,busy:entry.worker.busy(),last_command_at:entry.last_command_at})}).collect::<Vec<_>>();
                        (sessions,workers.size())
                    }else{let registry=self.registry.lock().await;
                        let sessions=registry.list_sessions(true).iter().filter_map(|row|{let id=row["sessionId"].as_str()?;let entry=registry.get(id)?;Some(crate::session_command_router::IdleSession{session_id:id.to_owned(),open:entry.close.state==crate::session_registry::RpcSessionState::Open,busy:entry.runtime.as_ref().is_some_and(|runtime|runtime.session().is_session_busy()),last_command_at:entry.last_command_at})}).collect::<Vec<_>>();
                        (sessions,registry.size())};
                    let sweep=idle.sweep(now,&mut sessions,size,can_exit());
                    if let Some(workers)=&self.workers{for session in sessions.iter().filter(|session|session.busy){workers.with_entry(&session.session_id,|entry|entry.last_command_at=session.last_command_at);}}
                    else{let mut registry=self.registry.lock().await;for session in sessions.iter().filter(|session|session.busy){if let Some(entry)=registry.get_mut(&session.session_id){entry.last_command_at=session.last_command_at;}}}
                    for session_id in sweep.evict{let core=std::sync::Arc::clone(self);tokio::spawn(async move{core.evict_idle_session(&session_id).await;});}
                    if sweep.exit{return true;}
                }
            }
        }
    }
    /// Arms the host's self-observation (senpi `startHostObservers`): loop-stall records blame the
    /// attributed session, RSS pressure halves idle parking, and above the refuse watermark the
    /// registry declines NEW worker sessions until a later sample clears it.
    pub fn start_host_observers(self:&std::sync::Arc<Self>,stopped:tokio::sync::watch::Receiver<bool>)->tokio::task::JoinHandle<()>{
        let core=std::sync::Arc::clone(self);
        tokio::spawn(async move{
            let env:HashMap<String,String>=std::env::vars().collect();
            let reader=std::sync::Arc::clone(&core);let publisher=std::sync::Arc::clone(&core);
            let memory=move||{
                // Only RSS is consumed, so skip the per-process fd walk of `read_host_process_metrics`.
                let rows=crate::host_process_table::load_process_table_reader(std::env::consts::OS).and_then(|read|read());
                let rss_mb=rows.map_or_else(crate::host_process_metrics::HostProcessMetrics::default,|rows|crate::host_process_metrics::metrics_from_rows(&rows,std::process::id(),None)).rss_mb.and_then(|mb|u64::try_from(mb).ok()).unwrap_or(0);
                // A held registry lock means the host is serving sessions right now.
                let sessions=match &reader.workers{Some(workers)=>workers.size() as u64,None=>reader.registry.try_lock().map_or(1,|registry|registry.size() as u64)};
                (rss_mb*1_048_576,sessions,crate::connection_handler::now_ms() as u64)
            };
            let publish=move|sample:HostMonitorSample|match sample{
                HostMonitorSample::Lag(sample)=>{
                    if let Some(record)=sample.record{publisher.broadcast_record(&record);}
                    if let Some(log)=sample.log{eprint!("{log}");}
                },
                HostMonitorSample::Memory(sample)=>{
                    if let Some(pressure)=sample.pressure_change{publisher.memory_pressure.store(pressure,std::sync::atomic::Ordering::SeqCst);}
                    if let Some((critical,rss_mb))=sample.critical_change{
                        let core=std::sync::Arc::clone(&publisher);
                        tokio::spawn(async move{core.registry.lock().await.set_worker_admission(critical.then_some((rss_mb,crate::host_memory_sampler::HOST_MEMORY_SAMPLE_MS)));});
                    }
                    if let Some(rss_mb)=sample.idle_pressure{eprintln!("senpi rpc host: memory pressure with no sessions: rssMb={rss_mb}");}
                    if let Some(record)=sample.record{publisher.broadcast_record(&record);}
                    if let Some(log)=sample.log{eprint!("{log}");}
                },
            };
            run_host_monitors(&env,&core.activity,&core.blocked,memory,crate::connection_handler::now_ms,publish,stopped).await;
        })
    }
    /// Detaches the closing connection's UI/width state (senpi `releaseOwnerAttachment`); the
    /// ownership count itself was released by the close claim.
    fn release_owner_attachment(&self,owner:&str,session_id:&str){
        self.widths.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear_width(session_id,Some(owner));
        self.fanout.lock().unwrap_or_else(std::sync::PoisonError::into_inner).targets.detach(owner,session_id);
    }
}
tokio::task_local!{static CURRENT_CONNECTION:Option<String>;}
/// Routes one session record (senpi `SessionEventWriter.enqueue`): responses, bash updates and
/// blocking UI requests go to their requester only; content goes to the connections attached to
/// the session and is remembered for late joiners; lifecycle records follow their visibility
/// rule. With no registered connection the record takes the shared stdio lane.
fn route_session_record(fanout:&std::sync::Mutex<crate::session_event_fanout::SessionEventFanout>,writer:&crate::session_event_writer::SessionWriterActor,session_id:&str,owner:Option<&str>,record:serde_json::Value){
    let targeted=matches!(record["type"].as_str(),Some("response"|"bash_execution_update"))||(record["type"]=="extension_ui_request"&&matches!(record["method"].as_str(),Some("select"|"confirm"|"input"|"editor")));
    let delivered=fanout.lock().unwrap_or_else(std::sync::PoisonError::into_inner).deliver(session_id,owner,targeted,&record);
    match delivered{
        Ok(stdio)=>for record in stdio{if let Err(error)=writer.enqueue(session_id,record){eprintln!("senpi rpc session {session_id} output failed: {error}");}},
        Err(error)=>eprintln!("senpi rpc session {session_id} output failed: {error}"),
    }
}
#[cfg(test)]mod handoff_tests{use super::*;
    fn idle_core()->std::sync::Arc<HostCore>{
        let create_runtime:HostRuntimeFactory=std::sync::Arc::new(|_profile:crate::session_registry::RpcSessionLaunchProfile|->std::pin::Pin<Box<dyn std::future::Future<Output=Result<maho_core::agent_session_runtime::AgentSessionRuntime,String>>+Send>>{Box::pin(async{Err("this host opens no session in this test".to_owned())})});
        let writer=std::sync::Arc::new(crate::session_event_writer::SessionWriterActor::new(tokio::io::sink()));
        std::sync::Arc::new(HostCore::new(HostCoreOptions{agent_dir:std::path::PathBuf::from("/tmp"),cwd:"/tmp".to_owned(),create_runtime,capabilities:vec![crate::custom_capability::EXTENSION_EVENTS_CAPABILITY.to_owned()],close_grace_ms:10_000},writer))
    }
    #[tokio::test]async fn handoff_is_advertised_only_after_its_handler_is_installed(){let core=idle_core();assert!(!core.advertised_capabilities().iter().any(|capability|capability==crate::host_decision::GENERATION_HANDOFF_CAPABILITY));core.enable_handoff();assert!(core.advertised_capabilities().iter().any(|capability|capability==crate::host_decision::GENERATION_HANDOFF_CAPABILITY));}
    #[tokio::test]async fn protocol_info_advertises_host_capabilities_the_decision_requires(){let capabilities=idle_core().advertised_capabilities();for required in crate::host_decision::REQUIRED_HOST_CAPABILITIES{assert!(capabilities.iter().any(|capability|capability==required),"{required}");}assert_eq!(capabilities.iter().filter(|capability|*capability=="multi_session").count(),1);}
    #[tokio::test]async fn an_empty_host_drains_to_exit_once(){let core=idle_core();assert!(!core.drain_sweep().await);core.begin_drain().await;assert!(core.drain_sweep().await);}
    fn peer(core:&HostCore,id:&str)->tokio::io::BufReader<tokio::io::DuplexStream>{let(write,read)=tokio::io::duplex(64*1024);core.register_connection(id,crate::socket_event_fanout::SocketEventSinkActor::new(write,4096,30_000,std::sync::Arc::clone(&core.blocked),|error|panic!("{error}")));tokio::io::BufReader::new(read)}
    async fn next(reader:&mut tokio::io::BufReader<tokio::io::DuplexStream>)->serde_json::Value{use tokio::io::AsyncBufReadExt;let mut line=String::new();tokio::time::timeout(std::time::Duration::from_secs(5),reader.read_line(&mut line)).await.unwrap().unwrap();serde_json::from_str(&line).unwrap()}
    #[tokio::test]async fn a_dropped_connection_gives_up_every_attachment_it_held(){
        let core=idle_core();core.attachments.lock().unwrap().attach("gone","s");
        core.release_connection("gone").await;
        assert!(!core.attachments.lock().unwrap().owns(Some("gone"),"s"));
    }
    #[tokio::test]async fn the_idle_policy_reports_an_empty_host_once_its_window_elapses(){
        let core=idle_core();let(_stop,stopped)=tokio::sync::watch::channel(false);
        assert!(core.run_idle_policy(HostIdlePolicy{idle_eviction_ms:f64::INFINITY,empty_exit_ms:100.},||true,stopped).await);
    }
    #[tokio::test]async fn every_attached_connection_receives_session_records_and_responses_stay_with_their_requester(){
        let core=idle_core();let mut first=peer(&core,"one");let mut second=peer(&core,"two");
        {let mut fanout=core.fanout.lock().unwrap();fanout.attach("one","s",0.).unwrap();fanout.attach("two","s",0.).unwrap();}
        route_session_record(&core.fanout,&core.default_writer,"s",Some("one"),serde_json::json!({"type":"agent_end"}));
        route_session_record(&core.fanout,&core.default_writer,"s",Some("two"),serde_json::json!({"type":"response","command":"prompt"}));
        assert_eq!(next(&mut first).await["type"],"agent_end");assert_eq!(next(&mut second).await["type"],"agent_end");
        let response=next(&mut second).await;assert_eq!(response["command"],"prompt");assert_eq!(response["sessionId"],"s");
    }
    #[tokio::test]async fn late_joiner_replays_the_in_flight_message(){
        let core=idle_core();let _first=peer(&core,"one");let mut late=peer(&core,"late");
        core.fanout.lock().unwrap().attach("one","s",0.).unwrap();
        route_session_record(&core.fanout,&core.default_writer,"s",None,serde_json::json!({"type":"message_start"}));
        core.fanout.lock().unwrap().attach("late","s",0.).unwrap();
        assert_eq!(next(&mut late).await["type"],"message_start");
    }
    #[tokio::test]async fn set_client_info_registers_connection_capabilities(){
        let core=idle_core();let mut client=peer(&core,"c");
        core.handle(Some("c"),&serde_json::json!({"type":"set_client_info","width":40,"capabilities":["media_placeholders"]}).to_string()).await.unwrap();
        assert_eq!(next(&mut client).await["command"],"set_client_info");
        assert_eq!(core.fanout.lock().unwrap().targets.capabilities("c"),Some(&["media_placeholders".to_owned()][..]));
    }
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
/// Process cleanup the embedding binary runs on SIGTERM/SIGHUP before the host exits (senpi
/// `registerShutdownSignals` calls `killTrackedDetachedChildren`, which lives in the CLI crate).
static SIGNAL_CLEANUP:std::sync::OnceLock<fn()>=std::sync::OnceLock::new();
pub fn set_host_signal_cleanup(cleanup:fn()){let _=SIGNAL_CLEANUP.set(cleanup);}
/// Serve the shared host: stdio when no socket was named, otherwise the public Unix socket
/// (senpi `runMultiSessionHost`).
pub async fn run_multi_session_host(core:std::sync::Arc<HostCore>,listen:Option<String>)->std::io::Result<()>{
    run_multi_session_host_with_signals(core,listen,host_shutdown_signals_from_env()).await
}
/// Serve the shared host with explicit shutdown triggers: the seam the CLI entry and its tests use.
pub async fn run_multi_session_host_with_signals(core:std::sync::Arc<HostCore>,listen:Option<String>,signals:HostShutdownSignals)->std::io::Result<()>{
    #[cfg(not(unix))]
    { let _=signals; return match listen.as_deref(){None|Some("stdio://")=>run_stdio_host(core).await,Some(_)=>Err(std::io::Error::other("rpc host sockets are not supported on this platform yet"))}; }
    #[cfg(unix)]
    {
    match listen.as_deref(){None|Some("stdio://")=>run_stdio_host(core).await,Some(path)=>run_socket_host(core,path.to_owned(),signals).await}
    }
}
#[cfg(not(unix))]
async fn run_stdio_host(core:std::sync::Arc<HostCore>)->std::io::Result<()>{
    use tokio::io::AsyncReadExt;
    let mut input=tokio::io::stdin();
    let mut reader=crate::jsonl::JsonlLineReader::new(crate::jsonl::MAX_RPC_LINE_CHARACTERS).map_err(std::io::Error::other)?;
    let mut chunk=[0u8;8192];
    loop{
        let count=input.read(&mut chunk).await?;
        let records=if count==0{reader.finish()}else{reader.push(&chunk[..count])};
        for record in records{let crate::jsonl::LineRecord::Line(line)=record else{continue};core.handle(None,&line).await.map_err(std::io::Error::other)?;}
        if count==0{core.default_writer.flush().await.map_err(std::io::Error::other)?;return Ok(());}
    }
}
#[cfg(unix)]
async fn run_stdio_host(core:std::sync::Arc<HostCore>)->std::io::Result<()>{
    use tokio::io::AsyncReadExt;
    let mut input=tokio::io::stdin();
    let mut reader=crate::jsonl::JsonlLineReader::new(crate::jsonl::MAX_RPC_LINE_CHARACTERS).map_err(std::io::Error::other)?;
    let mut chunk=[0u8;8192];
    let mut commands=Vec::new();
    // An empty host (no session ever opened, or all closed) must not stay resident forever: the
    // idle policy evicts idle sessions and ends the host once the empty window elapses (senpi
    // `runStdioHost`'s `onEmptyExit` + `startHostObservers`).
    let env:HashMap<String,String>=std::env::vars().collect();
    let(stop,stopped)=tokio::sync::watch::channel(false);
    let observers=core.start_host_observers(stopped.clone());
    let idle_core=core.clone();let policy=resolve_host_idle_policy(&env,None,None);
    let mut empty_exit=tokio::spawn(async move{idle_core.run_idle_policy(policy,||true,stopped).await});
    let mut empty_exit_done=false;
    let result=loop{
        let count=tokio::select!{
            result=poll_host_commands(&mut commands),if !commands.is_empty()=>{if let Err(error)=result{break Err(std::io::Error::other(error));}continue;},
            exit=&mut empty_exit,if !empty_exit_done=>{empty_exit_done=true;if matches!(exit,Ok(true)){break core.default_writer.flush().await.map_err(std::io::Error::other);}continue;},
            count=input.read(&mut chunk)=>match count{Ok(count)=>count,Err(error)=>break Err(error)},
        };
        let records=if count==0{reader.finish()}else{reader.push(&chunk[..count])};
        for record in records{let crate::jsonl::LineRecord::Line(line)=record else{continue};let core=core.clone();commands.push(Box::pin(async move{core.handle(None,&line).await}) as HostCommand);}
        if count==0{drop(commands);break core.default_writer.flush().await.map_err(std::io::Error::other);}
    };
    let _=stop.send(true);observers.abort();empty_exit.abort();
    result
}
type HostCommand=std::pin::Pin<Box<dyn std::future::Future<Output=Result<(),String>>+Send>>;
async fn poll_host_commands(commands:&mut Vec<HostCommand>)->Result<(),String>{
    let(index,result)=std::future::poll_fn(|cx|{
        for(index,command)in commands.iter_mut().enumerate(){if let std::task::Poll::Ready(result)=command.as_mut().poll(cx){return std::task::Poll::Ready((index,result));}}
        std::task::Poll::Pending
    }).await;
    drop(commands.remove(index));result
}
#[cfg(unix)]
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
    // Self-observation, idle eviction and the child reaper run for the host's life (senpi
    // `startHostObservers`, the router's idle sweep, `startHostChildReaper`). This loop keeps
    // the connection-gated empty exit, so the sweep here only evicts.
    let env:HashMap<String,String>=std::env::vars().collect();
    let(observers_stop,observers_stopped)=tokio::sync::watch::channel(false);
    let observers=core.start_host_observers(observers_stopped.clone());
    let idle_policy=resolve_host_idle_policy(&env,None,Some(f64::INFINITY));
    let idle_core=core.clone();
    let idle=tokio::spawn(async move{idle_core.run_idle_policy(idle_policy,||false,observers_stopped).await;});
    let reaper=crate::child_reaper::start_host_child_reaper(&env,std::env::consts::OS,|message|eprintln!("senpi rpc host: {message}"));
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
            _=terminate.recv()=>{if let Some(cleanup)=SIGNAL_CLEANUP.get(){cleanup();}signal_code=Some(143);break;},
            _=hangup.recv()=>{if let Some(cleanup)=SIGNAL_CLEANUP.get(){cleanup();}signal_code=Some(129);break;},
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
                let empty=(draining||connections.load(std::sync::atomic::Ordering::SeqCst)==0)&&core.session_count().await==0;
                match empty_exit_ms.filter(|window|window.is_finite()){
                    Some(window) if empty=>{let since=*empty_since.get_or_insert(tokio::time::Instant::now());if since.elapsed().as_secs_f64()*1000.>=window{break;}},
                    _=>empty_since=None,
                }
            }
        }
    }
    watchdog_task.abort();
    let _=observers_stop.send(true);observers.abort();idle.abort();
    if let Some(reaper)=reaper{reaper.stop();}
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
#[cfg(unix)]
async fn serve_socket_connection(core:std::sync::Arc<HostCore>,connection:String,socket:tokio::net::UnixStream){
    let(read,write)=socket.into_split();
    let failed=connection.clone();
    let actor=crate::socket_event_fanout::SocketEventSinkActor::new(write,crate::socket_event_fanout::DEFAULT_QUEUE_BYTES,crate::socket_event_fanout::DEFAULT_STALL_MS,std::sync::Arc::clone(&core.blocked),move|error|eprintln!("senpi rpc connection {failed} cut: {error}"));
    core.register_connection(&connection,actor);
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
    core.release_retained_attachments(&connection).await;
    core.unregister_connection(&connection);
    // A socket that dies without close_session still owns its sessions' attachments and path
    // reservations (senpi `detach` -> `router.releaseConnection`).
    core.release_connection(&connection).await;
}
#[cfg(test)]mod tests{use super::*;#[test]fn explicit_policy_overrides_environment_and_bad_env_uses_defaults(){let env=HashMap::from([(RPC_SESSION_IDLE_EVICTION_MS_ENV.into(),"100".into()),(RPC_HOST_EMPTY_EXIT_MS_ENV.into(),"invalid".into())]);assert_eq!(resolve_host_idle_policy(&env,None,None),HostIdlePolicy{idle_eviction_ms:100.,empty_exit_ms:DEFAULT_HOST_EMPTY_EXIT_MS});assert_eq!(resolve_host_idle_policy(&env,Some(f64::INFINITY),Some(0.)),HostIdlePolicy{idle_eviction_ms:f64::INFINITY,empty_exit_ms:0.});}}
