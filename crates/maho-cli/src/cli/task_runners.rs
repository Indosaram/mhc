//! Native executable/profile configuration shared by task launch and RPC respawn.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use senpi_task::runners::rpc::{
    model_admission::{RpcModelAdmissionOptions, create_rpc_model_admission},
    process::RpcSpawnDescriptor,
    spawn::{RpcSpawnRuntime, build_rpc_model_catalog_spawn, build_rpc_spawn},
};
use senpi_task::runners::rpc_process::RpcProcessRunnerOptions;

pub struct NativeChildModelRegistry(pub maho_core::model_registry::ModelRegistry);

pub fn resolved_omo_config(
    cwd: &std::path::Path,
    env: &BTreeMap<String, String>,
) -> serde_json::Value {
    maho_omo_config_resolution::load_senpi_omo_config(omo_config_core::LoadOmoConfigOptions {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        env: Some(env.clone()),
        ..Default::default()
    })
    .config
}

impl senpi_task::manager::parent_registry_context::ChildModelRegistry for NativeChildModelRegistry {
    fn find(
        &self,
        provider: &str,
        model_id: &str,
    ) -> Option<senpi_task::runners::in_process::child_options::HostHandle> {
        self.0.find(provider, model_id).map(|model| {
            Arc::new(model) as senpi_task::runners::in_process::child_options::HostHandle
        })
    }

    fn auth_storage(&self) -> senpi_task::runners::in_process::child_options::HostHandle {
        self.0.auth_storage.clone()
    }

    fn model_runtime(&self) -> Option<senpi_task::runners::in_process::child_options::HostHandle> {
        Some(Arc::new(self.0.model_runtime.clone()))
    }
}

pub fn live_parent_registry(
    parent: Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>,
) -> senpi_task::manager::parent_registry_context::ParentModelRegistryResolver {
    Arc::new(move || {
        parent().map(|session| {
            Arc::new(NativeChildModelRegistry(session.model_registry().clone()))
                as Arc<dyn senpi_task::manager::parent_registry_context::ChildModelRegistry>
        })
    })
}

pub fn native_child_session_manager(
    child: &senpi_task::runners::in_process::child_options::ChildSessionOptions,
) -> Result<maho_core::session_manager::SessionManager, senpi_task::host::HostError> {
    maho_core::session_manager::SessionManager::open(
        &child.session_manager.session_file().to_string_lossy(),
        Some(child.session_manager.session_dir()),
        Some(&child.cwd),
        None,
    )
    .map_err(|message| senpi_task::host::HostError { message })
}

pub(crate) fn workpool_yield_tool(
    identity: &senpi_task::runners::in_process::child_options::WorkpoolChildIdentity,
    cwd: &str,
) -> maho_ext_api::ToolDefinition {
    workpool_yield_tool_with_events(identity, cwd, None)
}

pub type WorkpoolEvents = Arc<dyn Fn(senpi_task::workpool::types::WorkpoolEvent) + Send + Sync>;

