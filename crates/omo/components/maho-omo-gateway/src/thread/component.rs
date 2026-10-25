//! The thread component: it wires the store, engine, relay, drain and tools of one session.
//!
//! Upstream counterpart: `thread/component.ts`.

pub type LocateCallback = Arc<
        dyn Fn(&str) -> super::adapter::BoxFuture<Option<super::adapter::GatewayEndpointRef>>
            + Send
            + Sync,
    >;
pub type NotifyCallback = Option<Arc<dyn Fn(&str) + Send + Sync>>;
use std::sync::Arc;

use serde_json::Value;

use super::adapter::SessionRuntimePort;
use super::drain::{InboxDrain, InboxDrainOptions};
use super::engine::{GatewayEngine, GatewayEngineOptions, GatewayResolve};
use super::relay::{GatewayRelay, GatewayRelayOptions};
use super::store_api::GatewayStoreApi;
use super::tools::{ThreadTool, ThreadToolsOptions, build_extension_tools, create_thread_tools};

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
    pub locate: LocateCallback,
    pub notify: NotifyCallback,
    pub log: NotifyCallback,
}

/// One session's gateway: its drain, its relay and the tools its model calls.
#[derive(Clone)]
pub struct GatewayThreadComponent {
    pub store: Arc<dyn GatewayStoreApi>,
    pub engine: GatewayEngine,
    pub relay: GatewayRelay,
    pub drain: Arc<InboxDrain>,
    pub tools: Vec<ThreadTool>,
    pub durable_id: String,
    registration: Arc<std::sync::Mutex<super::tools::ExtensionToolRegistration>>,
    log: NotifyCallback,
}

pub fn create_gateway_thread_component(
    options: Arc<GatewayThreadComponentOptions>,
) -> GatewayThreadComponent {
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
    if let Some(log) = options.log.clone() {
        options.store.on_after_commit_error(Arc::new(move |extension, error| log(&format!("thread gateway: extension {extension} committed, but an after-commit effect (its wake hint) failed: {error}"))));
    }
    GatewayThreadComponent {
        registration: Default::default(),
        log: options.log.clone(),
        store: options.store.clone(),
        engine,
        relay,
        drain,
        tools,
        durable_id: options.durable_id.clone(),
    }
}

impl maho_ext_api::Extension for GatewayThreadComponent {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        Arc::new(self.clone()).register_session_tools(api);
    }
}

impl GatewayThreadComponent {
    /// The session's own extension tools, on top of the thread tools.
    pub fn tools_with_extensions(
        &self,
        caller: Arc<dyn Fn() -> String + Send + Sync>,
    ) -> Vec<ThreadTool> {
        let mut tools = self.tools.clone();
        let resolve = self.engine.options().resolve.clone();
        let resolver: super::tools::CallerResolver = Arc::new(move |runtime_id| {
            let resolve = resolve.clone();
            Box::pin(
                async move { super::tools::resolve_caller_durable_id(&resolve, &runtime_id).await },
            )
        });
        let declared = build_extension_tools(self.store.clone(), caller, resolver);
        let present = tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        let log = self.log.clone();
        tools.extend(
            self.registration
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .session_start(declared, &present, &|line| {
                    if let Some(log) = &log {
                        log(line);
                    } else {
                        maho_ext_api::diagnostic(line);
                    }
                }),
        );
        tools
    }

