//! Production selection of a worker session on the parent's task-host shard.
pub type OpenedHostResult = Result<
        (
            Arc<dyn handle_port::HostSessionPort>,
            open_session::OpenedHostSession,
            Arc<dyn Fn() -> senpi_task::runners::RunnerOutcome + Send + Sync>,
        ),
        HostError,
    >;
#[path = "task_host_connection.rs"]
mod connection;
use senpi_task::{
    host::HostError,
    manager::{child_handle::ManagedChildHandle, types::*},
    runners::rpc_host::*,
};
#[cfg(unix)]
use serde_json::json;
use std::{path::PathBuf, sync::Arc};
fn error(error: impl std::fmt::Display) -> HostError {
    HostError {
        message: error.to_string(),
    }
}
#[derive(Clone)]
pub struct HostAssembly {
    pub session_context: std::collections::BTreeMap<String, String>,
    pub turns: Arc<std::sync::Mutex<std::collections::BTreeMap<String, connection::TurnState>>>,
    pub runtime: tokio::runtime::Handle,
    pub agent_dir: PathBuf,
    pub parent: Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>,
}
impl HostAssembly {
    fn recorded_socket(
        &self,
        spec: &senpi_task::runners::types::RpcRunnerSpec,
    ) -> Result<String, HostError> {
        let store = senpi_task::store::TaskRecordStore::new(&senpi_task::store::StateDirConfig {
            project_dir: spec.cwd.clone().into(),
            task_state_dir: Some(spec.state_dir.clone().into()),
        });
        match store
            .load(&spec.task_id)
            .map_err(error)?
            .and_then(|record| record.host_session)
        {
            Some(identity) => Ok(identity.socket),
            None => self.socket(),
        }
    }
    fn schedule_recovery(
        &self,
        handle: &Arc<handle::HostSessionHandle>,
        spec: senpi_task::runners::types::RpcRunnerSpec,
    ) {
        let weak = Arc::downgrade(handle);
        let assembly = self.clone();
        std::thread::spawn(move || {
            while let Some(handle) = weak.upgrade() {
                if handle.stopped() {
                    break;
                }
                if handle.heartbeat().is_err() {
                    let input = assembly.reattach_input(spec.clone());
                    let socket = match assembly.recorded_socket(&spec) {
                        Ok(socket) => socket,
                        Err(error) => {
                            handle.transport_lost(format!("host recovery routing failed: {error}"));
                            break;
                        }
                    };
                    match handle.recover(
                        Arc::new(input),
                        socket,
                        std::time::Duration::from_secs(30),
                    ) {
                        Ok(true) => {}
                        result => {
                            handle.transport_lost(format!("host recovery exhausted: {result:?}"));
                            break;
                        }
                    }
                }
                drop(handle);
                std::thread::park_timeout(std::time::Duration::from_secs(5));
            }
        });
    }
    fn reattach_input(
        &self,
        spec: senpi_task::runners::types::RpcRunnerSpec,
    ) -> reattach_port::ReattachPortInput {
        let admission = self.clone();
        let ensure = self.clone();
        let open = self.clone();
        let store = spec.state_dir.clone();
        let extensions = spec.extensions.clone().unwrap_or_default();
        reattach_port::ReattachPortInput {
            delays: [500, 1000, 2000, 4000, 8000]
                .into_iter()
                .map(std::time::Duration::from_millis)
                .collect(),
            sleep: Arc::new(std::thread::sleep),
            admit: Arc::new(move || admission.admit(store.clone()).map_err(|e| e.to_string())),
            ensure: Arc::new(move |socket| {
                ensure
                    .ensure(socket.into(), extensions.clone())
                    .map_err(|e| e.to_string())
            }),
            open: Arc::new(move |socket, path| {
                open.open(
                    &spec,
                    session_context::build_child_context(&spec),
                    socket.into(),
                    path.into(),
                )
                .map(|(port, opened, _)| (port, opened.session))
                .map_err(|e| e.to_string())
            }),
            notice: Arc::new(|notice| eprintln!("task host recovery: {notice}")),
            attach_only: self.session_context.contains_key("shard_key"),
        }
    }
    pub fn block<T: Send + 'static>(
        &self,
        future: impl std::future::Future<Output = Result<T, HostError>> + Send + 'static,
    ) -> Result<T, HostError> {
        // Callers include synchronous extension hooks on a current-thread runtime. Borrowing
        // that runtime's handle while joining this thread stalls its I/O driver forever.
        static HOST_RUNTIME: std::sync::OnceLock<
            Result<tokio::runtime::Runtime, std::io::Error>,
        > = std::sync::OnceLock::new();
        let runtime = HOST_RUNTIME
            .get_or_init(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
            })
            .as_ref()
            .map_err(error)?;
        std::thread::spawn(move || runtime.block_on(future))
            .join()
            .map_err(|_| error("task host assembly thread panicked"))?
    }
    pub fn socket(&self) -> Result<String, HostError> {
        let parent =
            (self.parent)().ok_or_else(|| error("task host routing requires a parent session"))?;
        let key = self
            .session_context
            .get("shard_key")
            .filter(|key| key.len() == 16 && key.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .cloned()
            .unwrap_or_else(|| {
                maho_rpc::host_endpoints::shard_key(
                    maho_rpc::host_endpoints::ShardKind::P,
                    &parent.session_id(),
                )
            });
        Ok(maho_rpc::host_endpoints::shard_socket_path_for_key(
            &self.agent_dir.join("rpc"),
            maho_rpc::host_endpoints::ShardKind::P,
            &key,
        )
        .to_string_lossy()
        .into_owned())
    }
    pub fn ensure(&self, socket: String, extensions: Vec<String>) -> Result<String, HostError> {
        let agent_dir = self.agent_dir.clone();
        let attach_only = self.session_context.contains_key("shard_key");
        self.block(async move {
            if attach_only {
                #[cfg(unix)]
                {
                    let reply = maho_rpc::host_probe::ask_host_authenticated(
                        &socket,
                        &json!({"type":"get_protocol_info","observe":true}),
                        10_000,
                        Some(&agent_dir),
                    )
                    .await
                    .map_err(error)?;
                    if reply.is_none() {
                        return Err(error("own_host_unreachable"));
                    }
                    return Ok(socket);
                }
            }
            maho_rpc::host_ensure::ensure_host(maho_rpc::host_ensure::EnsureHostOptions {
                socket,
                agent_dir: Some(agent_dir),
                extensions,
                ..Default::default()
            })
            .await
            .map(|host| host.socket)
            .map_err(error)
        })
    }
    pub fn admit(&self, store: String) -> Result<(), HostError> {
        let index = store_index::task_store_index_path(&self.agent_dir);
        self.block(async move {
            store_index::register_store_index(
                index,
                store.into(),
                chrono::Utc::now().timestamp_millis(),
            )
            .await
            .map_err(|e| error(format!("store_index_unavailable: {e}")))
        })
    }
    pub fn open(
        &self,
        spec: &senpi_task::runners::types::RpcRunnerSpec,
        context: session_context::ChildSessionContext,
        socket: String,
        path: String,
    ) -> OpenedHostResult {
        let mut context = context;
        if let Some((_, key)) = maho_rpc::host_endpoints::parse_shard_socket(&socket) {
            context.context.insert("shard_key".into(), key);
        }
        self.admit(spec.state_dir.clone())?;
        let turn = self
            .turns
            .lock()
            .expect("host turn registry lock poisoned")
            .entry(path.clone())
            .or_insert_with(|| {
                Arc::new((
                    std::sync::Mutex::new(connection::TurnData::default()),
                    std::sync::Condvar::new(),
                ))
            })
            .clone();
        let spec = spec.clone();
        let agent_dir = self.agent_dir.clone();
        let runtime = self.runtime.clone();
        self.block(async move {
            #[cfg(unix)]
            {
                let (socket_client,info)=maho_rpc::host_probe::authenticated_host_socket(&socket,&json!({"type":"get_protocol_info","observe":true}),30_000,Some(&agent_dir)).await.map_err(error)?;
                let client=Arc::new(maho_rpc::rpc_client::RpcClient::from_stream(socket_client.into_stream().map_err(error)?,Default::default()));
                let live=connection::LiveClient::new(client.clone(),runtime.clone(),turn);
                let mut spec=spec.clone();
                if let Some(reference)=spec.model.as_deref() {
                    let decorators=maho_omo_task::planner_explicit_pin::split_model_decorators(reference);
                    spec.model=Some(decorators.base);spec.reasoning=spec.reasoning.or(spec.variant.clone()).or(decorators.thinking_level);
                }
                let mut request=json!({"sessionPath":path,"cwd":spec.cwd,"kind":context.kind,"context":context.context,"retainOnDisconnect":true,"autoTitle":false});
                if let Some(reference)=spec.model.as_deref() {
                    let (provider,id)=reference.split_once('/').ok_or_else(||error("model_unavailable: noncanonical host model pin"))?;
                    request["provider"]=provider.into();request["modelId"]=id.into();
                }
                if let (Some(model),Some(chain))=(&spec.model,&spec.fallback_models)
                    && !chain.is_empty(){request["retryFallback"]=json!({"modelFallback":true,"fallbackChains":{(model):chain}});}
                if let Some(reasoning)=spec.reasoning.as_ref().or(spec.variant.as_ref()){request["thinkingLevel"]=reasoning.clone().into();}
                let opened=match client.open_session(request).await {Ok(opened)=>opened,Err(cause)=>{client.stop().await;return Err(error(cause));}};
                let routing_id=client.session_id().ok_or_else(||error("host open returned no routing ID"))?;
                let watch=live.clone();
                let port=Arc::new(session_client_connection::SessionClientConnection::new(routing_id.clone(),live.clone(),live.clone()).with_turn_watcher(Arc::new(move ||watch.watch())));
                let outcome=live.clone();
                Ok((port as Arc<dyn handle_port::HostSessionPort>,open_session::OpenedHostSession {
                    session:handle_port::HostSessionIdentity {routing_id,session_path:path,instance_id:info["data"]["instanceId"].as_str().unwrap_or_default().into()},
                    attached:opened["attached"]==true,durable_session_id:None,reported_model:None,
                },Arc::new(move ||outcome.outcome()) as Arc<dyn Fn()->senpi_task::runners::RunnerOutcome+Send+Sync>))
            }
            #[cfg(not(unix))] {let _=(spec,agent_dir,runtime,socket,path,context,turn);Err(error("host runner unavailable on Windows"))}
        })
    }
}
pub struct PrewarmHost(pub HostAssembly);

