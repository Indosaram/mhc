//! `runners/rpc-process.ts`: spawn a senpi RPC child (never through a shell) with an isolated
//! session dir and return a steerable handle. Process destruction goes only through
//! `rpc/terminate.rs`.

use std::sync::Arc;

use serde_json::json;

use crate::runners::rpc::handle::{Clock, RpcChildHandle, RpcChildHandleOptions};
use crate::runners::rpc::model_admission::{
    RpcModelAdmission, RpcModelAdmissionOptions, create_rpc_model_admission,
};
use crate::runners::rpc::process::{RpcChildProcess, RpcSpawnDescriptor};
use crate::runners::rpc::protocol_client::{
    MalformedLineHandler, RpcClientPort, RpcProtocolClient, RpcProtocolClientOptions,
};
use crate::runners::rpc::spawn::{RpcSpawnRuntime, build_rpc_spawn};
use crate::runners::types::{RpcRunnerSpec, RpcSpawnSpec, TerminateOptions};
use crate::runners::{RunnerFailure, RunnerFailureKind};

const DEFAULT_HEARTBEAT_INTERVAL_MS: u64 = 10_000;

pub type SpawnChild = Arc<dyn Fn(&RpcSpawnDescriptor) -> Arc<RpcChildProcess> + Send + Sync>;
pub type BuildSpawn = Arc<dyn Fn(&RpcRunnerSpec) -> RpcSpawnDescriptor + Send + Sync>;

#[derive(Clone, Default)]
pub struct RpcProcessRunnerOptions {
    pub spawn_child: Option<SpawnChild>,
    pub build_spawn: Option<BuildSpawn>,
    pub heartbeat_interval_ms: Option<u64>,
    pub on_malformed_line: Option<MalformedLineHandler>,
    pub now: Option<Clock>,
    pub model_admission: Option<RpcModelAdmission>,
    pub inherited_extensions: Vec<String>,
}

pub struct RpcProcessRunner {
    spawn_child: SpawnChild,
    build_spawn: BuildSpawn,
    heartbeat_interval_ms: u64,
    on_malformed_line: Option<MalformedLineHandler>,
    now: Clock,
    model_admission: RpcModelAdmission,
    inherited_extensions: Vec<String>,
}

impl RpcProcessRunner {
    pub fn new(options: RpcProcessRunnerOptions) -> Self {
        Self {
            spawn_child: options.spawn_child.unwrap_or_else(|| {
                Arc::new(|descriptor| Arc::new(RpcChildProcess::spawn(descriptor)))
            }),
            build_spawn: options.build_spawn.unwrap_or_else(|| {
                let runtime = RpcSpawnRuntime::default();
                Arc::new(move |spec| build_rpc_spawn(spec, &runtime))
            }),
            heartbeat_interval_ms: options
                .heartbeat_interval_ms
                .unwrap_or(DEFAULT_HEARTBEAT_INTERVAL_MS),
            on_malformed_line: options.on_malformed_line,
            now: options
                .now
                .unwrap_or_else(|| Arc::new(|| chrono::Utc::now().timestamp_millis())),
            model_admission: options
                .model_admission
                .unwrap_or_else(|| create_rpc_model_admission(RpcModelAdmissionOptions::default())),
            inherited_extensions: options.inherited_extensions,
        }
    }

    /// Admit the model, spawn, then either drive the initial prompt or switch to the resume session.
    /// On failure the unstarted child is terminated and disposed before the error returns.
    pub fn start(&self, spec_input: &RpcRunnerSpec) -> Result<Arc<RpcChildHandle>, RunnerFailure> {
        let mut spec = spec_input.clone();
        if spec.extensions.is_none() && !self.inherited_extensions.is_empty() {
            spec.extensions = Some(self.inherited_extensions.clone());
        }
        (self.model_admission)(&spec)?;
        let descriptor = (self.build_spawn)(&spec);
        let child = (self.spawn_child)(&descriptor);
        let client: Arc<dyn RpcClientPort> = Arc::new(RpcProtocolClient::new(
            child,
            RpcProtocolClientOptions {
                on_malformed_line: self.on_malformed_line.clone(),
                auto_answer_ui: None,
            },
        ));
        let handle = RpcChildHandle::new(RpcChildHandleOptions {
            client: Arc::clone(&client),
            task_id: spec.task_id.clone(),
            heartbeat_interval_ms: self.heartbeat_interval_ms,
            now: Arc::clone(&self.now),
        });
        let started = match &spec.resume_session_path {
            None => handle
                .start_initial_prompt(&spec.prompt)
                .map(|()| None)
                .map_err(|error| (RunnerFailureKind::ChildPromptFailed, error.to_string())),
            Some(path) => client
                .switch_session(path)
                .map(|result| Some((path.clone(), result)))
                .map_err(|error| (RunnerFailureKind::SessionUnavailable, error.to_string())),
        };
        let resume = match started {
            Ok(resume) => resume,
            Err((kind, message)) => {
                if let Err(cleanup) = handle.terminate_with(TerminateOptions::default()) {
                    utils::logger::log(
                        "senpi-task rpc start cleanup failed",
                        Some(&json!({ "taskId": spec.task_id, "error": cleanup.to_string() })),
                    );
                }
                handle.dispose_handle();
                return Err(RunnerFailure::new(kind, message));
            }
        };
        handle.attach_start_facts(
            RpcSpawnSpec {
                cwd: spec.cwd.clone(),
                extensions: spec.extensions.clone(),
                member_env: spec.member_env.clone(),
            },
            resume,
        );
        Ok(handle)
    }
}
