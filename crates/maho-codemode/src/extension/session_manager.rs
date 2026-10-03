use std::{collections::HashMap, path::PathBuf, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use serde_json::json;
use maho_ext_api::ExecuteToolOptions;
use crate::{bridge::{http_server::{BridgeServerHandle, BridgeServerOptions, BridgeCompletionHandler, start_bridge_server}, protocol::BridgeConnectionConfig}, bridges::{agent_bridge::AgentBridge, output_bridge::OutputExecuteTool, reserved_dispatch::{ReservedDispatchContext, is_reserved_tool_name, run_reserved_tool}}, config::settings::CodemodeSettings, kernels::{py::{kernel::PythonKernel, kernel_contract::PythonKernelStartOptions}, session_env::SessionEnvironment}, tool::tool_result_marshal::marshal_tool_result};
use super::session_manager_proxy::{SessionManagerLifecycle, SessionDisposeFuture};
use crate::{interpreters::detect::{InterpreterAvailability,InterpreterDetection},tool::{types::{EvalLanguage,EvalKernel,EvalKernelFuture},eval_tool_options::EvalKernelManager},kernels::{shared::{subprocess_kernel::SubprocessKernel,subprocess_contract::SubprocessKernelOptions},rb::kernel::RubyKernel,jl::kernel::JuliaKernel}};

pub struct CreateCodemodeSessionManagerOptions {
    pub session_id: String,
    pub cwd: PathBuf,
    pub settings: CodemodeSettings,
    pub availability: InterpreterAvailability,
    pub local_roots: Option<HashMap<String, String>>,
    pub artifacts_dir: Option<PathBuf>,
    pub session_env: Option<SessionEnvironment>,
    pub executor: Arc<dyn OutputExecuteTool>,
    pub list_tools: Option<crate::bridges::schema_bridge::EvalToolCatalog>,
    pub complete: BridgeCompletionHandler,
}

pub struct CodemodeSessionManager {
    options: CreateCodemodeSessionManagerOptions,
    bridge: BridgeServerHandle,
    python: tokio::sync::Mutex<Option<Arc<PythonKernel>>>,
    javascript: tokio::sync::Mutex<Option<Arc<crate::kernels::js::context_manager::JavaScriptKernel>>>,
    subprocesses: tokio::sync::Mutex<HashMap<EvalLanguage,Arc<SubprocessKernel>>>,
    disposed: AtomicBool,
    dispose_result: tokio::sync::OnceCell<Result<(), String>>,
    contexts: Arc<std::sync::Mutex<HashMap<String,Arc<crate::tool::eval_tool_options::EvalInvocationContext>>>>,
}

impl CodemodeSessionManager {
    pub async fn start(options: CreateCodemodeSessionManagerOptions) -> Result<Self, std::io::Error> {
        let executor = options.executor.clone();
        let list_tools = options.list_tools.clone();
        let task_tools = options.settings.task_tools.clone();
        let agent_bridge = AgentBridge::for_executor(&executor);
        let contexts: Arc<std::sync::Mutex<HashMap<String,Arc<crate::tool::eval_tool_options::EvalInvocationContext>>>>=Arc::new(std::sync::Mutex::new(HashMap::new()));
        let completion_contexts=contexts.clone();let complete=options.complete.clone();
        let bridge = start_bridge_server(BridgeServerOptions {
            token: None, body_limit_bytes: None,
            on_emit: Arc::new(|_, _| Box::pin(async { Ok(()) })),
            on_completion: Arc::new(move |mut request| {
                request.context=request.cell_id.as_ref().and_then(|id|completion_contexts.lock().expect("invocation contexts").get(id).map(|context|context.as_ref().clone()));
                complete(request)
            }),
            on_call: Arc::new(move |request| {
                let executor = executor.clone();
                let list_tools = list_tools.clone();
                let task_tools = task_tools.clone();
                let agent_bridge = agent_bridge.clone();
                Box::pin(async move {
                    let execute_options = ExecuteToolOptions { signal: Some(request.signal), ..Default::default() };
                    if is_reserved_tool_name(&request.tool_name) {
                        let tools = if request.tool_name==crate::bridge::reserved::RESERVED_OUTPUT_TOOL {None} else {list_tools.as_ref().map(|list| list()).transpose().map_err(|error| json!({"name":"Error","message":error}))?};
                        run_reserved_tool(&request.tool_name, ReservedDispatchContext {
                            call_id: &request.call_id, args: &request.args, executor: executor.as_ref(),
                            task_tool_name: &task_tools.task, task_output_tool_name: &task_tools.output,
                            tools: tools.as_deref(), execute_options, emit_status: None, agent_bridge: &agent_bridge,
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
        Ok(Self { options, bridge, python:tokio::sync::Mutex::new(None), javascript:tokio::sync::Mutex::new(None), subprocesses:tokio::sync::Mutex::new(HashMap::new()), disposed:AtomicBool::new(false), dispose_result:tokio::sync::OnceCell::new(),contexts })
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

    pub async fn get_javascript_kernel(&self)->Result<Arc<crate::kernels::js::context_manager::JavaScriptKernel>,String> {
        if self.disposed.load(Ordering::SeqCst) {return Err("codemode session manager is disposed".into());}
        let mut javascript=self.javascript.lock().await;
        if self.disposed.load(Ordering::SeqCst) {return Err("codemode session manager is disposed".into());}
        if let Some(kernel)=&*javascript {return Ok(kernel.clone());}
        let roots=self.options.local_roots.clone().or_else(||self.options.artifacts_dir.as_ref().map(|root|HashMap::from([("local".into(),root.join("local").to_string_lossy().into_owned())])));
        let width=self.options.settings.parallel_pool_width;
        let catalog=self.options.list_tools.clone();
        let names=Arc::new(move || {let host=catalog.as_ref().map(|catalog|catalog()).transpose()?.unwrap_or_default().into_iter().map(|tool|tool.name).collect();Ok((host,vec![]))});
        let kernel=Arc::new(crate::kernels::js::context_manager::JavaScriptKernel::start_with_names(&self.options.cwd,&self.options.session_id,if width.is_finite() {width.trunc().max(1.0) as u64} else {1},self.options.session_env.clone(),BridgeConnectionConfig {port:self.bridge.port,token:self.bridge.token.clone(),local_roots:roots,artifacts_dir:self.options.artifacts_dir.as_ref().map(|path|path.to_string_lossy().into_owned()),parallel_pool_width:None},names).await.map_err(|error|error.to_string())?);
        if self.disposed.load(Ordering::SeqCst) {kernel.close().await.map_err(|error|error.to_string())?;return Err("codemode session manager is disposed".into());}
        *javascript=Some(kernel.clone());Ok(kernel)
    }

    pub async fn get_subprocess_kernel(&self, language: EvalLanguage) -> Result<Arc<SubprocessKernel>,String> {
        if self.disposed.load(Ordering::SeqCst) {return Err("codemode session manager is disposed".into());}
        let command=self.interpreter_path(language)?;
        let mut kernels=self.subprocesses.lock().await;
        if self.disposed.load(Ordering::SeqCst) {return Err("codemode session manager is disposed".into());}
        if let Some(kernel)=kernels.get(&language) {return Ok(kernel.clone());}
        let roots=self.options.local_roots.clone().or_else(||self.options.artifacts_dir.as_ref().map(|root|HashMap::from([("local".into(),root.join("local").to_string_lossy().into_owned())])));
        let width=self.options.settings.parallel_pool_width;
        let options=SubprocessKernelOptions {command,args:vec![],cwd:self.options.cwd.clone(),env:None,session_env:self.options.session_env.clone(),on_message:None,session_id:self.options.session_id.clone(),connection:BridgeConnectionConfig {port:self.bridge.port,token:self.bridge.token.clone(),local_roots:roots,artifacts_dir:self.options.artifacts_dir.as_ref().map(|path|path.to_string_lossy().into_owned()),parallel_pool_width:Some(if width.is_finite() {width.trunc().max(1.0) as u64} else {1})}};
        let kernel=Arc::new(match language {
            EvalLanguage::Rb=>RubyKernel::start(options).await,
            EvalLanguage::Jl=>JuliaKernel::start(options).await,
            EvalLanguage::Py|EvalLanguage::Js=>return Err("language does not use the shared subprocess registry".into()),
        }.map_err(|error|error.to_string())?);
        if self.disposed.load(Ordering::SeqCst) {kernel.close().await.map_err(|error|error.to_string())?;return Err("codemode session manager is disposed".into());}
        kernels.insert(language,kernel.clone());Ok(kernel)
    }

    fn interpreter_path(&self,language:EvalLanguage)->Result<String,String> {
        let name=serde_json::to_value(language).expect("language serialization");
        match self.options.availability.iter().find(|(candidate,_)|*candidate==language).map(|(_,status)|&status.detected) {
            Some(InterpreterDetection::Detected {path,..})=>Ok(path.clone()),
            _=>Err(format!("No {} interpreter is available",name.as_str().expect("language string"))),
        }
    }

    pub async fn dispose(&self) -> Result<(), String> {
        self.disposed.store(true, Ordering::SeqCst);
        self.dispose_result.get_or_init(|| async {
            let kernel = self.python.lock().await.take();
            let result = if let Some(kernel) = kernel { kernel.close().await } else { Ok(()) };
            let kernels=std::mem::take(&mut *self.subprocesses.lock().await);
            let mut failures=Vec::new();
            if let Some(kernel)=self.javascript.lock().await.take() && let Err(error)=kernel.close().await {failures.push(error.to_string());}
            if let Err(error)=result {failures.push(error);}
            for kernel in kernels.into_values() {if let Err(error)=kernel.close().await {failures.push(error.to_string());}}
            self.bridge.close().await;
            if failures.is_empty() {Ok(())} else {Err(format!("Failed to dispose codemode session manager: {}",failures.join("; ")))}
        }).await.clone()
    }
}

impl SessionManagerLifecycle for CodemodeSessionManager {
    fn dispose(&self) -> SessionDisposeFuture<'_> { Box::pin(self.dispose()) }
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
        Box::pin(async move {match language {
            EvalLanguage::Py=>Ok(self.get_python_kernel(&self.interpreter_path(language)?).await? as Arc<dyn EvalKernel>),
            EvalLanguage::Rb|EvalLanguage::Jl=>Ok(self.get_subprocess_kernel(language).await? as Arc<dyn EvalKernel>),
            EvalLanguage::Js=>Ok(self.get_javascript_kernel().await? as Arc<dyn EvalKernel>),
        }})
    }
}