impl maho_omo_task::host_prewarm::HostPrewarmHost for PrewarmHost {
    fn agent_dir(&self) -> Option<String> {
        Some(self.0.agent_dir.to_string_lossy().into_owned())
    }
    fn shard_socket(&self) -> String {
        self.0.socket().unwrap_or_default()
    }
    fn is_own_endpoint(&self, socket: &str) -> bool {
        self.0.session_context.contains_key("shard_key")
            && self.0.socket().is_ok_and(|own| own == socket)
    }
    fn routes_children_to_host(&self) -> bool {
        true
    }
    fn ensure_endpoint(&self, socket: &str) -> Result<(), String> {
        self.0
            .ensure(socket.into(), Vec::new())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn warm_host(&self, socket: &str, cwd: &str, agent_dir: Option<&str>) -> Result<(), String> {
        let socket = self
            .0
            .ensure(socket.into(), Vec::new())
            .map_err(|e| e.to_string())?;
        let cwd = cwd.to_owned();
        let root = agent_dir.map(PathBuf::from);
        let state = tempfile::Builder::new()
            .prefix("omo-host-warmup-")
            .tempdir()
            .map_err(|error| error.to_string())?;
        let assembly = self.0.clone();
        let ask_assembly = assembly.clone();
        let fallback_assembly = assembly.clone();
        host_warmup::warm_task_host(&socket,&cwd,root.as_deref(),state.path(),
            move |socket,command,timeout,root| {
                let socket=socket.to_owned();let root=root.map(ToOwned::to_owned);
                ask_assembly.block(async move {
                    #[cfg(unix)] { maho_rpc::host_probe::ask_host_authenticated(&socket,&command,timeout,root.as_deref()).await.map_err(error) }
                    #[cfg(not(unix))] {let _=(socket,command,timeout,root);Err(error("host warm unavailable"))}
                }).map_err(|error|error.to_string())
            },
            |context| {
                let socket=socket.clone();let root=root.clone();let cwd=cwd.clone();let path=state.path().join("warm.jsonl");
                fallback_assembly.block(async move {
                    #[cfg(unix)] {
                        let (socket,_) = maho_rpc::host_probe::authenticated_host_socket(&socket,&json!({"type":"get_protocol_info","observe":true}),60_000,root.as_deref()).await.map_err(error)?;
                        let client=maho_rpc::rpc_client::RpcClient::from_stream(socket.into_stream().map_err(error)?,Default::default());
                        let result=async {client.open_session(json!({"sessionPath":path,"cwd":cwd,"kind":"worker","context":context,"retainOnDisconnect":false,"autoTitle":false})).await.map_err(error)?;client.close_session(None).await.map_err(error)}.await;
                        client.stop().await;result
                    }
                    #[cfg(not(unix))] {let _=(socket,root,cwd,path,context);Err(error("host warm unavailable"))}
                }).map_err(|error|error.to_string())
            }).map(|_|())
    }
}

