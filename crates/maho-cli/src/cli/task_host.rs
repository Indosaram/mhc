//! Production selection of a worker session on the parent's task-host shard.
#[path = "task_host_connection.rs"]
mod connection;
use senpi_task::{
    host::HostError,
    manager::{child_handle::ManagedChildHandle, types::*},
    runners::rpc_host::*,
};
use serde_json::json;
use std::{path::PathBuf, sync::Arc};
fn error(error: impl std::fmt::Display) -> HostError {
    HostError {
        message: error.to_string(),
    }
}
#[derive(Clone)]
pub struct HostAssembly {
    pub turns: Arc<std::sync::Mutex<std::collections::BTreeMap<String, connection::TurnState>>>,
    pub runtime: tokio::runtime::Handle,
    pub agent_dir: PathBuf,
    pub parent: Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>,
}
impl HostAssembly {
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
                    let socket = match assembly.socket() {
                        Ok(socket) => socket,
                        Err(error) => {
                            eprintln!("host recovery routing failed: {error}");
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
            attach_only: false,
        }
    }
    pub fn block<T: Send + 'static>(
        &self,
        future: impl std::future::Future<Output = Result<T, HostError>> + Send + 'static,
    ) -> Result<T, HostError> {
        let runtime = self.runtime.clone();
        std::thread::spawn(move || runtime.block_on(future))
            .join()
            .map_err(|_| error("task host assembly thread panicked"))?
    }
    pub fn socket(&self) -> Result<String, HostError> {
        let parent =
            (self.parent)().ok_or_else(|| error("task host routing requires a parent session"))?;
        let key = maho_rpc::host_endpoints::shard_key(
            maho_rpc::host_endpoints::ShardKind::P,
            &parent.session_id(),
        );
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
        self.block(async move {
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
    ) -> Result<
        (
            Arc<dyn handle_port::HostSessionPort>,
            open_session::OpenedHostSession,
            Arc<dyn Fn() -> senpi_task::runners::RunnerOutcome + Send + Sync>,
        ),
        HostError,
    > {
        self.admit(spec.state_dir.clone())?;
        let turn = self
            .turns
            .lock()
            .unwrap()
            .entry(path.clone())
            .or_insert_with(|| {
                Arc::new((std::sync::Mutex::new((0, None)), std::sync::Condvar::new()))
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
                let mut request=json!({"sessionPath":path,"cwd":spec.cwd,"kind":context.kind,"context":context.context,"retainOnDisconnect":true,"autoTitle":false});
                if let Some(reference)=spec.model.as_deref() {
                    let (provider,id)=reference.split_once('/').ok_or_else(||error("model_unavailable: noncanonical host model pin"))?;
                    request["provider"]=provider.into();request["modelId"]=id.into();
                }
                if let Some(reasoning)=spec.reasoning.as_ref().or(spec.variant.as_ref()){request["thinkingLevel"]=reasoning.clone().into();}
                let opened=client.open_session(request).await.map_err(error)?;
                let routing_id=client.session_id().ok_or_else(||error("host open returned no routing ID"))?;
                let watch=live.clone();
                let port=Arc::new(session_client_connection::SessionClientConnection::new(routing_id.clone(),live.clone(),live.clone()).with_turn_watcher(Arc::new(move ||watch.watch())));
                let outcome=live.clone();
                Ok((port as Arc<dyn handle_port::HostSessionPort>,open_session::OpenedHostSession {
                    session:handle_port::HostSessionIdentity {routing_id,session_path:path,instance_id:info["data"]["instanceId"].as_str().unwrap_or_default().into()},
                    attached:opened["attached"]==true,durable_session_id:None,reported_model:None,
                },Arc::new(move ||outcome.outcome()) as Arc<dyn Fn()->senpi_task::runners::RunnerOutcome+Send+Sync>))
            }
            #[cfg(not(unix))] {let _=(spec,agent_dir,runtime,socket,path,context);Err(error("host runner unavailable on Windows"))}
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
        self.0.block(async move {
            #[cfg(unix)] {maho_rpc::host_probe::ask_host_authenticated(&socket,&json!({"type":"warm","cwd":cwd,"kind":"worker","context":{"role":"child","host_warmup":"1"}}),60_000,root.as_deref()).await.map_err(error)?.filter(|reply|reply["success"]==true).ok_or_else(||error("host warm refused"))?; Ok(())}
            #[cfg(not(unix))] {let _=(socket,cwd,root);Err(error("host warm unavailable"))}
        }).map_err(|e|e.to_string())
    }
}

struct ProductionHostRunner {
    assembly: HostAssembly,
    configured: ConfiguredRpcHostRunner,
}
impl ProductionHostRunner {
    fn start_rpc(&self, rpc: senpi_task::runners::types::RpcRunnerSpec) -> ManagedRunnerResult {
        (self.configured.admit)(&rpc).map_err(|e| ManagedRunnerError::Other(e.to_string()))?;
        let handle = self
            .configured
            .runner
            .start(&rpc, |spec, context| (self.configured.open)(spec, context))
            .map_err(|e| ManagedRunnerError::Other(e.to_string()))?;
        self.assembly.schedule_recovery(&handle, rpc);
        Ok(handle)
    }
}
impl ManagedRunner for ProductionHostRunner {
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
    pub fallback: Arc<dyn RpcRespawnRunner>,
}
impl RpcRespawnRunner for HostRespawn {
    fn start(&self, spec: &senpi_task::runners::types::RpcRunnerSpec) -> ManagedRunnerResult {
        if cfg!(windows) || self.settings["task"]["process_runner"].as_str() != Some("host") {
            return self.fallback.start(spec);
        }
        build_host_runner(self.assembly.clone(), Arc::new(|| Ok(()))).start_rpc(spec.clone())
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
                open_assembly.socket()?,
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
    if cfg!(windows) || settings["task"]["process_runner"].as_str() != Some("host") {
        return fallback;
    }
    let cleanup = fallback.clone();
    Arc::new(build_host_runner(
        assembly,
        Arc::new(move || cleanup.settle_teardowns().map_err(error)),
    ))
}
