//! Native registration of the durable binding/reporting surface.
//! Cross-session admission requires a host adapter; it is not simulated here.
use super::adapter::{GatewayEndpointPort, RuntimePhase, SessionAdmissionGate, SessionRuntimePort};
use super::component::{GatewayThreadComponentOptions, create_gateway_thread_component};
use super::engine::GatewayResolution;
use super::store::{GatewayStoreOptions, create_gateway_store};
use super::types::{AdmissionLedger, Provenance};
use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, ToolDefinition, ToolResult};
use std::sync::Arc;

pub struct NativeThreadComponent;

impl Extension for NativeThreadComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let runtime = api.runtime.clone();
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_, ctx| {
                let runtime = runtime.clone();
                Box::pin(async move {
                    let store = create_gateway_store(GatewayStoreOptions::new(
                        ctx.agent_dir.to_string_lossy(),
                    ))
                    .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                    let caller = ctx.session_manager.session_id().to_owned();
                    let project = ctx.cwd.to_string_lossy().into_owned();
                    let component =
                        create_gateway_thread_component(Arc::new(GatewayThreadComponentOptions {
                            store,
                            runtime: SessionRuntimePort {
                                phase: Arc::new(|| RuntimePhase::Idle),
                                admission_gate: Arc::new(|| SessionAdmissionGate {
                                    can_admit: false,
                                    hold_reason: Some(
                                        "native host admission is not connected".into(),
                                    ),
                                    editor_revision: 0,
                                    turn_epoch: 0,
                                }),
                                admit_external_message: Arc::new(|_| {
                                    Err("native host admission is not connected".into())
                                }),
                                list_admitted_deliveries: Arc::new(AdmissionLedger::default),
                            },
                            durable_id: caller.clone(),
                            session_path: Arc::new(|| None),
                            caller_session_id: Arc::new(move || caller.clone()),
                            caller_name: None,
                            caller_provenance: Arc::new(move || Provenance {
                                workspace: "unclassified".into(),
                                project: project.clone(),
                                lane: None,
                                session: None,
                            }),
                            resolve: Arc::new(|_, _| {
                                Box::pin(async {
                                    GatewayResolution::Error(super::errors::thread_tool_failure(
                                        "host_unavailable",
                                        "Native thread addressing is not connected",
                                        "Use a host with thread addressing",
                                        None,
                                    ))
                                })
                            }),
                            endpoints: GatewayEndpointPort::inert(),
                            locate: Arc::new(|_| Box::pin(async { None })),
                            notify: None,
                            log: Some(Arc::new(maho_ext_api::diagnostic)),
                        }));
                    let mut session_api = ExtensionApi::new(
                        maho_ext_api::LoadedExtension::new(
                            "thread",
                            ctx.cwd.clone(),
                            Default::default(),
                        ),
                        Default::default(),
                        Default::default(),
                        runtime,
                    );
                    for tool in component.tools.into_iter().filter(|tool| {
                        matches!(
                            tool.name.as_str(),
                            "thread_report" | "thread_bind" | "thread_bindings" | "thread_outbox"
                        )
                    }) {
                        let execute = tool.execute;
                        session_api.try_register_tool(ToolDefinition::new(
                            &tool.name,
                            &tool.description,
                            tool.parameters,
                            Arc::new(move |call| {
                                let execute = execute.clone();
                                Box::pin(async move {
                                    let result = execute(call.id, call.params).await;
                                    let mut output = ToolResult::text(result.to_string());
                                    output.details = Some(serde_json::json!({"result": result}));
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
}
