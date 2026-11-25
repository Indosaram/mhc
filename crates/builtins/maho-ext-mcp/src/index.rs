use crate::{
    host_registry::HostMcpRegistry,
    service::{McpAttachOptions, McpService},
};
use maho_ext_api::{
    BeforeAgentStartEventResult, EventKind, EventResult, ExtensionApi, ExtensionEvent,
    ExtensionFailure, ExtensionHandlerOptions,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

type ScopeAttachRx = tokio::sync::watch::Receiver<Option<Result<(), String>>>;

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
    let attached_session_ids: Arc<Mutex<BTreeSet<String>>> = Arc::default();
    let wire_status_sink: Arc<dyn Fn(&crate::service_types::McpWireStatusSnapshot) + Send + Sync> = {
        let events = publish_events.clone();
        Arc::new(move |snapshot| {
            // Dropped by design when the runtime went stale before the update landed.
            let _stale = publish_wire_status(&events, snapshot);
        })
    };
    let inventory = crate::control_inventory::forward_mcp_control_inventory(
        &api.events,
        &service,
        attached_session_ids.clone(),
        Some(wire_status_sink),
    );
    let inventory = Arc::new(Mutex::new(Some(inventory)));
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
                Box::pin(async move { wait_for_startup_registration(&service).await })
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
                // A reload can invalidate this runtime while the reconnect awaits; the
                // stale snapshot then has no subscriber and is dropped.
                let _stale = publish_wire_status(&command_events, &snapshot);
            })),
            open_browser: Some(crate::auth::challenge_login::system_browser_opener()),
        },
    );

    let skill_reveal = Arc::new(crate::skill_reveal::create_mcp_skill_reveal(
        Arc::new(Vec::new),
        Arc::new(|_| {}),
    ));
    let attach_publisher: Arc<Mutex<Option<tokio::task::AbortHandle>>> = Arc::default();
    let binding: Arc<Mutex<Option<Arc<crate::service::McpSessionBinding>>>> = Arc::default();
    let session_started = Arc::new(AtomicBool::new(false));
    let attach_started = Arc::new(AtomicBool::new(false));
    let attached_contexts: Arc<Mutex<BTreeSet<String>>> = Arc::default();
    let scope_attaches: Arc<Mutex<BTreeMap<String, ScopeAttachRx>>> = Arc::default();

    // Input: rebind presentation for this scope and reveal `/skill:<name>` if present.
    {
        let input_binding = binding.clone();
        let input_reveal = skill_reveal.clone();
        let input_service = service.clone();
        api.on(
            EventKind::Input,
            Arc::new(move |event, ctx| {
                let binding = input_binding
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let reveal = input_reveal.clone();
                let service = input_service.clone();
                let session_id = ctx.session_manager.session_id().to_owned();
                let text = match &event {
                    ExtensionEvent::Input(input) => Some(input.text.clone()),
                    _ => None,
                };
                Box::pin(async move {
                    if let Some(binding) = binding {
                        binding.bind_presentation_for(&ctx.ui, Some(&session_id));
                    }
                    if let Some(text) = text
                        && let Some(skill_name) = parse_skill_slash_command(&text)
                        && let Some(declarations) = reveal.take_reveal(&session_id, &skill_name)
                    {
                        let service = service.lock().await;
                        let registered = service.tier_b_searchable();
                        let targets = crate::skills::skill_activation_targets(
                            &declarations,
                            &skill_name,
                            &registered,
                        );
                        if !targets.is_empty() {
                            service.activate_skill_mcp_tools(&targets);
                        }
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }

    // ToolCall: (1) first-use attach / scope-join for session-less runtimes (senpi#5229 / S16c);
    // (2) reveal skill MCP tools when `read` opens a listed skill's SKILL.md.
    {
        let tc_started = session_started.clone();
        let tc_attach_started = attach_started.clone();
        let tc_contexts = attached_contexts.clone();
        let tc_sessions = attached_session_ids.clone();
        let tc_scopes = scope_attaches.clone();
        let tc_service = service.clone();
        let tc_registrar = registrar.clone();
        let tc_search = tool_search.clone();
        let tc_events = publish_events.clone();
        let tc_binding = binding.clone();
        let tc_reveal = skill_reveal.clone();
        api.on(
            EventKind::ToolCall,
            Arc::new(move |event, ctx| {
                let session_id = ctx.session_manager.session_id().to_owned();
                let started = tc_started.clone();
                let attach_started = tc_attach_started.clone();
                let contexts = tc_contexts.clone();
                let sessions = tc_sessions.clone();
                let scopes = tc_scopes.clone();
                let service_handle = tc_service.clone();
                let registrar = tc_registrar.clone();
                let tool_search = tc_search.clone();
                let events = tc_events.clone();
                let binding = tc_binding.clone();
                let reveal = tc_reveal.clone();
                let read_path = match &event {
                    ExtensionEvent::ToolCall(call) if call.tool_name == "read" => call
                        .input
                        .get("path")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    _ => None,
                };
                Box::pin(async move {
                    if !started.load(Ordering::SeqCst) {
                        let first_for_context = contexts
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .insert(session_id.clone());
                        if first_for_context {
                            sessions
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .insert(session_id.clone());
                            let (tx, rx) = tokio::sync::watch::channel(None);
                            scopes
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .insert(session_id.clone(), rx);
                            let is_primary = !attach_started.swap(true, Ordering::SeqCst);
                            let service_for_task = service_handle.clone();
                            let ctx_for_task = ctx.clone();
                            let registrar = registrar.clone();
                            let tool_search = tool_search.clone();
                            let events = events.clone();
                            let binding = binding.clone();
                            let session_id_for_task = session_id.clone();
                            tokio::spawn(async move {
                                let outcome = if is_primary {
                                    attach_context(
                                        &service_for_task,
                                        &ctx_for_task,
                                        registrar,
                                        tool_search,
                                        events,
                                        binding,
                                    )
                                    .await
                                    .map_err(|error| error.message)
                                } else {
                                    let mut service = service_for_task.lock().await;
                                    service
                                        .attach_scope(
                                            &session_id_for_task,
                                            if ctx_for_task.has_ui {
                                                Some(ctx_for_task.ui.clone())
                                            } else {
                                                None
                                            },
                                            ctx_for_task.has_ui,
                                        )
                                        .await;
                                    Ok(())
                                };
                                tx.send_replace(Some(outcome));
                            });
                        }
                    }
                    if let Some(path) = read_path {
                        let mut to_reveal = Vec::new();
                        for (name, skill) in reveal.skills(&session_id) {
                            let skill_path = skill.file_path.to_string_lossy();
                            if path == skill_path.as_ref() || path.ends_with(skill_path.as_ref()) {
                                to_reveal.push(name);
                            }
                        }
                        for name in to_reveal {
                            if let Some(declarations) = reveal.take_reveal(&session_id, &name) {
                                let service = service_handle.lock().await;
                                let registered = service.tier_b_searchable();
                                let targets = crate::skills::skill_activation_targets(
                                    &declarations,
                                    &name,
                                    &registered,
                                );
                                if !targets.is_empty() {
                                    service.activate_skill_mcp_tools(&targets);
                                }
                            }
                        }
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }

    // SessionStart
    {
        let start_binding = binding.clone();
        let start = service.clone();
        let start_registrar = registrar.clone();
        let start_search = tool_search.clone();
        let start_events = publish_events.clone();
        let start_publisher = attach_publisher.clone();
        let start_sessions = attached_session_ids.clone();
        let start_contexts = attached_contexts.clone();
        let start_scopes = scope_attaches.clone();
        let start_session_flag = session_started.clone();
        let start_attach_flag = attach_started.clone();
        api.on(
            EventKind::SessionStart,
            Arc::new(move |event, ctx| {
                start_session_flag.store(true, Ordering::SeqCst);
                // index.ts: a sizes-only diagnostic startup skips the built-in MCP attach entirely.
                if matches!(event, ExtensionEvent::SessionStart(start) if start.sizes_only) {
                    return Box::pin(async { Ok(EventResult::None) });
                }
                let session_id = ctx.session_manager.session_id().to_owned();
                {
                    let mut sessions = start_sessions
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    sessions.clear();
                    sessions.insert(session_id.clone());
                }
                start_contexts
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(session_id.clone());
                start_attach_flag.store(true, Ordering::SeqCst);
                let (tx, rx) = tokio::sync::watch::channel(None);
                start_scopes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(session_id, rx);
                let start_binding = start_binding.clone();
                let service_handle = start.clone();
                let registrar = start_registrar.clone();
                let tool_search = start_search.clone();
                let events = start_events.clone();
                let attach_publisher = start_publisher.clone();
                Box::pin(async move {
                    let result = attach_context(
                        &service_handle,
                        ctx,
                        registrar,
                        tool_search,
                        events.clone(),
                        start_binding,
                    )
                    .await;
                    tx.send_replace(Some(result.clone().map_err(|error| error.message)));
                    result?;
                    if ctx.mode != maho_ext_api::ExtensionMode::Rpc {
                        // Non-RPC sessions still publish the settled snapshot after the deferred race settles.
                        let publish_service = service_handle.clone();
                        let publisher = tokio::spawn(async move {
                            wait_for_startup_registration(&publish_service).await;
                            let service = publish_service.lock().await;
                            let snapshot = service.wire_status_snapshot().await;
                            // Dropped by design when the runtime went stale before the attach settled.
                            let _stale = publish_wire_status(&events, &snapshot);
                        });
                        if let Some(previous) = attach_publisher
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .replace(publisher.abort_handle())
                        {
                            previous.abort();
                        }
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }

    // SessionShutdown
    {
        let shutdown = service.clone();
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_, _| {
                if let Some(forwarder) = inventory
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    forwarder.dispose();
                }
                if let Some(publisher) = attach_publisher
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    publisher.abort();
                }
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
    }

    // BeforeAgentStart: prompt preparation reuses the in-flight attach (or attaches lazily on real turns),
    // then reads skill declarations before composing the known MCP instructions.
    {
        let before = service.clone();
        let before_events = publish_events.clone();
        let before_scopes = scope_attaches;
        let before_attach_flag = attach_started;
        let before_registrar = registrar;
        let before_search = tool_search;
        let before_binding = binding;
        let before_reveal = skill_reveal;
        api.on_with_options(
            EventKind::BeforeAgentStart,
            Arc::new(move |event, ctx| {
                let service = before.clone();
                let events = before_events.clone();
                let scopes = before_scopes.clone();
                let attach_flag = before_attach_flag.clone();
                let registrar = before_registrar.clone();
                let tool_search = before_search.clone();
                let binding = before_binding.clone();
                let reveal = before_reveal.clone();
                Box::pin(async move {
                    let ExtensionEvent::BeforeAgentStart(event) = event else {
                        return Ok(EventResult::None);
                    };
                    let session_id = ctx.session_manager.session_id().to_owned();
                    let own_attach = scopes
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .get(&session_id)
                        .cloned();
                    if let Some(mut rx) = own_attach {
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_millis(
                                crate::startup_race::MCP_ATTACH_SETTLE_TIMEOUT_MS,
                            ),
                            async {
                                while rx.borrow().is_none() {
                                    if rx.changed().await.is_err() {
                                        break;
                                    }
                                }
                            },
                        )
                        .await;
                    }
                    if !event.preview && !attach_flag.swap(true, Ordering::SeqCst) {
                        attach_context(
                            &service,
                            ctx,
                            registrar,
                            tool_search,
                            events.clone(),
                            binding,
                        )
                        .await?;
                    }
                    wait_for_startup_registration(&service).await;
                    if !event.preview {
                        let mut service_guard = service.lock().await;
                        service_guard.set_elicitation_ui(if ctx.has_ui {
                            Some(ctx.ui.clone())
                        } else {
                            None
                        });
                        service_guard.bind_mcp_mrtr_presentation_for(&ctx.ui, Some(&session_id));
                        if !event.system_prompt_options.skills.is_empty() {
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
                                            project_root: provenance
                                                .project_root
                                                .clone()
                                                .map(Into::into),
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
                            let declarations = reveal.start_turn(&session_id, &skills);
                            let mut warnings = declarations.warnings.clone();
                            if !declarations.servers.is_empty() {
                                warnings.extend(
                                    service_guard
                                        .attach_skill_mcp_servers(&declarations)
                                        .await
                                        .map_err(|error| {
                                            ExtensionFailure::new(error.to_string())
                                        })?,
                                );
                            }
                            publish_skill_warnings(&events, &warnings);
                        }
                    }
                    let service_guard = service.lock().await;
                    let block =
                        crate::instructions::refresh_mcp_instructions_for_session(&service_guard)
                            .await;
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
            ExtensionHandlerOptions {
                preview_safe: true,
                run_last: true,
                ..Default::default()
            },
        );
    }

    service
}

async fn wait_for_startup_registration(service_handle: &tokio::sync::Mutex<McpService>) {
    let (deferred, registration) = {
        let service = service_handle.lock().await;
        (service.deferred_attach(), service.deferred_registration())
    };
    // Registration takes the service lock and may be enqueued by a settling connect.
    let timeout =
        std::time::Duration::from_millis(crate::startup_race::MCP_ATTACH_SETTLE_TIMEOUT_MS);
    deferred.wait(timeout).await;
    registration.wait(timeout).await;
}

async fn attach_context(
    service_handle: &Arc<tokio::sync::Mutex<McpService>>,
    ctx: &maho_ext_api::ExtensionContext,
    registrar: Arc<dyn crate::tool_registrar::McpToolRegistrar>,
    tool_search: Option<Arc<tokio::sync::Mutex<maho_ext_tool_search::service::ToolSearchService>>>,
    events: maho_ext_api::EventBus,
    binding: Arc<Mutex<Option<Arc<crate::service::McpSessionBinding>>>>,
) -> Result<(), ExtensionFailure> {
    let env = std::env::vars().collect();
    let declarations = if ctx.extension_connects_mcp_servers() {
        Vec::new()
    } else {
        ctx.get_registered_mcp_servers()
    };
    let mut service = service_handle.lock().await;
    let ui = if ctx.has_ui {
        Some(ctx.ui.clone())
    } else {
        None
    };
    service.set_elicitation_ui(ui.clone());
    service.set_session_mode(ctx.mode);
    service.set_policy_notice_event(Arc::new(move |notice| {
        // Notices from a connect that outlived a reload are dropped with the runtime.
        let _stale = events.try_emit("security_notice", &notice);
    }));
    service.bind_self(Arc::downgrade(service_handle));
    service.bind_registration(registrar, tool_search);
    service
        .attach_session_with_options(McpAttachOptions {
            cwd: &ctx.cwd,
            agent_dir: &ctx.agent_dir,
            env: &env,
            project_trusted: ctx.is_project_trusted(),
            declarations: &declarations,
            mode: Some(ctx.mode),
            session_id: Some(ctx.session_manager.session_id()),
            ui,
            has_ui: ctx.has_ui,
            extension_connects_mcp_servers: ctx.extension_connects_mcp_servers(),
        })
        .await
        .map_err(|error| ExtensionFailure::new(error.to_string()))?;
    service.bind_mcp_mrtr_presentation_for(&ctx.ui, Some(ctx.session_manager.session_id()));
    *binding
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(service.session_binding());
    Ok(())
}

fn parse_skill_slash_command(text: &str) -> Option<String> {
    static SKILL_CMD: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^\s*/skill:([A-Za-z0-9._-]+)").expect("skill command regex")
    });
    SKILL_CMD
        .captures(text)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_owned())
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
///
/// # Errors
/// Returns the stale-runtime failure, without publishing, when the bus belongs to a runtime that a
/// reload or session replacement invalidated: that runtime's subscribers are gone, so the snapshot
/// is dropped by design instead of panicking the background task that produced it.
pub fn publish_wire_status(
    events: &maho_ext_api::EventBus,
    snapshot: &crate::service_types::McpWireStatusSnapshot,
) -> Result<(), ExtensionFailure> {
    let erased: std::sync::Arc<dyn std::any::Any + Send + Sync> =
        std::sync::Arc::new(snapshot.clone());
    events.try_emit_native::<std::sync::Arc<dyn std::any::Any + Send + Sync>>(
        maho_core::agent_session::MCP_WIRE_STATUS_CHANGED_EVENT,
        &erased,
    )
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