pub(crate) fn workpool_yield_tool_with_events(
    identity: &senpi_task::runners::in_process::child_options::WorkpoolChildIdentity,
    cwd: &str,
    emit: Option<WorkpoolEvents>,
) -> maho_ext_api::ToolDefinition {
    let store = senpi_task::workpool::store::WorkpoolStore::new(&identity.state_dir);
    let tasks = senpi_task::store::TaskRecordStore::new(&senpi_task::store::StateDirConfig {
        project_dir: cwd.into(),
        task_state_dir: Some(identity.state_dir.clone().into()),
    });
    let yield_result = Arc::new(
        senpi_task::workpool::worker_capability::create_workpool_yield_capability(
            store,
            tasks,
            identity.task_id.clone(),
            identity.run_epoch,
            emit.map(|emit| {
                Box::new(move |event| emit(event))
                    as senpi_task::workpool::worker_capability::WorkpoolEventSink
            }),
        ),
    );
    maho_ext_api::ToolDefinition::new(
        "workpool",
        "Yield keyed results for this worker's current assignment.",
        serde_json::json!({"type":"object","additionalProperties":false,"required":["op","results"],"properties":{"op":{"const":"yield"},"results":{"type":"array","items":{}}}}),
        Arc::new(move |call| {
            let yield_result = yield_result.clone();
            Box::pin(async move {
                let (pool_id, generation, receipts) = yield_result(call.params)
                    .map_err(|error| maho_tools::ToolError::Message(error.to_string()))?;
                let results = receipts.into_iter().map(|receipt| match receipt {
                        senpi_task::workpool::worker_capability::WorkpoolYieldReceipt::Accepted { key, item_id } => serde_json::json!({"key":key,"item_id":item_id,"accepted":true}),
                        senpi_task::workpool::worker_capability::WorkpoolYieldReceipt::Duplicate { key, item_id } => serde_json::json!({"key":key,"item_id":item_id,"accepted":true,"duplicate":true}),
                        senpi_task::workpool::worker_capability::WorkpoolYieldReceipt::Refused(error) => serde_json::json!({"accepted":false,"code":error.code,"message":error.message,"key":error.key}),
                    }).collect::<Vec<_>>();
                let result = serde_json::json!({"pool_id":pool_id,"generation":generation,"results":results});
                Ok(maho_ext_api::ToolResult {
                    content: vec![maho_ext_api::ToolContent::text(result.to_string())],
                    details: Some(result),
                    structured_content: None,
                    is_error: None,
                })
            })
        }),
    )
}

struct NativeParentTool {
    name: String,
    description: String,
    parent: Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>,
    executor: tokio::runtime::Handle,
}

pub fn native_child_sdk_options(
    child: &senpi_task::runners::in_process::child_options::ChildSessionOptions,
    mut custom_tools: Vec<maho_ext_api::ToolDefinition>,
) -> Result<maho_core::sdk::CreateAgentSessionOptions, senpi_task::host::HostError> {
    let failure = |message: String| senpi_task::host::HostError { message };
    if custom_tools.len() != child.custom_tools.len()
        || custom_tools
            .iter()
            .zip(&child.custom_tools)
            .any(|(native, child)| native.name != child.name())
    {
        return Err(failure(
            "Native child definitions must preserve the imported custom tool inventory and order"
                .into(),
        ));
    }
    if let Some(identity) = &child.workpool_identity {
        custom_tools.push(workpool_yield_tool(identity, &child.cwd));
    }
    let auth_storage = child
        .auth_storage
        .clone()
        .map(|handle| {
            handle
                .downcast::<maho_core::auth_storage::AuthStorage>()
                .map_err(|_| failure("Child auth storage is not native AuthStorage".into()))
        })
        .transpose()?;
    let model_runtime = child
        .model_runtime
        .as_ref()
        .map(|handle| {
            handle
                .downcast_ref::<maho_core::model_runtime::ModelRuntime>()
                .cloned()
                .ok_or_else(|| failure("Child model runtime is not native ModelRuntime".into()))
        })
        .transpose()?;
    let model_registry = child
        .model_registry
        .as_ref()
        .map(|handle| {
            handle
                .downcast_ref::<NativeChildModelRegistry>()
                .map(|registry| registry.0.clone())
                .ok_or_else(|| {
                    failure("Child model registry is not the native parent facade".into())
                })
        })
        .transpose()?;
    let model = child
        .model
        .as_ref()
        .map(|handle| {
            handle
                .downcast_ref::<maho_ai::types::Model>()
                .cloned()
                .ok_or_else(|| failure("Child model is not native Model".into()))
        })
        .transpose()?;
    let thinking_selection = child
        .thinking_level
        .as_deref()
        .map(|level| {
            maho_ai::types::ModelThinkingLevel::parse(level)
                .map(|level| maho_ai::types::ThinkingSelection {
                    level,
                    source: maho_ai::types::ThinkingSelectionSource::Explicit,
                    legacy_variant_id: None,
                })
                .ok_or_else(|| failure(format!("Invalid child thinking level: {level}")))
        })
        .transpose()?;
    let minimal_resources = match child.resource_loader {
        senpi_task::runners::in_process::child_options::ChildResourceLoader::Minimal => true,
    };
    Ok(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(child.cwd.clone()),
        agent_dir: child.agent_dir.clone(),
        auth_storage,
        model_runtime,
        model_registry,
        model,
        thinking_selection,
        custom_tools,
        minimal_resources,
        session_manager: Some(native_child_session_manager(child)?),
        settings_manager: Some(native_child_settings(&child.settings)),
        tools: child.tools.clone(),
        exclude_tools: child.exclude_tools.clone(),
        system_prompt: child.system_prompt.clone(),
        ..Default::default()
    })
}