    /// Session-start registration reads declarations only; extension modules load on execution.
    pub fn register_session_tools(self: &Arc<Self>, api: &mut maho_ext_api::ExtensionApi) {
        use maho_ext_api::*;
        let component = Arc::clone(self);
        let runtime = api.runtime.clone();
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_, ctx| {
                let component = Arc::clone(&component);
                let runtime = runtime.clone();
                Box::pin(async move {
                    let mut session_api = ExtensionApi::new(
                        LoadedExtension::new(
                            "thread-session-tools",
                            ctx.cwd.clone(),
                            SourceInfo::default(),
                        ),
                        ExtensionSessionProfile::default(),
                        EventBus::default(),
                        runtime,
                    );
                    let present = session_api
                        .get_all_tools()?
                        .into_iter()
                        .map(|tool| tool.name)
                        .collect::<Vec<_>>();
                    let caller = ctx.session_manager.session_id().to_owned();
                    let resolve = component.engine.options().resolve.clone();
                    let resolver: super::tools::CallerResolver = Arc::new(move |id| {
                        let resolve = resolve.clone();
                        Box::pin(async move {
                            super::tools::resolve_caller_durable_id(&resolve, &id).await
                        })
                    });
                    let tools = build_extension_tools(
                        component.store.clone(),
                        Arc::new(move || caller.clone()),
                        resolver,
                    );
                    let admitted = component
                        .registration
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .session_start(tools, &present, &|line| {
                            if let Some(log) = &component.log {
                                log(line);
                            } else {
                                diagnostic(line);
                            }
                        });
                    for tool in admitted {
                        let execute = tool.execute.clone();
                        session_api.try_register_tool(ToolDefinition::new(
                            &tool.name,
                            &tool.description,
                            tool.parameters,
                            Arc::new(move |call| {
                                let execute = execute.clone();
                                Box::pin(async move {
                                    let result = execute(call.id, call.params).await;
                                    let mut output = ToolResult::text(result.to_string());
                                    output.details = Some(serde_json::json!({"result":result}));
                                    Ok(output)
                                })
                            }),
                        ))?;
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }

    /// Open a native child and record its engine-resolved creator only after success.
    pub async fn thread_create(
        &self,
        surface: &super::live_surface::LiveThreadSurface,
        runtime_id: &str,
        params: Value,
        name: Option<&str>,
    ) -> Result<Value, String> {
        let opened = surface.open_session(params, name).await?;
        let id = opened
            .get("durableSessionId")
            .or_else(|| opened.get("durable_session_id"))
            .and_then(Value::as_str)
            .ok_or_else(|| "opened session has no durable identity".to_owned())?;
        let result = serde_json::json!({"kind":"ok", "thread":{"thread_id":id}, "session":opened});
        super::tools::record_thread_creator(
            self.store.as_ref(),
            &self.engine.options().resolve,
            runtime_id,
            &result,
        )
        .await
        .map_err(|error| error.message())?;
        Ok(result)
    }

    /// Publishes this session's control endpoint and registers the drain's first pass.
    pub fn start(
        &self,
        incarnation: &str,
        endpoint_socket: Option<&str>,
        endpoint_kind: Option<&str>,
    ) -> Result<(), super::store_ops::StoreError> {
        self.store.register_incarnation(
            &self.durable_id,
            incarnation,
            endpoint_socket,
            endpoint_kind,
        )?;
        self.drain.drain(&super::adapter::DrainWakeEvent {
            reason: super::adapter::WakeReason::Start,
            delivery_ids: None,
        })?;
        Ok(())
    }

    /// Retires the drain and clears the published endpoint.
    pub fn stop(&self, incarnation: &str) -> Result<(), super::store_ops::StoreError> {
        self.store.clear_endpoint(&self.durable_id, incarnation)
    }

    /// A tool call by name, as a host dispatches it.
    pub fn execute(
        &self,
        name: &str,
        call_id: &str,
        args: Value,
    ) -> Option<super::adapter::BoxFuture<Value>> {
        self.tools
            .iter()
            .find(|tool| tool.name == name)
            .map(|tool| (tool.execute)(call_id, args))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tool_set_covers_send_report_and_bindings() {
        let store: Arc<dyn GatewayStoreApi> = super::super::store::create_gateway_store(
            super::super::store::GatewayStoreOptions::new(
                std::env::temp_dir()
                    .join("maho-component-test")
                    .to_string_lossy()
                    .into_owned(),
            ),
        )
        .expect("store");
        let runtime = SessionRuntimePort {
            phase: Arc::new(|| super::super::adapter::RuntimePhase::Idle),
            admission_gate: Arc::new(|| super::super::adapter::SessionAdmissionGate {
                can_admit: true,
                hold_reason: None,
                editor_revision: 1,
                turn_epoch: 1,
            }),
            admit_external_message: Arc::new(|_| Err("not in this test".to_owned())),
            list_admitted_deliveries: Arc::new(super::super::types::AdmissionLedger::default),
        };
        let component = create_gateway_thread_component(Arc::new(GatewayThreadComponentOptions {
            store,
            runtime,
            durable_id: "receiver".into(),
            session_path: Arc::new(|| None),
            caller_session_id: Arc::new(|| "sender".into()),
            caller_name: None,
            caller_provenance: Arc::new(|| super::super::types::Provenance {
                workspace: "unclassified".into(),
                project: "test".into(),
                lane: None,
                session: None,
            }),
            resolve: Arc::new(|_, _| {
                Box::pin(async {
                    super::super::engine::GatewayResolution::Error(
                        super::super::errors::thread_tool_failure(
                            "not_found",
                            "unused",
                            "none",
                            None,
                        ),
                    )
                })
            }),
            endpoints: super::super::adapter::GatewayEndpointPort::inert(),
            locate: Arc::new(|_| Box::pin(async { None })),
            notify: None,
            log: None,
        }));
        let names: Vec<&str> = component
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        for expected in [
            "thread_send",
            "thread_handoff",
            "thread_report",
            "thread_bind",
            "thread_bindings",
            "thread_outbox",
            "thread_answer",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
        assert!(
            component
                .execute("thread_missing", "call-1", Value::Null)
                .is_none()
        );
    }
}
