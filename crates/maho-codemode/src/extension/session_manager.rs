use super::session_manager_proxy::{SessionDisposeFuture, SessionManagerLifecycle};
use crate::tool::{
    eval_tool_options::EvalKernelManager,
    types::{EvalKernel, EvalKernelFuture, EvalLanguage},
};
use crate::{
    bridge::{
        http_server::{
            BridgeCompletionHandler, BridgeServerHandle, BridgeServerOptions, start_bridge_server,
        },
        protocol::BridgeConnectionConfig,
    },
    bridges::{
        agent_bridge::AgentBridge,
        output_bridge::OutputExecuteTool,
        reserved_dispatch::{ReservedDispatchContext, is_reserved_tool_name, run_reserved_tool},
    },
    config::settings::CodemodeSettings,
    kernels::session_env::SessionEnvironment,
    tool::tool_result_marshal::marshal_tool_result,
};
use maho_ext_api::ExecuteToolOptions;
use serde_json::json;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

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

type SharedRegistrations =
    Arc<std::sync::Mutex<super::kernel_registration::SessionKernelRegistrations>>;
type RawJavaScriptSlot =
    Arc<std::sync::Mutex<Option<Arc<crate::kernels::js::context_manager::JavaScriptKernel>>>>;

pub struct CodemodeSessionManager {
    registrations: SharedRegistrations,
    options: CreateCodemodeSessionManagerOptions,
    bridge: BridgeServerHandle,
    /// The session's JS kernel as cells see it: wrapped by `park_when_idle` when `memory.idleParkMinutes` > 0.
    javascript: tokio::sync::Mutex<Option<Arc<dyn EvalKernel>>>,
    /// The JS kernel process currently backing `javascript`; empty while it is parked.
    javascript_raw: RawJavaScriptSlot,
    disposed: Arc<AtomicBool>,
    environments: Arc<crate::environments::js_environments::JsEnvironments>,
    javascript_shutdown: maho_ai::utils::abort::AbortController,
    dispose_result: tokio::sync::OnceCell<Result<(), String>>,
    contexts: Arc<
        std::sync::Mutex<
            HashMap<String, Arc<crate::tool::eval_tool_options::EvalInvocationContext>>,
        >,
    >,
    session_context:
        Arc<std::sync::Mutex<Option<Arc<crate::tool::eval_tool_options::EvalInvocationContext>>>>,
}

