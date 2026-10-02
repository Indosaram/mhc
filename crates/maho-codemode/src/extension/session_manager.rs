use std::{collections::HashMap, path::PathBuf, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use serde_json::json;
use maho_ext_api::ExecuteToolOptions;
use crate::{bridge::{http_server::{BridgeServerHandle, BridgeServerOptions, BridgeCompletionHandler, start_bridge_server}, protocol::BridgeConnectionConfig}, bridges::{agent_bridge::AgentBridge, output_bridge::OutputExecuteTool, schema_bridge::EvalSchemaToolInfo, reserved_dispatch::{ReservedDispatchContext, is_reserved_tool_name, run_reserved_tool}}, config::settings::CodemodeSettings, kernels::{py::{kernel::PythonKernel, kernel_contract::PythonKernelStartOptions}, session_env::SessionEnvironment}, tool::tool_result_marshal::marshal_tool_result};
use super::session_manager_proxy::{SessionManagerLifecycle, SessionDisposeFuture};

pub struct CreateCodemodeSessionManagerOptions {
    pub session_id: String,
    pub cwd: PathBuf,
    pub settings: CodemodeSettings,
    pub local_roots: Option<HashMap<String, String>>,
    pub artifacts_dir: Option<PathBuf>,
    pub session_env: Option<SessionEnvironment>,
    pub executor: Arc<dyn OutputExecuteTool>,
    pub list_tools: Option<Arc<dyn Fn() -> Vec<EvalSchemaToolInfo> + Send + Sync>>,
    pub complete: BridgeCompletionHandler,
}

pub struct CodemodeSessionManager {
    options: CreateCodemodeSessionManagerOptions,
    bridge: BridgeServerHandle,
    python: tokio::sync::Mutex<Option<Arc<PythonKernel>>>,
    disposed: AtomicBool,
    dispose_result: tokio::sync::OnceCell<Result<(), String>>,
}

impl CodemodeSessionManager {
    pub async fn start(options: CreateCodemodeSessionManagerOptions) -> Result<Self, std::io::Error> {
        let executor = options.executor.clone();
        let list_tools = options.list_tools.clone();
        let task_tools = options.settings.task_tools.clone();
        let agent_bridge = Arc::new(AgentBridge::default());
        let bridge = start_bridge_server(BridgeServerOptions {
            token: None, body_limit_bytes: None,
            on_emit: Arc::new(|_, _| Box::pin(async { Ok(()) })),
            on_completion: options.complete.clone(),
            on_call: Arc::new(move |request| {
                let executor = executor.clone();
                let tools = list_tools.as_ref().map(|list| list());
                let task_tools = task_tools.clone();
                let agent_bridge = agent_bridge.clone();
                Box::pin(async move {
                    let execute_options = ExecuteToolOptions { signal: Some(request.signal), ..Default::default() };
                    if is_reserved_tool_name(&request.tool_name) {
                        run_reserved_tool(&request.tool_name, ReservedDispatchContext {
                            call_id: &request.call_id, args: &request.args, executor: executor.as_ref(),
                            task_tool_name: &task_tools.task, task_output_tool_name: &task_tools.output,
                            tools: tools.as_deref(), execute_options, emit_status: None, agent_bridge: &agent_bridge,
                        }).await.map_err(|error| json!({"name":"Error","message":error.to_string()}))
                    } else {
                        executor.execute_tool(&request.tool_name, request.args, execute_options).await
                            .map(|result| marshal_tool_result(&result))
                            .map_err(|error| json!({"name":"Error","message":error.to_string()}))
                    }
                })
            }),
        }).await?;
        Ok(Self { options, bridge, python:tokio::sync::Mutex::new(None), disposed:AtomicBool::new(false), dispose_result:tokio::sync::OnceCell::new() })
    }

    pub fn bridge_endpoint(&self) -> Result<(u16, &str), String> {
        if self.disposed.load(Ordering::SeqCst) { return Err("codemode session manager is disposed".into()); }
        Ok((self.bridge.port, &self.bridge.token))
    }

    pub async fn get_python_kernel(&self, interpreter_path: &str) -> Result<Arc<PythonKernel>, String> {
        if self.disposed.load(Ordering::SeqCst) { return Err("codemode session manager is disposed".into()); }
        let mut python = self.python.lock().await;
        if self.disposed.load(Ordering::SeqCst) { return Err("codemode session manager is disposed".into()); }
        if let Some(kernel) = &*python { return Ok(kernel.clone()); }
        let roots = self.options.local_roots.clone().or_else(|| self.options.artifacts_dir.as_ref().map(|artifacts| HashMap::from([("local".into(), artifacts.join("local").to_string_lossy().into_owned())])));
        let width = self.options.settings.parallel_pool_width;
        let kernel = Arc::new(PythonKernel::start(PythonKernelStartOptions {
            interpreter_path:interpreter_path.into(), session_id:self.options.session_id.clone(), cwd:self.options.cwd.clone(),
            connection:BridgeConnectionConfig { port:self.bridge.port, token:self.bridge.token.clone(), local_roots:roots, artifacts_dir:self.options.artifacts_dir.as_ref().map(|path|path.to_string_lossy().into_owned()), parallel_pool_width:Some(if width.is_finite() {width.trunc().max(1.0) as u64} else {1}) },
            env:None, session_env:self.options.session_env.clone(), startup_timeout:None, on_message:None,
        }).await?);
        if self.disposed.load(Ordering::SeqCst) { kernel.close().await?; return Err("codemode session manager is disposed".into()); }
        *python = Some(kernel.clone());
        Ok(kernel)
    }

    pub async fn dispose(&self) -> Result<(), String> {
        self.disposed.store(true, Ordering::SeqCst);
        self.dispose_result.get_or_init(|| async {
            let kernel = self.python.lock().await.take();
            let result = if let Some(kernel) = kernel { kernel.close().await } else { Ok(()) };
            self.bridge.close().await;
            result
        }).await.clone()
    }
}

impl SessionManagerLifecycle for CodemodeSessionManager {
    fn dispose(&self) -> SessionDisposeFuture<'_> { Box::pin(self.dispose()) }
}
