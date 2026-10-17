use std::{collections::HashMap, path::PathBuf, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use serde_json::json;
use maho_ext_api::ExecuteToolOptions;
use crate::{bridge::{http_server::{BridgeServerHandle, BridgeServerOptions, BridgeCompletionHandler, start_bridge_server}, protocol::BridgeConnectionConfig}, bridges::{agent_bridge::AgentBridge, output_bridge::OutputExecuteTool, reserved_dispatch::{ReservedDispatchContext, is_reserved_tool_name, run_reserved_tool}}, config::settings::CodemodeSettings, kernels::session_env::SessionEnvironment, tool::tool_result_marshal::marshal_tool_result};
use super::session_manager_proxy::{SessionManagerLifecycle, SessionDisposeFuture};
use crate::tool::{types::{EvalLanguage,EvalKernel,EvalKernelFuture},eval_tool_options::EvalKernelManager};

pub struct CreateCodemodeSessionManagerOptions {
    pub session_id: String,
    pub cwd: PathBuf,
    pub settings: CodemodeSettings,
    pub local_roots: Option<crate::bridge::protocol::LocalRoots>,
    pub artifacts_dir: Option<PathBuf>,
    pub session_env: Option<SessionEnvironment>,
    pub executor: Arc<dyn OutputExecuteTool>,
    pub list_tools: Option<crate::bridges::schema_bridge::EvalToolCatalog>,
    pub complete: BridgeCompletionHandler,
}

pub struct CodemodeSessionManager {
        options: CreateCodemodeSessionManagerOptions,
    bridge: BridgeServerHandle,
    javascript: tokio::sync::Mutex<Option<Arc<crate::kernels::js::context_manager::JavaScriptKernel>>>,
    disposed: AtomicBool,
    javascript_shutdown: maho_ai::utils::abort::AbortController,
    dispose_result: tokio::sync::OnceCell<Result<(), String>>,
    contexts: Arc<std::sync::Mutex<HashMap<String,Arc<crate::tool::eval_tool_options::EvalInvocationContext>>>>,
    session_context: Arc<std::sync::Mutex<Option<Arc<crate::tool::eval_tool_options::EvalInvocationContext>>>>,
}

impl CodemodeSessionManager {
    pub async fn start(options: CreateCodemodeSessionManagerOptions) -> Result<Self, std::io::Error> {
        let executor = options.executor.clone();
        let list_tools = options.list_tools.clone();
        let task_tools = options.settings.task_tools.clone();
        let agent_bridge = AgentBridge::for_executor(&executor);
        let contexts: Arc<std::sync::Mutex<HashMap<String,Arc<crate::tool::eval_tool_options::EvalInvocationContext>>>>=Arc::new(std::sync::Mutex::new(HashMap::new()));
        let session_context:Arc<std::sync::Mutex<Option<Arc<crate::tool::eval_tool_options::EvalInvocationContext>>>>=Arc::new(std::sync::Mutex::new(None));
        let completion_session_context=session_context.clone();
        let completion_contexts=contexts.clone();let complete=options.complete.clone();
        let bridge = start_bridge_server(BridgeServerOptions {
            token: None, body_limit_bytes: None,
            on_emit: Arc::new(|_, _| Box::pin(async { Ok(()) })),
            on_completion: Arc::new(move |mut request| {
                request.context=request.cell_id.as_ref().and_then(|id|completion_contexts.lock().expect("invocation contexts").get(id).map(|context|context.as_ref().clone())).or_else(||completion_session_context.lock().expect("session context").as_ref().map(|context|context.as_ref().clone()));
                complete(request)
            }),
            on_call: Arc::new(move |request| {
                let executor = executor.clone();
                let list_tools = list_tools.clone();
                let task_tools = task_tools.clone();
                let agent_bridge = agent_bridge.clone();
                Box::pin(async move {
                let execute_options = ExecuteToolOptions { signal: Some(request.signal.clone()), ..Default::default() };
                    if is_reserved_tool_name(&request.tool_name) {
                        let tools = if request.tool_name==crate::bridge::reserved::RESERVED_OUTPUT_TOOL {None} else {list_tools.as_ref().map(|list| list()).transpose().map_err(|error| json!({"name":"Error","message":error}))?};
                        run_reserved_tool(&request.tool_name, ReservedDispatchContext {
                            call_id: &request.call_id, args: &request.args, executor: executor.as_ref(),
                            task_tool_name: &task_tools.task, task_output_tool_name: &task_tools.output,
                            tools: tools.as_deref(), execute_options, emit_status: None, agent_bridge: &agent_bridge, environments: None,
                        }).await.map_err(|error| {
                            let mut payload=json!({"name":"Error","message":error.to_string()});
                            if let Some(code)=error.code() {payload["code"]=json!(code);}
                            payload
                        })
                    } else {
                        executor.execute_tool(&request.tool_name, request.args, execute_options).await
                            .map(|result| marshal_tool_result(&result))
                            .map_err(|error| {
                                let code=crate::bridges::reserved_dispatch::execute_tool_error_code(&error.code);
                                json!({"name":"Error","message":error.to_string(),"code":code})
                            })
                    }
                })
            }),
        }).await?;
        Ok(Self { options, bridge, javascript:tokio::sync::Mutex::new(None), disposed:AtomicBool::new(false), javascript_shutdown:maho_ai::utils::abort::AbortController::new(),dispose_result:tokio::sync::OnceCell::new(),contexts,session_context })
    }

    pub fn bridge_endpoint(&self) -> Result<(u16, &str), String> {
        if self.disposed.load(Ordering::SeqCst) { return Err("codemode session manager is disposed".into()); }
        Ok((self.bridge.port, &self.bridge.token))
    }

    pub async fn get_javascript_kernel(&self)->Result<Arc<crate::kernels::js::context_manager::JavaScriptKernel>,String> {
        if self.disposed.load(Ordering::SeqCst) {return Err("codemode session manager is disposed".into());}
        let mut javascript=self.javascript.lock().await;
        if self.disposed.load(Ordering::SeqCst) {return Err("codemode session manager is disposed".into());}
        if let Some(kernel)=&*javascript {return Ok(kernel.clone());}
        let roots=self.options.local_roots.clone().or_else(||self.options.artifacts_dir.as_ref().map(|root|crate::bridge::protocol::LocalRoots::from([("local".into(),root.join("local").to_string_lossy().into_owned())])));
        let width=self.options.settings.parallel_pool_width;
        let catalog=self.options.list_tools.clone();
        let names=Arc::new(move || {let host=catalog.as_ref().map(|catalog|catalog()).transpose()?.unwrap_or_default().into_iter().map(|tool|tool.name).collect();Ok((host,vec![]))});
        let settings=&self.options.settings;
        let memory=crate::bridge::protocol::KernelMemoryThresholds {gc_watermark_bytes:settings.memory_gc_watermark_bytes as u64,notice_bytes:settings.memory_notice_bytes as u64,ceiling_bytes:settings.memory_ceiling_bytes as u64};
        let kernel=Arc::new(crate::kernels::js::context_manager::JavaScriptKernel::start_with_memory(&self.options.cwd,&self.options.session_id,crate::kernels::js::context_manager::JavaScriptKernelStart {parallel_pool_width:if width.is_finite() {width.trunc().max(1.0) as u64} else {1},session_env:self.options.session_env.clone(),connection:BridgeConnectionConfig {port:self.bridge.port,token:self.bridge.token.clone(),local_roots:roots,artifacts_dir:self.options.artifacts_dir.as_ref().map(|path|path.to_string_lossy().into_owned()),parallel_pool_width:None},names,shutdown:self.javascript_shutdown.clone(),memory}).await.map_err(|error|error.to_string())?);
        if self.disposed.load(Ordering::SeqCst) {kernel.close().await.map_err(|error|error.to_string())?;return Err("codemode session manager is disposed".into());}
        *javascript=Some(kernel.clone());Ok(kernel)
    }

    pub async fn dispose(&self) -> Result<(), String> {
        self.disposed.store(true, Ordering::SeqCst);
        self.javascript_shutdown.abort(None);
        self.dispose_result.get_or_init(|| async {
            let mut failures=Vec::new();
            if let Some(kernel)=self.javascript.lock().await.take() && let Err(error)=kernel.close().await {failures.push(error.to_string());}
            self.bridge.close().await;
            if failures.is_empty() {Ok(())} else {Err(format!("Failed to dispose codemode session manager: {}",failures.join("; ")))}
        }).await.clone()
    }
}

impl SessionManagerLifecycle for CodemodeSessionManager {
    fn dispose(&self) -> SessionDisposeFuture<'_> { Box::pin(self.dispose()) }
    fn set_context(&self, context: crate::tool::eval_tool_options::EvalInvocationContext) {
        *self.session_context.lock().expect("session context")=Some(Arc::new(context));
    }
}

impl EvalKernelManager for CodemodeSessionManager {
    fn set_invocation_context(&self,cell_id:&str,context:crate::tool::eval_tool_options::EvalInvocationContext) -> Option<Box<dyn FnOnce()+Send>> {
        let context=Arc::new(context);
        self.contexts.lock().expect("invocation contexts").insert(cell_id.into(),context.clone());
        let contexts=self.contexts.clone();let cell_id=cell_id.to_owned();
        Some(Box::new(move || {
            let mut contexts=contexts.lock().expect("invocation contexts");
            if contexts.get(&cell_id).is_some_and(|current|Arc::ptr_eq(current,&context)) {contexts.remove(&cell_id);}
        }))
    }
    fn get_kernel(&self,language:EvalLanguage)->EvalKernelFuture<'_,Arc<dyn EvalKernel>> {
        Box::pin(async move {match language { EvalLanguage::Js=>Ok(self.get_javascript_kernel().await? as Arc<dyn EvalKernel>) }})
    }
}