impl CodemodeSessionManager {
    pub async fn start(
        options: CreateCodemodeSessionManagerOptions,
    ) -> Result<Self, std::io::Error> {
        let mut options = options;
        // SAFETY-DIVERGENCE(senpi tools/bash.ts): the overlay is also sent to the kernel as `sessionEnv`.
        if let (Some(passthrough), Some(overlay)) = (
            options.settings.env_passthrough.as_deref(),
            options.session_env.as_mut(),
        ) {
            let mut scrubbed: std::collections::BTreeMap<_, _> = overlay.drain().collect();
            maho_process_env::env_scrub::scrub_secrets(&mut scrubbed, passthrough);
            overlay.extend(scrubbed);
        }
        let mut environment = crate::kernels::session_env::apply_session_environment(
            &std::env::vars().collect(),
            options.session_env.as_ref(),
        );
        // SAFETY-DIVERGENCE(senpi tools/bash.ts): scrub the final eval kernel env after the overlay merge.
        if let Some(passthrough) = options.settings.env_passthrough.as_deref() {
            let mut scrubbed: std::collections::BTreeMap<_, _> = environment.drain().collect();
            maho_process_env::env_scrub::scrub_secrets(&mut scrubbed, passthrough);
            environment.extend(scrubbed);
        }
        let environments = Arc::new(crate::environments::js_environments::JsEnvironments::new(
            crate::environments::js_environments::JsEnvironmentsOptions {
                artifacts_dir: options
                    .artifacts_dir
                    .clone()
                    .unwrap_or_else(|| options.cwd.join(".omo")),
                cwd: options.cwd.clone(),
                runtime: "bun".into(),
                env: environment,
                settings: options.settings.environments.clone(),
            },
        ));
        let bridge_environments = environments.clone();
        let executor = options.executor.clone();
        let list_tools = options.list_tools.clone();
        let task_tools = options.settings.task_tools.clone();
        let agent_bridge = AgentBridge::for_executor(&executor);
        let contexts: Arc<
            std::sync::Mutex<
                HashMap<String, Arc<crate::tool::eval_tool_options::EvalInvocationContext>>,
            >,
        > = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let session_context: Arc<
            std::sync::Mutex<Option<Arc<crate::tool::eval_tool_options::EvalInvocationContext>>>,
        > = Arc::new(std::sync::Mutex::new(None));
        let completion_session_context = session_context.clone();
        let completion_contexts = contexts.clone();
        let complete = options.complete.clone();
        let bridge = start_bridge_server(BridgeServerOptions {
            token: None,
            body_limit_bytes: None,
            on_emit: Arc::new(|_, _| Box::pin(async { Ok(()) })),
            on_completion: Arc::new(move |mut request| {
                request.context = request
                    .cell_id
                    .as_ref()
                    .and_then(|id| {
                        completion_contexts
                            .lock()
                            .expect("invocation contexts")
                            .get(id)
                            .map(|context| context.as_ref().clone())
                    })
                    .or_else(|| {
                        completion_session_context
                            .lock()
                            .expect("session context")
                            .as_ref()
                            .map(|context| context.as_ref().clone())
                    });
                complete(request)
            }),
            on_call: Arc::new(move |request| {
                let executor = executor.clone();
                let list_tools = list_tools.clone();
                let task_tools = task_tools.clone();
                let agent_bridge = agent_bridge.clone();
                let environments = bridge_environments.clone();
                Box::pin(async move {
                    let execute_options = ExecuteToolOptions {
                        signal: Some(request.signal.clone()),
                        ..Default::default()
                    };
                    if is_reserved_tool_name(&request.tool_name) {
                        let tools =
                            if request.tool_name == crate::bridge::reserved::RESERVED_OUTPUT_TOOL {
                                None
                            } else {
                                list_tools
                                    .as_ref()
                                    .map(|list| list())
                                    .transpose()
                                    .map_err(|error| json!({"name":"Error","message":error}))?
                            };
                        run_reserved_tool(
                            &request.tool_name,
                            ReservedDispatchContext {
                                call_id: &request.call_id,
                                args: &request.args,
                                executor: executor.as_ref(),
                                task_tool_name: &task_tools.task,
                                task_output_tool_name: &task_tools.output,
                                tools: tools.as_deref(),
                                execute_options,
                                emit_status: None,
                                agent_bridge: &agent_bridge,
                                environments: Some(environments.as_ref()),
                            },
                        )
                        .await
                        .map_err(|error| {
                            let mut payload = json!({"name":"Error","message":error.to_string()});
                            if let Some(code) = error.code() {
                                payload["code"] = json!(code);
                            }
                            payload
                        })
                    } else {
                        executor
                            .execute_tool(&request.tool_name, request.args, execute_options)
                            .await
                            .map(|result| marshal_tool_result(&result))
                            .map_err(|error| {
                                let code =
                                    crate::bridges::reserved_dispatch::execute_tool_error_code(
                                        &error.code,
                                    );
                                json!({"name":"Error","message":error.to_string(),"code":code})
                            })
                    }
                })
            }),
        })
        .await?;
        Ok(Self {
            registrations: Arc::new(std::sync::Mutex::new(
                super::kernel_registration::SessionKernelRegistrations::new(
                    options.session_id.clone(),
                ),
            )),
            options,
            bridge,
            javascript: tokio::sync::Mutex::new(None),
            javascript_raw: Arc::new(std::sync::Mutex::new(None)),
            disposed: Arc::new(AtomicBool::new(false)),
            environments,
            javascript_shutdown: maho_ai::utils::abort::AbortController::new(),
            dispose_result: tokio::sync::OnceCell::new(),
            contexts,
            session_context,
        })
    }

    pub fn bridge_endpoint(&self) -> Result<(u16, &str), String> {
        if self.disposed.load(Ordering::SeqCst) {
            return Err("codemode session manager is disposed".into());
        }
        Ok((self.bridge.port, &self.bridge.token))
    }

    /// The session's JS kernel process, started on first use; an error while it is parked after idling.
    pub async fn get_javascript_kernel(
        &self,
    ) -> Result<Arc<crate::kernels::js::context_manager::JavaScriptKernel>, String> {
        self.javascript_eval_kernel().await?;
        self.javascript_raw
            .lock()
            .expect("javascript kernel slot")
            .clone()
            .ok_or_else(|| {
                "the js kernel is parked after idling; run a cell to start a fresh kernel".into()
            })
    }

