//! The gateway SDK: sends and reports over one store, for a CLI or a connector.
//!
//! Upstream counterpart: `thread/sdk.ts`. A send resolves sender provenance in the sender's own
//! runtime and reports whether an armed completion wake reached its target.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use super::adapter::BoxFuture;
use super::engine::{GatewayDeliverRequest, GatewayEngine};
use super::errors::ThreadToolFailure;
use super::provenance_resolver::{ProvenanceRuntime, ProvenanceWorkspace, resolve_provenance};
use super::relay::{GatewayRelay, RelayResult};
use super::store_api::GatewayStoreApi;
use super::store_relay_ops::{BindInput, BindingRecord, RelayOutcome, ReportOpRequest, ReportOpResult};
use super::store_extensions::{DeclaredSessionOp, SessionCaller, StoreExtensionRegistration, StoreExtensionResult};
use super::types::{GatewayDeliveryMode, GatewayDeliveryResult, GatewaySender, Provenance};

pub struct ThreadSdkOptions {
    pub agent_dir: String,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub uid: i64,
    pub user: String,
    pub store: Arc<dyn GatewayStoreApi>,
    pub engine: GatewayEngine,
    pub relay: GatewayRelay,
    pub workspaces: Option<Vec<ProvenanceWorkspace>>,
}

/// The sender's own provenance, resolved once per send in the sender's runtime.
pub fn sdk_provenance(cwd: &str, env: &BTreeMap<String, String>, workspaces: Option<Vec<ProvenanceWorkspace>>) -> Provenance {
    let mut runtime = ProvenanceRuntime { cwd: std::path::PathBuf::from(cwd), env: env.clone(), workspaces, ..Default::default() };
    if runtime.env.is_empty() {
        runtime.env = std::env::vars().collect();
    }
    resolve_provenance(&runtime)
}

#[derive(Clone)]
pub struct ThreadSdk {
    options: Arc<ThreadSdkOptions>,
}

impl ThreadSdk {
    pub fn new(options: ThreadSdkOptions) -> Self {
        Self { options: Arc::new(options) }
    }

    fn sender(&self) -> GatewaySender {
        GatewaySender::Cli { uid: self.options.uid, user: self.options.user.clone() }
    }

    /// `thread_send` through the SDK: the same engine path a session tool takes.
    pub fn send(&self, thread: &str, text: &str, mode: Option<GatewayDeliveryMode>) -> BoxFuture<GatewayDeliveryResult> {
        let provenance = sdk_provenance(&self.options.cwd, &self.options.env, self.options.workspaces.clone());
        let provenance = Arc::new(provenance);
        let options = self.options.clone();
        let request = GatewayDeliverRequest { mode, ..GatewayDeliverRequest::new(self.sender(), thread, text) };
        Box::pin(async move {
            let engine = GatewayEngine::new(super::engine::GatewayEngineOptions {
                sender_provenance: Some(Arc::new(move || (*provenance).clone())),
                store: options.store.clone(),
                endpoints: options.engine.options().endpoints.clone(),
                resolve: options.engine.options().resolve.clone(),
                now: options.engine.options().now.clone(),
            });
            engine.deliver(request).await
        })
    }

    pub fn report(&self, request: &ReportOpRequest) -> RelayResult<ReportOpResult> {
        self.options.relay.report(request)
    }

    pub fn bind(&self, input: &BindInput) -> RelayResult<BindingRecord> {
        self.options.relay.bind(input)
    }

    pub fn unbind(&self, binding_id: &str, expected_revision: i64) -> RelayResult<(BindingRecord, bool, Vec<String>)> {
        self.options.relay.unbind(binding_id, expected_revision)
    }

    pub fn rebind(&self, binding_id: &str, expected_revision: i64, session_durable_id: &str) -> RelayResult<(BindingRecord, Vec<String>)> {
        self.options.relay.rebind(binding_id, expected_revision, session_durable_id)
    }

    pub fn register_store_extension(&self, descriptor: &StoreExtensionRegistration) -> StoreExtensionResult<i64> {
        self.options.store.register_store_extension(descriptor, self.options.store.now(), None)
    }

    pub fn extension_call(&self, name: &str, op: &str, args: &Value) -> StoreExtensionResult<Value> {
        self.options.store.extension_call(name, op, args, self.options.store.now())
    }

    pub fn session_callable_ops(&self) -> Vec<DeclaredSessionOp> {
        self.options.store.session_callable_ops()
    }

    pub fn extension_session_await(&self, name: &str, op: &str, args: &Value, caller: &SessionCaller) -> StoreExtensionResult<Value> {
        self.options.store.extension_session_await(name, op, args, caller, self.options.store.now())
    }

    /// The session settled: armed completions become outbox rows.
    pub fn settle(&self, session_durable_id: &str, outcome: &str, through_arm_seq: Option<i64>) -> RelayResult<Vec<(String, i64)>> {
        self.options.relay.settle(session_durable_id, outcome, through_arm_seq)
    }
}

/// A report the caller retries is refused with the same failure a relay reports.
pub fn report_failure(refusal: RelayOutcome<ReportOpResult>) -> Option<ThreadToolFailure> {
    match refusal {
        RelayOutcome::Ok { .. } => None,
        RelayOutcome::Refused { code, message, details } => Some(super::errors::thread_tool_failure(code.as_str(), message, "Call thread_bindings and retry after checking the binding.", details)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_environment_labels_reach_the_sender_provenance() {
        let env = BTreeMap::from([
            ("OMO_PROVENANCE_WORKSPACE".to_owned(), "Runtime workspace".to_owned()),
            ("OMO_PROVENANCE_PROJECT".to_owned(), "runtime-project".to_owned()),
            ("OMO_PROVENANCE_LANE".to_owned(), "review".to_owned()),
        ]);
        let provenance = sdk_provenance("/tmp/whatever", &env, None);
        assert_eq!(provenance.workspace, "Runtime workspace");
        assert_eq!(provenance.project, "runtime-project");
        assert_eq!(provenance.lane.as_deref(), Some("review"));
        assert!(provenance.session.is_none());
    }
}
