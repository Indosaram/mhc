//! Runner adapters (`manager/runner.ts`): map a [`ManagedStartSpec`] onto each runner's spec.
//! Both runner handles implement [`ManagedChildHandle`] directly, so TS `adapt*` is identity.

use std::path::Path;
use std::sync::Arc;

use crate::manager::child_handle::ManagedChildHandle;
use crate::manager::types::{
    ManagedRunner, ManagedRunnerError, ManagedRunnerResult, ManagedStartSpec, RpcRespawnRunner,
};
use crate::runners::in_process::child_handle::InProcessChildHandle;
use crate::runners::in_process::child_options::HostHandle;
use crate::runners::in_process::runner_error::RunnerError;
use crate::runners::in_process::{ChildSpec, InProcessRunner};
use crate::runners::rpc::handle::RpcChildHandle;
use crate::runners::rpc::spawn::resolve_child_session_dir;
use crate::runners::rpc_process::{RpcProcessRunner, RpcProcessRunnerOptions};
use crate::runners::types::RpcRunnerSpec;
use crate::runners::{RunnerFailure, RunnerFailureKind};

/// The host-typed per-child context (`InProcessSessionContext`).
#[derive(Clone, Default)]
pub struct InProcessSessionContext {
    pub agent_dir: Option<String>,
    pub auth_storage: Option<HostHandle>,
    pub model_registry: Option<HostHandle>,
    pub model_runtime: Option<HostHandle>,
    pub model: Option<HostHandle>,
    pub thinking_level: Option<String>,
}

/// `ResumeSessionContextResult`; the only failure code is `model_unavailable`.
pub type ResumeSessionContextResult = Result<InProcessSessionContext, String>;

/// `InProcessSessionContextProvider` with its optional `resolveResumeContext`.
pub trait InProcessSessionContextProvider: Send + Sync {
    fn provide(&self, spec: &ManagedStartSpec) -> InProcessSessionContext;
    fn resolve_resume_context(
        &self,
        _spec: &ManagedStartSpec,
    ) -> Option<ResumeSessionContextResult> {
        None
    }
}

/// The default provider: an empty context.
pub struct EmptySessionContext;

impl InProcessSessionContextProvider for EmptySessionContext {
    fn provide(&self, _spec: &ManagedStartSpec) -> InProcessSessionContext {
        InProcessSessionContext::default()
    }
}

impl<F> InProcessSessionContextProvider for F
where
    F: Fn(&ManagedStartSpec) -> InProcessSessionContext + Send + Sync,
{
    fn provide(&self, spec: &ManagedStartSpec) -> InProcessSessionContext {
        self(spec)
    }
}

pub type InProcessHandleResult = Result<Arc<dyn ManagedChildHandle>, RunnerError>;

/// `InProcessRunnerLike`.
pub trait InProcessRunnerLike: Send + Sync {
    fn start(&self, spec: &ChildSpec) -> InProcessHandleResult;
    /// `None` when the runner cannot resume.
    fn resume(&self, _spec: &ChildSpec, _session_path: &str) -> Option<InProcessHandleResult> {
        None
    }
}

fn erase(handle: Arc<InProcessChildHandle>) -> Arc<dyn ManagedChildHandle> {
    handle
}

impl InProcessRunnerLike for InProcessRunner {
    fn start(&self, spec: &ChildSpec) -> InProcessHandleResult {
        InProcessRunner::start(self, spec).map(erase)
    }

    fn resume(&self, spec: &ChildSpec, session_path: &str) -> Option<InProcessHandleResult> {
        Some(InProcessRunner::resume(self, spec, Path::new(session_path)).map(erase))
    }
}

/// `RpcRunnerLike`.
pub trait RpcRunnerLike: Send + Sync {
    fn start(&self, spec: &RpcRunnerSpec) -> Result<Arc<dyn ManagedChildHandle>, RunnerFailure>;
}

impl RpcRunnerLike for RpcProcessRunner {
    fn start(&self, spec: &RpcRunnerSpec) -> Result<Arc<dyn ManagedChildHandle>, RunnerFailure> {
        RpcProcessRunner::start(self, spec)
            .map(|handle: Arc<RpcChildHandle>| handle as Arc<dyn ManagedChildHandle>)
    }
}

fn runner_error(error: RunnerError) -> ManagedRunnerError {
    ManagedRunnerError::Runner(error.failure)
}

struct InProcessManagedRunner<R, C> {
    runner: R,
    context: C,
}

impl<R: InProcessRunnerLike, C: InProcessSessionContextProvider> ManagedRunner
    for InProcessManagedRunner<R, C>
{
    fn start(&self, spec: &ManagedStartSpec) -> ManagedRunnerResult {
        let child = to_child_spec(spec, self.context.provide(spec));
        self.runner.start(&child).map_err(runner_error)
    }

    fn resume(&self, spec: &ManagedStartSpec, session_path: &str) -> Option<ManagedRunnerResult> {
        let resolved = match self.context.resolve_resume_context(spec) {
            Some(Err(reason)) => {
                return Some(Err(ManagedRunnerError::Runner(RunnerFailure::new(
                    RunnerFailureKind::ModelUnavailable,
                    reason,
                ))));
            }
            Some(Ok(context)) => Some(context),
            None => None,
        };
        let child = to_child_spec(spec, resolved.unwrap_or_else(|| self.context.provide(spec)));
        Some(match self.runner.resume(&child, session_path) {
            Some(result) => result.map_err(runner_error),
            None => Err(ManagedRunnerError::Runner(RunnerFailure::new(
                RunnerFailureKind::SessionUnavailable,
                "in-process runner cannot resume sessions",
            ))),
        })
    }
}

