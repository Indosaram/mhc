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

fn native_profile(mut descriptor: RpcSpawnDescriptor) -> RpcSpawnDescriptor {
    // The imported builder owns member-environment filtering and isolated session paths.
    // The native host reads the MAHO spelling of that same path.
    if let Some(path) = descriptor.env.get("SENPI_CODING_AGENT_SESSION_DIR").cloned() {
        descriptor.env.insert("MAHO_CODING_AGENT_SESSION_DIR".into(), path);
    }
    descriptor
}