pub fn native_child_settings(
    retry: &senpi_task::runners::in_process::runtime_fallback_settings::RetryFallbackSettings,
) -> maho_core::settings_manager::SettingsManager {
    let mut settings = maho_core::settings_manager::SettingsManager::from_storage(
        Box::<maho_core::settings_manager::InMemorySettingsStorage>::default(),
        false,
    );
    let mut retry_block = serde_json::Map::new();
    retry_block.insert(
        "modelFallback".into(),
        serde_json::Value::Bool(retry.model_fallback),
    );
    retry_block.insert("fallbackChains".into(), serde_json::json!(retry.chains));
    // `maxRetries` and `baseDelayMs` are INDEPENDENT optionals: each is written only when the child
    // set it, and `Some(0)` is preserved (0 is a real budget, not "absent").
    if let Some(max_retries) = retry.max_retries {
        retry_block.insert("maxRetries".into(), serde_json::Value::from(max_retries));
    }
    if let Some(base_delay_ms) = retry.base_delay_ms {
        retry_block.insert("baseDelayMs".into(), serde_json::Value::from(base_delay_ms));
    }
    settings.apply_overrides(&serde_json::Map::from_iter([(
        "retry".into(),
        serde_json::Value::Object(retry_block),
    )]));
    settings
}

pub fn native_shared_parent_tool_definition(
    name: &str,
    parent: Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>,
) -> Result<maho_ext_api::ToolDefinition, senpi_task::host::HostError> {
    let session = parent().ok_or_else(|| senpi_task::host::HostError {
        message: "Parent session retired".into(),
    })?;
    let mut definition =
        session
            .get_tool_definition(name)
            .ok_or_else(|| senpi_task::host::HostError {
                message: format!("Shared parent tool {name} has no native definition"),
            })?;
    session
        .get_registered_tool(name)
        .ok_or_else(|| senpi_task::host::HostError {
            message: format!("Shared parent tool {name} is not executable"),
        })?;
    let name = name.to_owned();
    definition.execute = Arc::new(move |call| {
        let parent = parent.clone();
        let name = name.clone();
        Box::pin(async move {
            call.signal.check()?;
            let parent = parent().ok_or_else(|| {
                maho_tools::ToolError::Message("Parent session retired".into())
            })?;
            let controller = maho_ai::utils::abort::AbortController::new();
            let updates = call.on_update.map(|update| {
                Arc::new(move |result| {
                    let result = serde_json::to_value(result)
                        .and_then(serde_json::from_value)
                        .map_err(maho_tools::ToolError::from)
                        .and_then(|result| update(result));
                    if let Err(error) = result {
                        eprintln!("Tool update failed: {error}");
                    }
                }) as maho_agent::AgentToolUpdateCallback
            });
            let operation = parent.execute_prepared_shared_tool(
                call.id,
                &name,
                call.params,
                maho_core::agent_session::ExecuteToolOptions {
                    signal: Some(controller.signal()),
                    activate_inactive_tool: None,
                },
                updates,
            );
            let result = tokio::select! {
                biased;
                () = call.signal.cancelled() => {
                    controller.abort(None);
                    return Err(maho_tools::ToolError::Aborted);
                }
                result = operation => result,
            }
            .map_err(|error| maho_tools::ToolError::Message(error.to_string()))?;
            serde_json::to_value(result)
                .and_then(serde_json::from_value)
                .map_err(maho_tools::ToolError::from)
        })
    });
    Ok(definition)
}