pub fn create_in_process_managed_runner<R, C>(runner: R, context: C) -> Arc<dyn ManagedRunner>
where
    R: InProcessRunnerLike + 'static,
    C: InProcessSessionContextProvider + 'static,
{
    Arc::new(InProcessManagedRunner { runner, context })
}

struct RpcManagedRunner<R> {
    runner: R,
}

impl<R: RpcRunnerLike> ManagedRunner for RpcManagedRunner<R> {
    fn start(&self, spec: &ManagedStartSpec) -> ManagedRunnerResult {
        self.runner
            .start(&to_rpc_spec(spec))
            .map_err(ManagedRunnerError::Runner)
    }
}

pub fn create_rpc_managed_runner<R: RpcRunnerLike + 'static>(runner: R) -> Arc<dyn ManagedRunner> {
    Arc::new(RpcManagedRunner { runner })
}

/// A detached RPC child cannot share the parent's model registry, so model and variant ride the
/// spec onto the child's command line.
pub fn to_rpc_spec(spec: &ManagedStartSpec) -> RpcRunnerSpec {
    RpcRunnerSpec {
        task_id: spec.task_id.clone(),
        cwd: spec.cwd.clone(),
        state_dir: spec.state_dir.clone(),
        prompt: spec.prompt.clone(),
        model: spec.model.clone(),
        variant: spec.variant.clone(),
        extensions: spec.extensions.clone(),
        member_env: spec.member_env.clone(),
        ..RpcRunnerSpec::default()
    }
}

/// `stateDir` is already `children/<taskId>`; the session dir nests `sessions/<taskId>/` under
/// it, the same layout as an RPC child.
pub fn to_child_spec(spec: &ManagedStartSpec, context: InProcessSessionContext) -> ChildSpec {
    ChildSpec {
        task_id: spec.task_id.clone(),
        cwd: spec.cwd.clone(),
        session_dir: Some(resolve_child_session_dir(&spec.state_dir, &spec.task_id)),
        depth: spec.depth,
        parent_session_id: spec.parent_session_id.clone(),
        root_session_id: spec.root_session_id.clone(),
        prompt: spec.prompt.clone(),
        // `ManagedStartSpec` carries no system prompt (upstream `toChildSpec` maps none), so a
        // manager-routed child keeps the engine's own default; the direct `ChildSpec` callers set
        // `system_prompt` on the spec themselves.
        system_prompt: None,
        // `ManagedStartSpec` carries no prompt envelope (upstream `toChildSpec` maps none), so a
        // manager-routed child keeps the default subagent envelope.
        prompt_envelope: None,
        // `ManagedStartSpec` carries no completion policy (upstream `toChildSpec` maps none), so a
        // manager-routed child keeps the default `FinalText` judgment.
        completion: None,
        agent_dir: context.agent_dir,
        auth_storage: context.auth_storage,
        model_registry: context.model_registry,
        model_runtime: context.model_runtime,
        model: context.model,
        thinking_level: context.thinking_level,
        selected_model: spec.model.clone(),
        requested_model: spec.requested_model.clone(),
        fallback_models: spec.fallback_models.clone(),
        resolved_model: spec.resolved_model.clone(),
        // `ManagedStartSpec` carries no retry override (upstream `toChildSpec` maps none), so a
        // manager-routed child keeps the engine's own budget; the direct `ChildSpec` callers (the
        // memory/kibitzer launch sites) set it on the spec themselves.
        retry: None,
        agent_type: spec.agent_type.clone(),
        instructions: spec.instructions.clone(),
        tool_allowlist: spec.tool_allowlist.clone(),
        tool_denylist: spec.tool_denylist.clone(),
        member_scoped_tool_names: spec.member_scoped_tool_names.clone(),
        member_scoped_tools: spec.member_scoped_tools.clone(),
    }
}

/// The manager's default respawn runner (`new RpcProcessRunner()`), built lazily so a manager
/// that never respawns never resolves the spawn runtime.
#[derive(Default)]
pub struct DefaultRpcRespawnRunner {
    runner: std::sync::OnceLock<RpcProcessRunner>,
}

impl RpcRespawnRunner for DefaultRpcRespawnRunner {
    fn start(&self, spec: &RpcRunnerSpec) -> ManagedRunnerResult {
        self.runner
            .get_or_init(|| RpcProcessRunner::new(RpcProcessRunnerOptions::default()))
            .start(spec)
            .map(|handle| handle as Arc<dyn ManagedChildHandle>)
            .map_err(ManagedRunnerError::Runner)
    }
}
