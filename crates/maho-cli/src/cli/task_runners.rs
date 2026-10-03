//! Native executable/profile configuration shared by task launch and RPC respawn.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use senpi_task::runners::rpc::{
    model_admission::{RpcModelAdmissionOptions, create_rpc_model_admission},
    process::RpcSpawnDescriptor,
    spawn::{RpcSpawnRuntime, build_rpc_model_catalog_spawn, build_rpc_spawn},
};
use senpi_task::runners::rpc_process::RpcProcessRunnerOptions;

pub struct NativeChildModelRegistry(pub maho_core::model_registry::ModelRegistry);

impl senpi_task::manager::parent_registry_context::ChildModelRegistry for NativeChildModelRegistry {
    fn find(&self, provider: &str, model_id: &str) -> Option<senpi_task::runners::in_process::child_options::HostHandle> {
        self.0.find(provider, model_id).map(|model| Arc::new(model) as senpi_task::runners::in_process::child_options::HostHandle)
    }

    fn auth_storage(&self) -> senpi_task::runners::in_process::child_options::HostHandle {
        self.0.auth_storage.clone()
    }

    fn model_runtime(&self) -> Option<senpi_task::runners::in_process::child_options::HostHandle> {
        Some(Arc::new(self.0.model_runtime.clone()))
    }
}

pub fn live_parent_registry(
    parent: std::sync::Weak<maho_core::agent_session::AgentSession>,
) -> senpi_task::manager::parent_registry_context::ParentModelRegistryResolver {
    Arc::new(move || parent.upgrade().map(|session| {
        Arc::new(NativeChildModelRegistry(session.model_registry().clone()))
            as Arc<dyn senpi_task::manager::parent_registry_context::ChildModelRegistry>
    }))
}

pub fn native_child_session_manager(
    child: &senpi_task::runners::in_process::child_options::ChildSessionOptions,
) -> maho_core::session_manager::SessionManager {
    maho_core::session_manager::SessionManager::open(
        &child.session_manager.session_file().to_string_lossy(),
        Some(child.session_manager.session_dir()), Some(&child.cwd), None,
    )
}

struct NativeParentTool {
    name: String,
    description: String,
    parent: std::sync::Weak<maho_core::agent_session::AgentSession>,
    executor: tokio::runtime::Handle,
}

pub fn native_child_settings(
    retry: &senpi_task::runners::in_process::runtime_fallback_settings::RetryFallbackSettings,
) -> maho_core::settings_manager::SettingsManager {
    let mut settings = maho_core::settings_manager::SettingsManager::from_storage(
        Box::<maho_core::settings_manager::InMemorySettingsStorage>::default(), false,
    );
    settings.apply_overrides(&serde_json::Map::from_iter([("retry".into(), serde_json::json!({
        "modelFallback": retry.model_fallback, "fallbackChains": retry.chains,
    }))]));
    settings
}

impl senpi_task::runners::in_process::shared_tool_filter::ChildTool for NativeParentTool {
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }

    fn execute(&self, tool_call_id: &str, input: &serde_json::Value) -> Result<serde_json::Value, senpi_task::host::HostError> {
        let failure = |message: String| senpi_task::host::HostError { message };
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(failure("Synchronous child tools must execute on the task worker, not the host executor".into()));
        }
        let parent = self.parent.upgrade().ok_or_else(|| failure("Parent session retired".into()))?;
        let result = self.executor.block_on(parent.execute_tool_with_call_id(tool_call_id, &self.name, input.clone(),
            maho_core::agent_session::ExecuteToolOptions { signal: None, activate_inactive_tool: None }))
            .map_err(|error| failure(error.to_string()))?;
        if result.is_error == Some(true) {
            let message = result.content.iter().filter_map(|part| match part {
                maho_ai::types::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            }).collect::<Vec<_>>().join("\n");
            return Err(failure(message));
        }
        serde_json::to_value(result).map_err(|error| failure(error.to_string()))
    }
}

pub fn live_parent_tools(
    parent: std::sync::Weak<maho_core::agent_session::AgentSession>,
    executor: tokio::runtime::Handle,
) -> Arc<dyn Fn() -> Vec<senpi_task::runners::in_process::shared_tool_filter::ChildToolRef> + Send + Sync> {
    Arc::new(move || match parent.upgrade() {
        Some(session) => session.get_all_tools().into_iter().filter_map(|tool| {
            session.get_registered_tool(&tool.name)?;
            Some(Arc::new(NativeParentTool {
                name: tool.name, description: tool.description,
                parent: parent.clone(), executor: executor.clone(),
            }) as senpi_task::runners::in_process::shared_tool_filter::ChildToolRef)
        }).collect(),
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
    let executable = executable.to_string_lossy().into_owned();
    let native = executable.clone();
    let runtime = RpcSpawnRuntime {
        is_bun_binary: false,
        exec_path: executable,
        platform: match std::env::consts::OS {
            "macos" => "darwin",
            "windows" => "win32",
            platform => platform,
        }.into(),
        parent_env,
        resolve_rpc_entry: Arc::new(String::new),
        resolve_senpi_executable: Some(Arc::new(move |_| Some(native.clone()))),
    };
    let catalog = runtime.clone();
    RpcProcessRunnerOptions {
        build_spawn: Some(Arc::new(move |spec| native_profile(build_rpc_spawn(spec, &runtime)))),
        model_admission: Some(create_rpc_model_admission(RpcModelAdmissionOptions {
            build_spawn: Some(Arc::new(move |spec| native_profile(build_rpc_model_catalog_spawn(spec, &catalog)))),
            ..Default::default()
        })),
        inherited_extensions,
        ..Default::default()
    }
}

pub fn authenticated_rpc_options(
    mut options: RpcProcessRunnerOptions,
    parent: std::sync::Weak<maho_core::agent_session::AgentSession>,
) -> RpcProcessRunnerOptions {
    let catalog_admission = options.model_admission.take();
    options.model_admission = Some(Arc::new(move |spec| {
        let unavailable = |message| senpi_task::runners::RunnerFailure::new(
            senpi_task::runners::RunnerFailureKind::ModelUnavailable, message);
        let session = parent.upgrade().ok_or_else(|| unavailable("Parent session retired".to_owned()))?;
        let reference = match spec.model.as_deref().filter(|model| !model.trim().is_empty()) {
            Some(reference) => reference,
            None => return Err(unavailable("Authenticated process admission requires the planner's resolved model".to_owned())),
        };
        let model = senpi_task::manager::parent_registry_context::find_model_reference(
            |provider, id| session.model_registry().find(provider, id), reference,
        ).ok_or_else(|| unavailable(format!("Task model {reference} is not registered in the live parent")))?;
        if !session.model_registry().has_configured_auth(&model) {
            return Err(unavailable(format!("Task model {reference} has no configured parent authentication")));
        }
        match &catalog_admission {
            Some(admit) => admit(spec),
            None => Err(unavailable("Native child catalog admission is not configured".to_owned())),
        }
    }));
    options
}

fn native_profile(mut descriptor: RpcSpawnDescriptor) -> RpcSpawnDescriptor {
    // The imported builder owns member-environment filtering and isolated session paths.
    // The native host reads the MAHO spelling of that same path.
    if let Some(path) = descriptor.env.get("SENPI_CODING_AGENT_SESSION_DIR").cloned() {
        descriptor.env.insert("MAHO_CODING_AGENT_SESSION_DIR".into(), path);
    }
    descriptor
}
