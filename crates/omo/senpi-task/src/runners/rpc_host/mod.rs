//! Host RPC adapters (`runners/rpc-host.ts` and `rpc-host/`).
pub type AdmitCallback = Arc<dyn Fn(&crate::runners::types::RpcRunnerSpec) -> Result<(), HostError> + Send + Sync>;
pub mod durable_json;
pub mod handle;
pub mod handle_listeners;
pub mod handle_port;
pub mod handle_recovery;
pub mod handle_session_identity;
pub mod handle_teardown;
pub mod host_command_dispatch;
pub mod host_warmup;
pub mod open_session;
pub mod reattach_port;
pub mod session_client_connection;
pub mod session_context;
pub mod session_open;
pub mod session_role;
pub mod store_index;
pub mod task_parent_wire;
use crate::host::HostError;
use std::sync::Arc;
pub type OpenHostSession = Arc<
    dyn Fn(
            &crate::runners::types::RpcRunnerSpec,
            session_context::ChildSessionContext,
        ) -> Result<
            (
                Arc<dyn handle_port::HostSessionPort>,
                open_session::OpenedHostSession,
                Arc<dyn Fn() -> crate::runners::RunnerOutcome + Send + Sync>,
            ),
            HostError,
        > + Send
        + Sync,
>;

/// Manager-facing adapter; the assembly supplies the live authenticated session opener.
pub struct ConfiguredRpcHostRunner {
    pub runner: RpcHostRunner,
    /// Store-index admission is a precondition of opening any hosted child.
    pub admit:
        AdmitCallback,
    pub open: OpenHostSession,
}
impl crate::manager::runner::RpcRunnerLike for ConfiguredRpcHostRunner {
    fn start(
        &self,
        spec: &crate::runners::types::RpcRunnerSpec,
    ) -> Result<
        Arc<dyn crate::manager::child_handle::ManagedChildHandle>,
        crate::runners::RunnerFailure,
    > {
        (self.admit)(spec).map_err(|error| {
            crate::runners::RunnerFailure::new(
                crate::runners::RunnerFailureKind::SessionCreateFailed,
                error.to_string(),
            )
        })?;
        self.runner
            .start(spec, |spec, context| (self.open)(spec, context))
            .map(|handle| handle as Arc<dyn crate::manager::child_handle::ManagedChildHandle>)
            .map_err(|error| {
                crate::runners::RunnerFailure::new(
                    crate::runners::RunnerFailureKind::SessionCreateFailed,
                    error.to_string(),
                )
            })
    }
    fn wait_for_teardown_settlement(&self) -> Result<(), crate::runners::RunnerFailure> {
        self.runner.settle_teardowns().map_err(|error| {
            crate::runners::RunnerFailure::new(
                crate::runners::RunnerFailureKind::ChildTurnFailed,
                error.to_string(),
            )
        })
    }
}

pub struct RpcHostRunner {
    pub opener: session_open::HostSessionOpener,
    pub settle_fallback: Arc<dyn Fn() -> Result<(), HostError> + Send + Sync>,
}
impl RpcHostRunner {
    pub fn start(
        &self,
        spec: &crate::runners::types::RpcRunnerSpec,
        open: impl FnOnce(
            &crate::runners::types::RpcRunnerSpec,
            session_context::ChildSessionContext,
        ) -> Result<
            (
                std::sync::Arc<dyn handle_port::HostSessionPort>,
                open_session::OpenedHostSession,
                std::sync::Arc<dyn Fn() -> crate::runners::RunnerOutcome + Send + Sync>,
            ),
            HostError,
        >,
    ) -> Result<std::sync::Arc<handle::HostSessionHandle>, HostError> {
        let (client, opened, outcome) = open(spec, session_context::build_child_context(spec))?;
        let pin = spec
            .model
            .as_deref()
            .and_then(|reference| reference.split_once('/'))
            .map(|(provider, id)| {
                crate::runners::pinned_model_equivalence::CatalogModelIdentity::new(provider, id)
            });
        let opened = open_session::open_task_host_session(client.clone(), opened, pin.as_ref())?;
        let attached = opened.attached;
        let handle =
            handle::create_host_session_handle(spec.task_id.clone(), client, opened, outcome);
        if attached {
            Ok(handle)
        } else {
            self.opener.start_initial_prompt(handle, &spec.prompt)
        }
    }

    pub fn settle_teardowns(&self) -> Result<(), HostError> {
        let opened = self.opener.settle_teardowns();
        let fallback = (self.settle_fallback)();
        opened.and(fallback)
    }
}