    async fn javascript_eval_kernel(&self) -> Result<Arc<dyn EvalKernel>, String> {
        if self.disposed.load(Ordering::SeqCst) {
            return Err("codemode session manager is disposed".into());
        }
        let mut javascript = self.javascript.lock().await;
        if self.disposed.load(Ordering::SeqCst) {
            return Err("codemode session manager is disposed".into());
        }
        if let Some(kernel) = &*javascript {
            super::session_cwd::assert_session_cwd_available_sync(&self.options.cwd)
                .map_err(|error| error.to_string())?;
            return Ok(kernel.clone());
        }
        super::session_cwd::assert_session_cwd_available(&self.options.cwd)
            .await
            .map_err(|error| error.to_string())?;
        let launcher = self.launcher();
        let kernel = launcher.start().await?;
        if self.disposed.load(Ordering::SeqCst) {
            kernel.close().await.map_err(|error| error.to_string())?;
            return Err("codemode session manager is disposed".into());
        }
        let idle_park_minutes = self
            .options
            .settings
            .memory()
            .idle_park_minutes
            .unwrap_or(0.0);
        let restart: super::idle_parking_kernel::RestartParkedKernel = {
            let registrations = self.registrations.clone();
            let disposed = self.disposed.clone();
            let raw = self.javascript_raw.clone();
            Arc::new(move || -> EvalKernelFuture<'static, Arc<dyn EvalKernel>> {
                let launcher = launcher.clone();
                let registrations = registrations.clone();
                let disposed = disposed.clone();
                let raw = raw.clone();
                Box::pin(async move {
                    let restarted = launcher.start().await?;
                    if disposed.load(Ordering::SeqCst) {
                        let _ = restarted.close().await;
                        return Err("codemode session manager is disposed".into());
                    }
                    register_javascript(&registrations, &restarted);
                    *raw.lock().expect("javascript kernel slot") = Some(restarted.clone());
                    Ok(restarted as Arc<dyn EvalKernel>)
                })
            })
        };
        let on_parked: super::idle_parking_kernel::OnParked = {
            let registrations = self.registrations.clone();
            let raw = self.javascript_raw.clone();
            Arc::new(move || {
                registrations
                    .lock()
                    .expect("session registrations lock")
                    .unregister(EvalLanguage::Js);
                *raw.lock().expect("javascript kernel slot") = None;
            })
        };
        let resolved = super::idle_parking_kernel::park_when_idle(
            EvalLanguage::Js,
            idle_park_minutes,
            kernel.clone(),
            restart,
            Some(on_parked),
        );
        register_javascript(&self.registrations, &kernel);
        *self.javascript_raw.lock().expect("javascript kernel slot") = Some(kernel);
        *javascript = Some(resolved.clone());
        // Retain ownership if cwd vanished during startup so disposal still closes it.
        super::session_cwd::assert_session_cwd_available(&self.options.cwd)
            .await
            .map_err(|error| error.to_string())?;
        if self.disposed.load(Ordering::SeqCst) {
            return Err("codemode session manager is disposed".into());
        }
        Ok(resolved)
    }

    fn launcher(&self) -> JavaScriptLauncher {
        JavaScriptLauncher {
            cwd: self.options.cwd.clone(),
            session_id: self.options.session_id.clone(),
            local_roots: self.options.local_roots.clone(),
            artifacts_dir: self.options.artifacts_dir.clone(),
            settings: self.options.settings.clone(),
            list_tools: self.options.list_tools.clone(),
            session_env: self.options.session_env.clone(),
            port: self.bridge.port,
            token: self.bridge.token.clone(),
            shutdown: self.javascript_shutdown.clone(),
        }
    }

    pub async fn dispose(&self) -> Result<(), String> {
        self.disposed.store(true, Ordering::SeqCst);
        self.javascript_shutdown.abort(None);
        self.dispose_result
            .get_or_init(|| async {
                let mut failures = Vec::new();
                let kernel = self.javascript.lock().await.take();
                self.javascript_raw
                    .lock()
                    .expect("javascript kernel slot")
                    .take();
                self.registrations
                    .lock()
                    .expect("session registrations lock")
                    .clear();
                if let Some(kernel) = kernel
                    && let Err(error) = kernel.close().await
                {
                    failures.push(error.to_string());
                }
                self.bridge.close().await;
                if failures.is_empty() {
                    Ok(())
                } else {
                    Err(format!(
                        "Failed to dispose codemode session manager: {}",
                        failures.join("; ")
                    ))
                }
            })
            .await
            .clone()
    }
}

