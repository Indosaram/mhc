use crate::{host_registry::HostMcpRegistry, service::McpService};
use maho_ext_api::{
    BeforeAgentStartEventResult, EventKind, EventResult, ExtensionApi, ExtensionEvent,
    ExtensionFailure,
};
use std::sync::Arc;
pub fn register_mcp_lifecycle(
    api: &mut ExtensionApi,
    registry: Arc<HostMcpRegistry>,
    owner: u64,
) -> Arc<tokio::sync::Mutex<McpService>> {
    register_mcp_lifecycle_with_tool_search(api, registry, owner, None)
}
pub fn register_mcp_lifecycle_with_tool_search(
    api: &mut ExtensionApi,
    registry: Arc<HostMcpRegistry>,
    owner: u64,
    tool_search: Option<Arc<tokio::sync::Mutex<maho_ext_tool_search::service::ToolSearchService>>>,
) -> Arc<tokio::sync::Mutex<McpService>> {
    let service = Arc::new(tokio::sync::Mutex::new(McpService::new(registry, owner)));
    let publish_events = api.events.clone();
    let inventory_service = service.clone();
    let inventory=api.events.on_native::<crate::control_inventory::McpControlInventoryRequest>(crate::control_inventory::MCP_CONTROL_INVENTORY_REQUEST_EVENT,Arc::new(move |request| {
        let sender=request.respond.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        let service=inventory_service.clone();
        if let Some(sender)=sender { tokio::spawn(async move { let snapshot=service.lock().await.wire_status_snapshot().await; match serde_json::to_value(snapshot) { Ok(snapshot) => { if sender.send(snapshot).is_err() { /* Requesting connection was disposed. */ } }, Err(error) => maho_ext_api::diagnostic(&format!("MCP inventory serialization failed: {error}")) } }); }
    }));
    let inventory = Arc::new(std::sync::Mutex::new(Some(inventory)));
    let registrar: Arc<dyn crate::tool_registrar::McpToolRegistrar> = Arc::new(
        crate::tool_registrar::SessionMcpToolRegistrar::from_api(api),
    );
    let command_events = publish_events.clone();
    let pending_service = service.clone();
    let entry_api = Arc::new(ExtensionApi::new(
        api.registered.clone(),
        api.profile.clone(),
        api.events.clone(),
        api.runtime.clone(),
    ));
    crate::commands::register_mcp_commands(
        api,
        crate::commands::McpCommandHost {
            service: service.clone(),
            pending_attach: Arc::new(move || {
                let service = pending_service.clone();
                Box::pin(async move {
                    let deferred = service.lock().await.deferred_attach();
                    deferred
                        .wait(std::time::Duration::from_millis(
                            crate::startup_race::MCP_ATTACH_SETTLE_TIMEOUT_MS,
                        ))
                        .await;
                })
            }),
            append_entry: Some(Arc::new(move |message| {
                if let Err(error) = entry_api.append_entry(
                    crate::commands::MCP_AUTH_ENTRY_TYPE,
                    Some(serde_json::Value::String(message.into())),
                ) {
                    maho_ext_api::diagnostic(&format!("MCP auth notice failed: {}", error.message));
                }
            })),
            after_reconnect: Some(Arc::new(move |snapshot| {
                publish_wire_status(&command_events, &snapshot);
            })),
            open_browser: Some(crate::auth::challenge_login::system_browser_opener()),
        },
    );
    let start = service.clone();
    let start_registrar = registrar.clone();
    let start_search = tool_search.clone();
    let start_events = publish_events.clone();
    // The live binding, so the input hook rebinds presentation without waiting on an attach in flight.
    let binding: Arc<std::sync::Mutex<Option<Arc<crate::service::McpSessionBinding>>>> =
        Arc::default();
    let input_binding = binding.clone();
    api.on(
        EventKind::Input,
        Arc::new(move |_, ctx| {
            let binding = input_binding
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            Box::pin(async move {
                // Upstream `pi.on("input")`: `service.bindMcpMrtrPresentation(pi, ctx.ui)` on every input.
                if let Some(binding) = binding {
                    binding.bind_presentation(&ctx.ui);
                }
                Ok(EventResult::None)
            })
        }),
    );
    api.on(
        EventKind::SessionStart,
        Arc::new(move |event, ctx| {
            // index.ts: a sizes-only diagnostic startup skips the built-in MCP attach entirely.
            if matches!(event, ExtensionEvent::SessionStart(start) if start.sizes_only) {
                return Box::pin(async { Ok(EventResult::None) });
            }
            let start_binding = binding.clone();
            let service_handle = start.clone();
            let registrar = start_registrar.clone();
            let tool_search = start_search.clone();
            let events = start_events.clone();
            Box::pin(async move {
                let env = std::env::vars().collect();
                let mut service = service_handle.lock().await;
                service.set_elicitation_ui(if ctx.has_ui {
                    Some(ctx.ui.clone())
                } else {
                    None
                });
                service.set_session_mode(ctx.mode);
                // Upstream `sessionManager: binding.context.sessionManager`: tools registered for
                // this binding serve only calls from this session.
                service
                    .session_binding()
                    .bind_session(ctx.session_manager.session_id());
                // Bind the registration seam once; `attach_session` registers through it
                // (upstream `attachSession`) and the deferred startup connects re-use it.
                service.set_policy_notice_event(Arc::new({
                    let events = events.clone();
                    move |notice| {
                        events.emit("security_notice", &notice);
                    }
                }));
                service.bind_self(Arc::downgrade(&service_handle));
                service.bind_registration(registrar, tool_search);
                service
                    .attach_session(
                        &ctx.cwd,
                        &ctx.agent_dir,
                        &env,
                        ctx.is_project_trusted(),
                        &ctx.registered_mcp_servers,
                    )
                    .await
                    .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                // Upstream `bindMcpMrtrPresentation(pi, ctx.ui)` after attach: the binding's MRTR presentation.
                service.bind_mcp_mrtr_presentation(&ctx.ui);
                *start_binding
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(service.session_binding());
                drop(service);
                // Attach no longer completes inside session_start, so publish the session-owned snapshot on
                // the bound runner bus once the deferred startup connect settles (pinned
                // `onWireStatusChanged`); later changes publish via the reconnect/command paths.
                let publish_service = service_handle.clone();
                tokio::spawn(async move {
                    // PERF-DIVERGENCE(senpi mcp index.ts): upstream awaits attach settle without any lock; this port wrapped the wait in the service Mutex guard, blocking other lockers up to 5s. Now the guard is dropped before awaiting. Output is identical.
                    let deferred = publish_service.lock().await.deferred_attach();
                    deferred
                        .wait(std::time::Duration::from_millis(
                            crate::startup_race::MCP_ATTACH_SETTLE_TIMEOUT_MS,
                        ))
                        .await;
                    let service = publish_service.lock().await;
                    let snapshot = service.wire_status_snapshot().await;
                    publish_wire_status(&events, &snapshot);
                });
                Ok(EventResult::None)
            })
        }),
    );
    let shutdown = service.clone();
    api.on(
        EventKind::SessionShutdown,
        Arc::new(move |_, _| {
            inventory
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            let service = shutdown.clone();
            Box::pin(async move {
                service
                    .lock()
                    .await
                    .dispose()
                    .await
                    .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                Ok(EventResult::None)
            })
        }),
    );
    // Prompt preparation reuses the in-flight attach, then reads skill declarations on real
    // turns (never previews) before composing the known MCP instructions.
    let before = service.clone();
    let before_events = publish_events.clone();
    api.on(
        EventKind::BeforeAgentStart,
        Arc::new(move |event, _| {
            let service = before.clone();
            let events = before_events.clone();
            Box::pin(async move {
                let ExtensionEvent::BeforeAgentStart(event) = event else {
                    return Ok(EventResult::None);
                };
                // PERF-DIVERGENCE(senpi mcp index.ts): upstream awaits `whenAttachSettled` lock-free; this port held the service Mutex across the wait. Now the guard is dropped before awaiting. Output is identical.
                let deferred = service.lock().await.deferred_attach();
                deferred
                    .wait(std::time::Duration::from_millis(
                        crate::startup_race::MCP_ATTACH_SETTLE_TIMEOUT_MS,
                    ))
                    .await;
                let mut service = service.lock().await;
                if !event.preview && !event.system_prompt_options.skills.is_empty() {
                    let skills = event
                        .system_prompt_options
                        .skills
                        .iter()
                        .map(|skill| crate::skills::SkillLike {
                            name: skill.name.clone(),
                            file_path: skill.file_path.clone().into(),
                            base_dir: skill.base_dir.clone().into(),
                            scope: Some(
                                match skill.source_info.scope {
                                    maho_ext_api::SourceScope::Project => "project",
                                    _ => "user",
                                }
                                .into(),
                            ),
                            provenance: skill.provenance.as_ref().map(|provenance| {
                                crate::skills::SkillProvenance {
                                    kind: provenance.kind.clone(),
                                    selected_root: provenance.selected_root.clone().into(),
                                    project_root: provenance.project_root.clone().map(Into::into),
                                }
                            }),
                            allowed_set: skill.allowed_set.as_ref().map(|allowed| {
                                crate::skills::SkillAllowedSet {
                                    roots: allowed
                                        .roots
                                        .iter()
                                        .map(|path| std::path::PathBuf::from(path.as_str()))
                                        .collect(),
                                    files: allowed
                                        .files
                                        .iter()
                                        .map(|path| std::path::PathBuf::from(path.as_str()))
                                        .collect(),
                                    digest: allowed.digest.clone(),
                                }
                            }),
                        })
                        .collect::<Vec<_>>();
                    let declarations = crate::skills::parse_skill_mcp_declarations(&skills);
                    let mut warnings = declarations.warnings.clone();
                    if !declarations.servers.is_empty() {
                        warnings.extend(
                            service
                                .attach_skill_mcp_servers(&declarations)
                                .await
                                .map_err(|error| ExtensionFailure::new(error.to_string()))?,
                        );
                    }
                    publish_skill_warnings(&events, &warnings);
                }
                let block =
                    crate::instructions::refresh_mcp_instructions_for_session(&service).await;
                Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult {
                    system_prompt_parts: None,
                    message: None,
                    system_prompt: crate::instructions::inject_mcp_instructions(
                        &block,
                        &event.system_prompt,
                    ),
                }))
            })
        }),
    );
    service
}
fn publish_skill_warnings(events: &maho_ext_api::EventBus, warnings: &[String]) {
    for warning in warnings {
        if !warning.starts_with("Project MCP server") {
            maho_ext_api::diagnostic(warning);
        }
        if warning.starts_with("Blocked skill link") {
            events.emit(
                "security_notice",
                &serde_json::json!({
                    "category": "skill-link-blocked", "message": warning,
                }),
            );
        }
    }
}