impl senpi_task::runners::in_process::shared_tool_filter::ChildTool for NativeParentTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }

    fn execute(
        &self,
        tool_call_id: &str,
        input: &serde_json::Value,
    ) -> Result<serde_json::Value, senpi_task::host::HostError> {
        let failure = |message: String| senpi_task::host::HostError { message };
        let parent = (self.parent)().ok_or_else(|| failure("Parent session retired".into()))?;
        let executor = self.executor.clone();
        let name = self.name.clone();
        let tool_call_id = tool_call_id.to_owned();
        let input = input.clone();
        let execute =
            move || run_native_parent_tool(&executor, &parent, &tool_call_id, &name, input, None);
        if tokio::runtime::Handle::try_current().is_ok() {
            std::thread::Builder::new()
                .name("native-parent-tool".into())
                .spawn(execute)
                .map_err(|error| failure(error.to_string()))?
                .join()
                .map_err(|_| failure("Native parent tool worker panicked".into()))?
        } else {
            execute()
        }
    }

    /// Runs the blocking parent execution on tokio's blocking pool so the async caller's worker
    /// thread is not parked on a dedicated OS thread's `join()`.
    fn execute_async<'a>(
        &'a self,
        tool_call_id: &'a str,
        input: &'a serde_json::Value,
        signal: Option<maho_ai::utils::abort::AbortSignal>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<serde_json::Value, senpi_task::host::HostError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let failure = |message: String| senpi_task::host::HostError { message };
            if let Some(signal) = &signal {
                signal
                    .throw_if_aborted()
                    .map_err(|error| failure(error.to_string()))?;
            }
            if tokio::runtime::Handle::try_current().is_err() {
                return self.execute(tool_call_id, input);
            }
            let parent = (self.parent)().ok_or_else(|| failure("Parent session retired".into()))?;
            let executor = self.executor.clone();
            let name = self.name.clone();
            let tool_call_id = tool_call_id.to_owned();
            let input = input.clone();
            tokio::task::spawn_blocking(move || {
                run_native_parent_tool(&executor, &parent, &tool_call_id, &name, input, signal)
            })
            .await
            .map_err(|_| failure("Native parent tool worker panicked".into()))?
        })
    }
}

