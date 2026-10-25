//! The thread component: it wires the store, engine, relay, drain and tools of one session.
//!
//! Upstream counterpart: `thread/component.ts`.

use std::sync::Arc;

use serde_json::Value;

use super::adapter::SessionRuntimePort;
use super::drain::{InboxDrain, InboxDrainOptions};
use super::engine::{GatewayEngine, GatewayEngineOptions, GatewayResolve};
use super::relay::{GatewayRelay, GatewayRelayOptions};
use super::store_api::GatewayStoreApi;
use super::tools::{ThreadTool, ThreadToolsOptions, create_thread_tools, register_extension_tools};

pub struct GatewayThreadComponentOptions {
    pub store: Arc<dyn GatewayStoreApi>,
    pub runtime: SessionRuntimePort,
    pub durable_id: String,
    pub session_path: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub caller_session_id: Arc<dyn Fn() -> String + Send + Sync>,
    pub caller_name: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
    pub caller_provenance: Arc<dyn Fn() -> super::types::Provenance + Send + Sync>,
    pub resolve: GatewayResolve,
    pub endpoints: super::adapter::GatewayEndpointPort,
    pub locate: Arc<dyn Fn(&str) -> super::adapter::BoxFuture<Option<super::adapter::GatewayEndpointRef>> + Send + Sync>,
    pub notify: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub log: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

/// One session's gateway: its drain, its relay and the tools its model calls.
pub struct GatewayThreadComponent {
    pub store: Arc<dyn GatewayStoreApi>,
    pub engine: GatewayEngine,
    pub relay: GatewayRelay,
    pub drain: Arc<InboxDrain>,
    pub tools: Vec<ThreadTool>,
    pub durable_id: String,
}

pub fn create_gateway_thread_component(options: Arc<GatewayThreadComponentOptions>) -> GatewayThreadComponent {
    let engine = GatewayEngine::new(GatewayEngineOptions {
        sender_provenance: Some(options.caller_provenance.clone()),
        store: options.store.clone(),
        endpoints: options.endpoints.clone(),
        resolve: options.resolve.clone(),
        now: None,
    });
    let relay = GatewayRelay::new(GatewayRelayOptions {
        store: options.store.clone(),
        engine: engine.clone(),
        endpoints: options.endpoints.clone(),
        locate: options.locate.clone(),
        now: None,
    });
    let drain = Arc::new(InboxDrain::new(InboxDrainOptions {
        store: options.store.clone(),
        runtime: options.runtime.clone(),
        durable_id: options.durable_id.clone(),
        session_path: options.session_path.clone(),
        now: None,
        log: options.log.clone(),
        notify: options.notify.clone(),
        on_admitted: None,
    }));
    let tools = create_thread_tools(Arc::new(ThreadToolsOptions {
        store: options.store.clone(),
        engine: engine.clone(),
        relay: relay.clone(),
        caller_session_id: options.caller_session_id.clone(),
        caller_name: options.caller_name.clone(),
        caller_provenance: options.caller_provenance.clone(),
    }));
    GatewayThreadComponent { store: options.store.clone(), engine, relay, drain, tools, durable_id: options.durable_id.clone() }
}

impl GatewayThreadComponent {
    /// The session's own extension tools, on top of the thread tools.
    pub fn tools_with_extensions(&self, caller: Arc<dyn Fn() -> String + Send + Sync>) -> Vec<ThreadTool> {
        let mut tools = self.tools.clone();
        tools.extend(register_extension_tools(self.store.clone(), caller));
        tools
    }

    /// Publishes this session's control endpoint and registers the drain's first pass.
    pub fn start(&self, incarnation: &str, endpoint_socket: Option<&str>, endpoint_kind: Option<&str>) -> Result<(), super::store_ops::StoreError> {
        self.store.register_incarnation(&self.durable_id, incarnation, endpoint_socket, endpoint_kind)?;
        self.drain.drain(&super::adapter::DrainWakeEvent { reason: super::adapter::WakeReason::Start, delivery_ids: None })?;
        Ok(())
    }

    /// Retires the drain and clears the published endpoint.
    pub fn stop(&self, incarnation: &str) -> Result<(), super::store_ops::StoreError> {
        self.store.clear_endpoint(&self.durable_id, incarnation)
    }

    /// A tool call by name, as a host dispatches it.
    pub fn execute(&self, name: &str, call_id: &str, args: Value) -> Option<super::adapter::BoxFuture<Value>> {
        self.tools.iter().find(|tool| tool.name == name).map(|tool| (tool.execute)(call_id, args))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tool_set_covers_send_report_and_bindings() {
        let store: Arc<dyn GatewayStoreApi> = super::super::store::create_gateway_store(super::super::store::GatewayStoreOptions::new(std::env::temp_dir().join("maho-component-test").to_string_lossy().into_owned())).expect("store");
        let runtime = SessionRuntimePort {
            phase: Arc::new(|| super::super::adapter::RuntimePhase::Idle),
            admission_gate: Arc::new(|| super::super::adapter::SessionAdmissionGate { can_admit: true, hold_reason: None, editor_revision: 1, turn_epoch: 1 }),
            admit_external_message: Arc::new(|_| Err("not in this test".to_owned())),
            list_admitted_deliveries: Arc::new(|| super::super::types::AdmissionLedger::default()),
        };
        let component = create_gateway_thread_component(Arc::new(GatewayThreadComponentOptions {
            store,
            runtime,
            durable_id: "receiver".into(),
            session_path: Arc::new(|| None),
            caller_session_id: Arc::new(|| "sender".into()),
            caller_name: None,
            caller_provenance: Arc::new(|| super::super::types::Provenance { workspace: "unclassified".into(), project: "test".into(), lane: None, session: None }),
            resolve: Arc::new(|_, _| Box::pin(async { super::super::engine::GatewayResolution::Error(super::super::errors::thread_tool_failure("not_found", "unused", "none", None)) })),
            endpoints: super::super::adapter::GatewayEndpointPort::inert(),
            locate: Arc::new(|_| Box::pin(async { None })),
            notify: None,
            log: None,
        }));
        let names: Vec<&str> = component.tools.iter().map(|tool| tool.name.as_str()).collect();
        for expected in ["thread_send", "thread_handoff", "thread_report", "thread_bind", "thread_bindings", "thread_outbox", "thread_answer"] {
            assert!(names.contains(&expected), "missing {expected}");
        }
        assert!(component.execute("thread_missing", "call-1", Value::Null).is_none());
    }
}