/// Publish a session-owned MCP wire-status snapshot on the bound extension runner bus, matching
/// the pinned `onWireStatusChanged` producer. Native payload; no JSON round-trip.
pub fn publish_wire_status(
    events: &maho_ext_api::EventBus,
    snapshot: &crate::service_types::McpWireStatusSnapshot,
) {
    let erased: std::sync::Arc<dyn std::any::Any + Send + Sync> =
        std::sync::Arc::new(snapshot.clone());
    events.emit_native::<std::sync::Arc<dyn std::any::Any + Send + Sync>>(
        maho_core::agent_session::MCP_WIRE_STATUS_CHANGED_EVENT,
        &erased,
    );
}
/// Crate-level extension factory (upstream `mcpExtension`/`createMcpExtension`).
/// The host builds it with the shared tool-search service and reads the returned
/// service handle to bind the native tool-search gate (`installMcpNativeToolSearchGate`).
pub struct McpExtension {
    pub registry: Arc<HostMcpRegistry>,
    pub owner: u64,
    pub tool_search:
        Option<Arc<tokio::sync::Mutex<maho_ext_tool_search::service::ToolSearchService>>>,
}
impl McpExtension {
    pub fn register_with_service(
        &self,
        api: &mut ExtensionApi,
    ) -> Arc<tokio::sync::Mutex<McpService>> {
        register_mcp_lifecycle_with_tool_search(
            api,
            self.registry.clone(),
            self.owner,
            self.tool_search.clone(),
        )
    }
}
impl maho_ext_api::Extension for McpExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let _ = self.register_with_service(api);
    }
}

#[cfg(test)]
mod security_notice_tests {
    use super::*;

    #[test]
    fn blocked_skill_warnings_publish_only_security_diagnostics() {
        let events = maho_ext_api::EventBus::default();
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = captured.clone();
        let _subscription = events.on(
            "security_notice",
            Arc::new(move |value| {
                sink.lock().unwrap().push(value.clone());
            }),
        );
        publish_skill_warnings(
            &events,
            &[
                "ordinary warning".into(),
                "Blocked skill link fixture".into(),
            ],
        );
        assert_eq!(
            *captured.lock().unwrap(),
            vec![serde_json::json!({
                "category":"skill-link-blocked", "message":"Blocked skill link fixture",
            })]
        );
    }
}
