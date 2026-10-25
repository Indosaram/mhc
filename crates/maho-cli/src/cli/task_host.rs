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
    pub runtime: tokio::runtime::Handle,
    pub agent_dir: PathBuf,
    pub parent: Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>,
}
impl HostAssembly {
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
        let spec = spec.clone();
        let agent_dir = self.agent_dir.clone();
        let runtime = self.runtime.clone();
        self.block(async move {
            #[cfg(unix)]
            {
                let (socket_client,info)=maho_rpc::host_probe::authenticated_host_socket(&socket,&json!({"type":"get_protocol_info","observe":true}),30_000,Some(&agent_dir)).await.map_err(error)?;
                let client=Arc::new(maho_rpc::rpc_client::RpcClient::from_stream(socket_client.into_stream(),Default::default()));
                let live=connection::LiveClient::new(client.clone(),runtime.clone());
                let opened=client.open_session(json!({"sessionPath":path,"cwd":spec.cwd,"kind":context.kind,"context":context.context,"retainOnDisconnect":true,"autoTitle":false,"model":spec.model,"thinkingLevel":spec.reasoning})).await.map_err(error)?;
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
struct ProductionHostRunner {
    assembly: HostAssembly,
    configured: ConfiguredRpcHostRunner,
}
impl ManagedRunner for ProductionHostRunner {
    fn start(&self, spec: &ManagedStartSpec) -> ManagedRunnerResult {
        use senpi_task::manager::runner::RpcRunnerLike;
        self.configured
            .start(&senpi_task::manager::runner::to_rpc_spec(spec))
            .map_err(ManagedRunnerError::Runner)
    }
    fn settle_teardowns(&self) -> Result<(), ManagedRunnerError> {
        use senpi_task::manager::runner::RpcRunnerLike;
        self.configured
            .wait_for_teardown_settlement()
            .map_err(ManagedRunnerError::Runner)
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
    let open_assembly = assembly.clone();
    let admission = assembly.clone();
    let cleanup = fallback.clone();
    let configured = ConfiguredRpcHostRunner {
        runner: RpcHostRunner {
            opener: session_open::create_host_session_opener(),
            settle_fallback: Arc::new(move || cleanup.settle_teardowns().map_err(error)),
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
    Arc::new(ProductionHostRunner {
        assembly,
        configured,
    })
}