impl SessionManagerLifecycle for CodemodeSessionManager {
    fn dispose(&self) -> SessionDisposeFuture<'_> {
        Box::pin(self.dispose())
    }
    fn set_context(&self, context: crate::tool::eval_tool_options::EvalInvocationContext) {
        *self.session_context.lock().expect("session context") = Some(Arc::new(context));
    }
}

impl EvalKernelManager for CodemodeSessionManager {
    fn js_environments(&self) -> Option<Arc<crate::environments::js_environments::JsEnvironments>> {
        Some(self.environments.clone())
    }
    fn set_invocation_context(
        &self,
        cell_id: &str,
        context: crate::tool::eval_tool_options::EvalInvocationContext,
    ) -> Option<Box<dyn FnOnce() + Send>> {
        let context = Arc::new(context);
        self.contexts
            .lock()
            .expect("invocation contexts")
            .insert(cell_id.into(), context.clone());
        let contexts = self.contexts.clone();
        let cell_id = cell_id.to_owned();
        Some(Box::new(move || {
            let mut contexts = contexts.lock().expect("invocation contexts");
            if contexts
                .get(&cell_id)
                .is_some_and(|current| Arc::ptr_eq(current, &context))
            {
                contexts.remove(&cell_id);
            }
        }))
    }
    fn get_kernel(&self, language: EvalLanguage) -> EvalKernelFuture<'_, Arc<dyn EvalKernel>> {
        Box::pin(async move {
            match language {
                EvalLanguage::Js => self.javascript_eval_kernel().await,
            }
        })
    }
}

fn register_javascript(
    registrations: &SharedRegistrations,
    kernel: &Arc<crate::kernels::js::context_manager::JavaScriptKernel>,
) {
    registrations
        .lock()
        .expect("session registrations lock")
        .register(
            EvalLanguage::Js,
            Some(super::kernel_registration::java_script_kernel_memory(
                kernel.clone(),
            )),
        );
}

/// Everything a JS kernel start needs, cloneable so an idle-parked kernel can restart without the manager.
#[derive(Clone)]
struct JavaScriptLauncher {
    cwd: PathBuf,
    session_id: String,
    local_roots: Option<crate::bridge::protocol::LocalRoots>,
    artifacts_dir: Option<PathBuf>,
    settings: CodemodeSettings,
    list_tools: Option<crate::bridges::schema_bridge::EvalToolCatalog>,
    session_env: Option<SessionEnvironment>,
    port: u16,
    token: String,
    shutdown: maho_ai::utils::abort::AbortController,
}

impl JavaScriptLauncher {
    async fn start(
        &self,
    ) -> Result<Arc<crate::kernels::js::context_manager::JavaScriptKernel>, String> {
        let roots = self.local_roots.clone().or_else(|| {
            self.artifacts_dir.as_ref().map(|root| {
                crate::bridge::protocol::LocalRoots::from([(
                    "local".into(),
                    root.join("local").to_string_lossy().into_owned(),
                )])
            })
        });
        let width = self.settings.parallel_pool_width;
        let catalog = self.list_tools.clone();
        let names = Arc::new(move || {
            let host = catalog
                .as_ref()
                .map(|catalog| catalog())
                .transpose()?
                .unwrap_or_default()
                .into_iter()
                .map(|tool| tool.name)
                .collect();
            Ok((host, vec![]))
        });
        let settings = &self.settings;
        let memory = crate::config::memory_settings::resolve_kernel_memory_thresholds(
            Some(&settings.memory()),
            &maho_ext_api::process_env::vars().into_iter().collect(),
        );
        Ok(Arc::new(
            crate::kernels::js::context_manager::JavaScriptKernel::start_with_memory(
                &self.cwd,
                &self.session_id,
                crate::kernels::js::context_manager::JavaScriptKernelStart {
                    isolation: crate::config::feature_settings::resolve_js_isolation(
                        settings,
                        &maho_ext_api::process_env::vars().into_iter().collect(),
                    ),
                    parallel_pool_width: if width.is_finite() {
                        width.trunc().max(1.0) as u64
                    } else {
                        1
                    },
                    session_env: self.session_env.clone(),
                    connection: BridgeConnectionConfig {
                        port: self.port,
                        token: self.token.clone(),
                        local_roots: roots,
                        artifacts_dir: self
                            .artifacts_dir
                            .as_ref()
                            .map(|path| path.to_string_lossy().into_owned()),
                        parallel_pool_width: None,
                    },
                    names,
                    shutdown: self.shutdown.clone(),
                    memory,
                    env_passthrough: settings.env_passthrough.clone(),
                },
            )
            .await
            .map_err(|error| error.to_string())?,
        ))
    }
}