pub struct ProductionHostRunner {
    assembly: HostAssembly,
    configured: ConfiguredRpcHostRunner,
}
impl ProductionHostRunner {
    fn start_rpc(&self, rpc: senpi_task::runners::types::RpcRunnerSpec) -> ManagedRunnerResult {
        let mut rpc = rpc;
        if let Some(reference) = rpc.model.as_deref() {
            let decorators = maho_omo_task::planner_explicit_pin::split_model_decorators(reference);
            rpc.model = Some(decorators.base);
            rpc.reasoning = rpc
                .reasoning
                .or(rpc.variant.clone())
                .or(decorators.thinking_level);
        }
        (self.configured.admit)(&rpc).map_err(|e| ManagedRunnerError::Other(e.to_string()))?;
        let handle = self
            .configured
            .runner
            .start(&rpc, |spec, context| (self.configured.open)(spec, context))
            .map_err(|e| ManagedRunnerError::Other(e.to_string()))?;
        let store = senpi_task::store::TaskRecordStore::new(&senpi_task::store::StateDirConfig {
            project_dir: rpc.cwd.clone().into(),
            task_state_dir: Some(rpc.state_dir.clone().into()),
        });
        let identity = handle.identity.current();
        let socket = self
            .assembly
            .recorded_socket(&rpc)
            .map_err(|error| ManagedRunnerError::Other(error.to_string()))?;
        let recorded = store.mutate(&rpc.task_id, |current| {
            let mut record = current.clone();
            record.runner_kind = Some(senpi_task::state::RunnerKind::HostSession);
            record.host_session = Some(senpi_task::state::HostSessionIdentity {
                socket,
                routing_id: identity.routing_id,
                session_path: identity.session_path,
                instance_id: identity.instance_id,
                daemon_pid: None,
                effect_seq: None,
                adopted_entry_id: None,
            });
            record
        });
        if let Err(error) = recorded {
            if let Err(cleanup) = handle.dispose() {
                eprintln!("host record failure cleanup: {cleanup}");
            }
            return Err(ManagedRunnerError::Other(error.to_string()));
        }
        self.assembly.schedule_recovery(&handle, rpc);
        Ok(handle)
    }
}
impl ManagedRunner for ProductionHostRunner {
    fn resume(&self, spec: &ManagedStartSpec, path: &str) -> Option<ManagedRunnerResult> {
        let mut rpc = senpi_task::manager::runner::to_rpc_spec(spec);
        rpc.resume_session_path = Some(path.into());
        Some(self.start_rpc(rpc))
    }
    fn start(&self, spec: &ManagedStartSpec) -> ManagedRunnerResult {
        self.start_rpc(senpi_task::manager::runner::to_rpc_spec(spec))
    }
    fn settle_teardowns(&self) -> Result<(), ManagedRunnerError> {
        use senpi_task::manager::runner::RpcRunnerLike;
        self.configured
            .wait_for_teardown_settlement()
            .map_err(ManagedRunnerError::Runner)
    }
}
pub struct HostRespawn {
    pub assembly: HostAssembly,
    pub settings: serde_json::Value,
    pub runner: std::sync::OnceLock<ProductionHostRunner>,
    pub fallback: Arc<dyn RpcRespawnRunner>,
}
impl RpcRespawnRunner for HostRespawn {
    fn start(&self, spec: &senpi_task::runners::types::RpcRunnerSpec) -> ManagedRunnerResult {
        if cfg!(windows)
            || self.settings["task"]["process_runner"]
                .as_str()
                .unwrap_or("host")
                != "host"
        {
            return self.fallback.start(spec);
        }
        self.runner
            .get_or_init(|| build_host_runner(self.assembly.clone(), Arc::new(|| Ok(()))))
            .start_rpc(spec.clone())
    }
}
fn build_host_runner(
    assembly: HostAssembly,
    settle: Arc<dyn Fn() -> Result<(), HostError> + Send + Sync>,
) -> ProductionHostRunner {
    let open_assembly = assembly.clone();
    let admission = assembly.clone();
    let configured = ConfiguredRpcHostRunner {
        runner: RpcHostRunner {
            opener: session_open::create_host_session_opener(),
            settle_fallback: settle,
        },
        admit: Arc::new(move |spec| admission.admit(spec.state_dir.clone())),
        open: Arc::new(move |spec, context| {
            let socket = open_assembly.ensure(
                open_assembly.recorded_socket(spec)?,
                spec.extensions.clone().unwrap_or_default(),
            )?;
            let path = spec.resume_session_path.clone().unwrap_or_else(|| {
                session_context::resolve_child_session_path(
                    std::path::Path::new(&spec.state_dir),
                    &spec.task_id,
                    chrono::Utc::now(),
                    &uuid::Uuid::new_v4().to_string(),
                )
                .to_string_lossy()
                .into_owned()
            });
            open_assembly.open(spec, context, socket, path)
        }),
    };
    ProductionHostRunner {
        assembly,
        configured,
    }
}
pub fn select_runner(
    assembly: HostAssembly,
    settings: &serde_json::Value,
    fallback: Arc<dyn ManagedRunner>,
) -> Arc<dyn ManagedRunner> {
    if cfg!(windows)
        || settings["task"]["process_runner"]
            .as_str()
            .unwrap_or("host")
            != "host"
    {
        return fallback;
    }
    let cleanup = fallback.clone();
    Arc::new(build_host_runner(
        assembly,
        Arc::new(move || cleanup.settle_teardowns().map_err(error)),
    ))
}

#[cfg(test)]
mod blocking_tests {
    #[tokio::test(flavor = "current_thread")]
    async fn synchronous_host_call_drives_its_own_io() {
        let assembly = super::HostAssembly {
            session_context: Default::default(),
            turns: Default::default(),
            runtime: tokio::runtime::Handle::current(),
            agent_dir: Default::default(),
            parent: std::sync::Arc::new(|| None),
        };
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = assembly.block(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .map_err(super::error)?;
                let address = listener.local_addr().map_err(super::error)?;
                let (accepted, connected) = tokio::join!(
                    listener.accept(),
                    tokio::net::TcpStream::connect(address)
                );
                accepted.map_err(super::error)?;
                connected.map_err(super::error)?;
                Ok(())
            });
            send.send(result).expect("report host I/O result");
        });
        // Deliberately block this runtime like a synchronous extension hook.
        receive
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("host I/O must not need the blocked caller's driver")
            .expect("host I/O succeeds");
    }
}
