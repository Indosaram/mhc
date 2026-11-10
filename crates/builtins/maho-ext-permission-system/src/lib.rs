pub mod always_scope;
pub mod arity;
pub mod auto_credentials;
pub mod auto_paths;
pub mod auto_policy;
pub mod auto_program_rules;
pub mod auto_shell_grammar;
pub mod auto_shell_segments;
pub mod bash_command_segments;
pub mod cli;
pub mod config;
pub mod dispatch;
pub mod dispatch_metadata;
pub mod dispatch_policy;
pub mod evaluate;
pub mod events;
pub mod external_dir;
pub mod external_pattern;
pub mod external_spellings;
pub mod internal_tools;
pub mod non_interactive;
pub mod parsers;
pub mod preset_registry;
pub mod prompt;
pub mod read_permission;
pub mod service;
pub mod settings;
pub mod storage;
pub mod types;
pub mod wildcard;

use maho_ext_api::{
    EventKind, EventResult, Extension, ExtensionApi, ExtensionContext, ExtensionEvent,
    ExtensionFailure, FlagType, FlagValue, ToolCallEvent, ToolCallEventResult,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
struct SessionPolicy {
    service: service::PermissionService,
    registry: parsers::ParserRegistry,
    preset_bound: bool,
    initial_approved: usize,
}
type PolicyState = Arc<Mutex<Option<SessionPolicy>>>;
fn home_dir() -> Result<String, ExtensionFailure> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|error| ExtensionFailure::new(error.to_string()))
}
fn lock_state(state: &PolicyState) -> std::sync::MutexGuard<'_, Option<SessionPolicy>> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
/// Rejects a request whose prompt was abandoned before it was answered.
struct PendingRequest<'a> {
    state: &'a PolicyState,
    id: Option<String>,
}
impl Drop for PendingRequest<'_> {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else { return };
        if let Some(policy) = lock_state(self.state).as_mut() {
            policy.service.reply(types::ReplyInput {
                request_id: id,
                reply: types::Reply::Reject,
                message: None,
                always: None,
            });
        }
    }
}
/// The `tool_call` authorization: every parsed request must be allowed, asked and approved, or the call is blocked.
async fn authorize_tool_call(
    state: &PolicyState,
    counter: &AtomicU64,
    event: &ToolCallEvent,
    ctx: &ExtensionContext,
) -> Result<Option<ToolCallEventResult>, ExtensionFailure> {
    let home = home_dir()?;
    // The parser may call back into the host (definition resolver, custom permission parsers): run it unlocked.
    let Some(registry) = lock_state(state)
        .as_ref()
        .map(|policy| policy.registry.clone())
    else {
        return Ok(None);
    };
    let requests = registry.parse(
        &event.tool_name,
        &event.input,
        (&ctx.cwd, std::path::Path::new(&home)),
    );
    for parsed in requests {
        let mut metadata = event.input.as_object().cloned().unwrap_or_default();
        metadata.insert("toolName".into(), event.tool_name.clone().into());
        let path = metadata
            .get("path")
            .or_else(|| metadata.get("file_path"))
            .cloned()
            .or_else(|| {
                let patch = metadata
                    .get("input")
                    .or_else(|| metadata.get("patchText"))?
                    .as_str()?;
                maho_ext_gpt_apply_patch::text::extract_patched_paths(patch)
                    .into_iter()
                    .next()
                    .map(serde_json::Value::String)
            });
        if let Some(path) = path {
            if config::EDIT_TOOLS.contains(&event.tool_name.as_str()) {
                metadata.insert("filepath".into(), path.clone());
            }
            if event.tool_name == "read" {
                metadata.insert("filePath".into(), path);
            }
        }
        let mut request = types::Request::new(
            format!("permission-{}", counter.fetch_add(1, Ordering::Relaxed) + 1),
            ctx.session_manager.session_id().into(),
            parsed.permission,
            parsed.patterns,
            parsed.always,
            metadata,
        );
        request.original_paths = parsed.original_paths;
        request.wider_always = parsed.wider_always.map(|scope| types::WiderAlways {
            label: scope.label,
            patterns: scope.patterns,
        });
        // senpi pins the requesting call on the request, so a client binds the dialog to the call it
        // approves: with several calls of one tool in flight nothing else names which.
        request.tool = Some(types::RequestTool {
            message_id: None,
            call_id: event.tool_call_id.clone(),
            parent_call_id: event.parent_tool_call_id.clone(),
        });
        let ask = service::AskOptions {
            auto_approve_ask: parsed.auto_approve_ask,
            rule_aliases: parsed.rule_aliases,
        };
        let (future, pending, preset_bound) = {
            let mut guard = lock_state(state);
            let Some(policy) = guard.as_mut() else {
                return Ok(None);
            };
            let future = policy.service.ask_with(request.clone(), ask);
            let pending = policy
                .service
                .list()
                .iter()
                .any(|entry| entry.id == request.id);
            (future, pending, policy.preset_bound)
        };
        if pending {
            // senpi passes the dispatch abort signal into the prompt; here dropping this future (a
            // dispatch abort or retirement) rejects the pending request instead of leaving it open.
            let mut unanswered = PendingRequest {
                state,
                id: Some(request.id.clone()),
            };
            let reply = if ctx.has_ui {
                prompt::show_permission_prompt(ctx, &request).await
            } else {
                // The service already emitted asked; a private bus keeps it from being emitted twice.
                non_interactive::handle_no_ui(
                    &request,
                    preset_bound,
                    &events::PermissionEventEmitter::default(),
                )
                .map_err(|error| ExtensionFailure::new(error.to_string()))?
            };
            unanswered.id = None;
            if let Some(policy) = lock_state(state).as_mut() {
                policy.service.reply(reply);
            }
        }
        if let Err(error) = future.await {
            return Ok(Some(ToolCallEventResult {
                block: Some(true),
                reason: Some(error.to_string()),
                terminate: None,
            }));
        }
    }
    Ok(None)
}
pub struct PermissionSystem;
impl Extension for PermissionSystem {
    fn register(&self, api: &mut ExtensionApi) {
        api.register_flag(
            "permission",
            FlagType::String { default: None },
            Some("Set permission rules (format: tool=action or tool:pattern=action)".into()),
        );
        api.register_flag("permission-preset",FlagType::String{default:None},Some("Set permission preset (guarded, full-access, workspace, accept-edits, auto, read-only, or ask)".into()));
        let state: PolicyState = Arc::new(Mutex::new(None));
        let counter = Arc::new(AtomicU64::new(0));
        // senpi createDispatchAuthorizer: tools that dispatch a prepared operation re-check live permission at dispatch.
        let policy_state = Arc::clone(&state);
        let ask_state = Arc::clone(&state);
        let ask_counter = Arc::clone(&counter);
        let dispatch_authorizer = dispatch_policy::create_dispatch_authorizer(
            Arc::new(move |request, ctx| {
                let home = home_dir().map_err(|error| error.message)?;
                // Parse unlocked: the parser calls the definition resolver and custom permission parsers.
                let registry = lock_state(&policy_state)
                    .as_ref()
                    .map(|policy| policy.registry.clone())
                    .ok_or_else(|| "Permission authorizer is not initialized".to_owned())?;
                let requests = registry.parse(
                    &request.tool_name,
                    &request.input,
                    (&ctx.cwd, std::path::Path::new(&home)),
                );
                let guard = lock_state(&policy_state);
                let policy = guard
                    .as_ref()
                    .ok_or_else(|| "Permission authorizer is not initialized".to_owned())?;
                Ok(dispatch_policy::decide_dispatch_policy(
                    &policy.service,
                    policy.preset_bound,
                    &requests,
                    ctx,
                ))
            }),
            Arc::new(move |event, ctx| {
                let (state, counter) = (Arc::clone(&ask_state), Arc::clone(&ask_counter));
                Box::pin(async move { authorize_tool_call(&state, &counter, &event, ctx).await })
            }),
        );
        let retirement: Arc<Mutex<Option<dispatch::DispatchRetirement>>> =
            Arc::new(Mutex::new(None));
        let start_state = Arc::clone(&state);
        let runtime = api.runtime.clone();
        let bus = api.events.clone();
        let start_authorizer = Arc::clone(&dispatch_authorizer);
        let start_retirement = Arc::clone(&retirement);
        api.on(EventKind::SessionStart,Arc::new(move|_,ctx|{
            let state=Arc::clone(&start_state);let runtime=runtime.clone();let bus=bus.clone();
            let authorizer=Arc::clone(&start_authorizer);let retirement=Arc::clone(&start_retirement);
            Box::pin(async move{
                let home=home_dir()?;
                let manager=maho_core::settings_manager::SettingsManager::create(&ctx.cwd.to_string_lossy(),&ctx.agent_dir.to_string_lossy(),&home,ctx.is_project_trusted());
                let cli=match runtime.get_flag("permission"){Some(FlagValue::String(value))=>cli::parse_permission_flag(&value),_=>Vec::new()};
                let preset=match runtime.get_flag("permission-preset"){
                    Some(FlagValue::String(value))=>Some(cli::parse_permission_preset_flag(&value).ok_or_else(||ExtensionFailure::new(format!("Invalid --permission-preset \"{value}\". Expected one of: guarded, full-access, workspace, accept-edits, auto, read-only, ask.")))?),_=>None,
                };
                let resolved=settings::resolve_permission_config(&manager,&cli,&home,preset).map_err(|error|ExtensionFailure::new(error.to_string()))?;
                let preset_bound=resolved.preset==types::PermissionPresetName::Auto;let rules=resolved.rules;
                let approved=storage::load_approved(&ctx.agent_dir,&ctx.cwd);
                let initial_approved=approved.len();let actions=runtime.session_actions()?;let active=actions.get_active_tools()?;
                let disabled=config::disabled(&active,&rules);actions.set_active_tools(active.into_iter().filter(|name|!disabled.contains(name)).collect())?;
                let mut registry = parsers::create_builtin_parser_registry_in(&ctx.agent_dir);
                let rt = runtime.clone();
                registry.set_definition_resolver(Arc::new(move |name: &str| rt.live_tool_definition(name)));
                *lock_state(&state)=Some(SessionPolicy{service:service::PermissionService::new(rules.clone(),approved,events::PermissionEventEmitter{bus}).with_store_paths(&ctx.agent_dir,&ctx.cwd),registry,preset_bound,initial_approved});
                let previous=retirement.lock().expect("dispatch retirement lock").replace(dispatch::register_dispatch_authorizer(&ctx.session_manager,authorizer));
                if let Some(previous)=previous{previous.retire();}
                Ok(EventResult::None)
            })
        }));
        let call_state = Arc::clone(&state);
        let call_counter = Arc::clone(&counter);
        let call_runtime = api.runtime.clone();
        api.on(
            EventKind::ToolCall,
            Arc::new(move |event, ctx| {
                let state = Arc::clone(&call_state);
                let counter = Arc::clone(&call_counter);
                let runtime = call_runtime.clone();
                let authorizer = Arc::clone(&dispatch_authorizer);
                Box::pin(async move {
                    let ExtensionEvent::ToolCall(event) = event else {
                        return Ok(EventResult::None);
                    };
                    // senpi prepareMcpDispatchApproval: the policy and operation presented now are what an approval covers.
                    let approve = dispatch_metadata::prepare_mcp_dispatch_approval(
                        |name| runtime.live_tool_definition(name),
                        event,
                        ctx,
                        authorizer.as_ref(),
                    )
                    .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                    if let Some(blocked) = authorize_tool_call(&state, &counter, event, ctx).await?
                    {
                        return Ok(EventResult::ToolCall(blocked));
                    }
                    if let Some(approve) = approve {
                        approve
                            .approve()
                            .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        // senpi forgetDispatchApproval: a finished call's approval must not outlive it.
        api.on(
            EventKind::ToolResult,
            Arc::new(|_, _| {
                Box::pin(async {
                    if let Some(input) = maho_ext_api::invocation_input::current_invocation_input()
                    {
                        dispatch::forget_dispatch_approval(&input);
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_, ctx| {
                let state = Arc::clone(&state);
                let retirement = Arc::clone(&retirement);
                Box::pin(async move {
                    if let Some(retirement) =
                        retirement.lock().expect("dispatch retirement lock").take()
                    {
                        retirement.retire();
                    }
                    let mut guard = lock_state(&state);
                    if let Some(policy) = guard.as_mut() {
                        let approved = policy.service.get_approved();
                        storage::append_approved(
                            &ctx.agent_dir,
                            &ctx.cwd,
                            &approved[policy.initial_approved..],
                        )
                        .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                        for request in policy.service.list() {
                            policy.service.reply(types::ReplyInput {
                                request_id: request.id,
                                reply: types::Reply::Reject,
                                message: None,
                                always: None,
                            });
                        }
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }
}