fn run_native_parent_tool(
    executor: &tokio::runtime::Handle,
    parent: &maho_core::agent_session::AgentSession,
    tool_call_id: &str,
    name: &str,
    input: serde_json::Value,
    signal: Option<maho_ai::utils::abort::AbortSignal>,
) -> Result<serde_json::Value, senpi_task::host::HostError> {
    let failure = |message: String| senpi_task::host::HostError { message };
    let result = executor
        .block_on(parent.execute_tool_with_call_id(
            tool_call_id,
            name,
            input,
            maho_core::agent_session::ExecuteToolOptions {
                signal,
                activate_inactive_tool: None,
            },
        ))
        .map_err(|error| failure(error.to_string()))?;
    if result.is_error == Some(true) {
        let message = result
            .content
            .iter()
            .filter_map(|part| match part {
                maho_ai::types::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(failure(message));
    }
    serde_json::to_value(result).map_err(|error| failure(error.to_string()))
}

pub fn live_parent_tools(
    parent: Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>,
    executor: tokio::runtime::Handle,
) -> Arc<
    dyn Fn() -> Vec<senpi_task::runners::in_process::shared_tool_filter::ChildToolRef>
        + Send
        + Sync,
> {
    // Every in-process child keeps its tool list for as long as it stays resident (it can be
    // reopened), so a per-child deep copy of every parent tool description grows memory with each
    // completed child. Children share one `Arc` per unchanged parent tool instead.
    type ToolCache = std::collections::HashMap<
        String,
        (String, senpi_task::runners::in_process::shared_tool_filter::ChildToolRef),
    >;
    let cache: Arc<std::sync::Mutex<ToolCache>> = Arc::default();
    Arc::new(move || match parent() {
        Some(session) => {
            let mut cache = cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let tools: Vec<_> = session
                .get_all_tools()
                .into_iter()
                .filter_map(|tool| {
                    session.get_registered_tool(&tool.name)?;
                    if let Some((description, shared)) = cache.get(&tool.name)
                        && *description == tool.description
                    {
                        return Some(Arc::clone(shared));
                    }
                    let shared = Arc::new(NativeParentTool {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        parent: parent.clone(),
                        executor: executor.clone(),
                    })
                        as senpi_task::runners::in_process::shared_tool_filter::ChildToolRef;
                    cache.insert(tool.name, (tool.description, Arc::clone(&shared)));
                    Some(shared)
                })
                .collect();
            cache.retain(|name, _| tools.iter().any(|tool| tool.name() == name));
            tools
        }
        None => Vec::new(),
    })
}

/// Both builders use the same native binary, agent home, environment and extensions.
/// Admission retains the bounded catalog probe rather than admitting unconditionally.
pub fn native_rpc_options(
    executable: PathBuf,
    agent_dir: &str,
    mut parent_env: BTreeMap<String, String>,
    inherited_extensions: Vec<String>,
) -> RpcProcessRunnerOptions {
    parent_env.insert("MAHO_CODING_AGENT_DIR".into(), agent_dir.into());
    // Upstream extensions write process.env (Orca deletes the consumed prefill and publishes the
    // status owner pid), so later children inherit those writes.
    maho_ext_api::process_env::apply(&mut parent_env);
    let executable = executable.to_string_lossy().into_owned();
    let native = executable.clone();
    let runtime = RpcSpawnRuntime {
        is_bun_binary: false,
        exec_path: executable,
        platform: match std::env::consts::OS {
            "macos" => "darwin",
            "windows" => "win32",
            platform => platform,
        }
        .into(),
        parent_env,
        resolve_rpc_entry: Arc::new(String::new),
        resolve_senpi_executable: Some(Arc::new(move |_| Some(native.clone()))),
    };
    let catalog = runtime.clone();
    RpcProcessRunnerOptions {
        build_spawn: Some(Arc::new(move |spec| {
            native_profile(build_rpc_spawn(spec, &runtime))
        })),
        model_admission: Some(create_rpc_model_admission(RpcModelAdmissionOptions {
            build_spawn: Some(Arc::new(move |spec| {
                native_profile(build_rpc_model_catalog_spawn(spec, &catalog))
            })),
            ..Default::default()
        })),
        inherited_extensions,
        ..Default::default()
    }
}

pub fn authenticated_rpc_options(
    mut options: RpcProcessRunnerOptions,
    parent: Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>,
) -> RpcProcessRunnerOptions {
    let catalog_admission = options.model_admission.take();
    options.model_admission = Some(Arc::new(move |spec| {
        let unavailable = |message| {
            senpi_task::runners::RunnerFailure::new(
                senpi_task::runners::RunnerFailureKind::ModelUnavailable,
                message,
            )
        };
        let session = parent().ok_or_else(|| unavailable("Parent session retired".to_owned()))?;
        let reference = match spec
            .model
            .as_deref()
            .filter(|model| !model.trim().is_empty())
        {
            Some(reference) => reference,
            None => {
                return Err(unavailable(
                    "Authenticated process admission requires the planner's resolved model"
                        .to_owned(),
                ));
            }
        };
        let model = senpi_task::manager::parent_registry_context::find_model_reference(
            |provider, id| session.model_registry().find(provider, id),
            reference,
        )
        .ok_or_else(|| {
            unavailable(format!(
                "Task model {reference} is not registered in the live parent"
            ))
        })?;
        if !session.model_registry().has_configured_auth(&model) {
            return Err(unavailable(format!(
                "Task model {reference} has no configured parent authentication"
            )));
        }
        match &catalog_admission {
            Some(admit) => admit(spec),
            None => Err(unavailable(
                "Native child catalog admission is not configured".to_owned(),
            )),
        }
    }));
    options
}

fn native_profile(mut descriptor: RpcSpawnDescriptor) -> RpcSpawnDescriptor {
    // The imported builder owns member-environment filtering and isolated session paths.
    // The native host reads the MAHO spelling of that same path.
    if let Some(path) = descriptor
        .env
        .get("SENPI_CODING_AGENT_SESSION_DIR")
        .cloned()
    {
        descriptor
            .env
            .insert("MAHO_CODING_AGENT_SESSION_DIR".into(), path);
    }
    descriptor
}
