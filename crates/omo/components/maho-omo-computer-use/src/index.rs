fn canonical_path(path: &std::path::Path) -> std::path::PathBuf {
    if path.exists() {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}
/// Mirror of the private `telemetry::matches_tool_name`, used to skip non-computer tool starts early.
fn matches_tool_name(tool_name: &str, expected: &str) -> bool {
    let normalized = tool_name.trim().to_lowercase().replace('-', "_");
    normalized == expected || normalized.ends_with(&format!("_{expected}")) || normalized.ends_with(&format!(":{expected}"))
}
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use maho_ext_api::{
    CommandArgumentCompletions, CommandContextHandler, EventKind, EventResult, Extension,
    ExtensionApi, ExtensionCommandContext, ExtensionEvent, NotificationType, ResourceDiscoverEntry,
    ResourcesDiscoverResult, SourceScope,
};
use maho_omo_bundled_skills::contributed_skill::{
    host_commands_from_runtime, read_disabled_skills, ContributedSkill,
};
use maho_omo_config_resolution::load_senpi_omo_config;
use maho_tools::definition::{ToolCall, ToolError};
use maho_tui::autocomplete::AutocompleteItem;
use omo_config_core::{LoadOmoConfigOptions, OmoConfigEnv};
use senpi_desktop_tool::cua_definition::{
    computer_actions_permission_parser, computer_actions_tool_definition, COMPUTER_ACTIONS_TOOL_NAME,
};
use senpi_desktop_tool::permission::computer_permission_parser;
use senpi_desktop_tool::registration::{
    is_supported_host, materialize_computer_skill, run_computer_command,
    COMPUTER_COMMAND_USAGE, COMPUTER_SKILL_NAME, COMPUTER_SUBCOMMANDS,
};
use senpi_desktop_tool::session::{ComputerHostContext, ComputerModel, ComputerSessionManager};
use senpi_desktop_tool::settings::ComputerSettings;
use senpi_desktop_tool::tool_definition::{computer_tool_definition, COMPUTER_TOOL_NAME};

use crate::engine_source::describe_engine_source;
use crate::engine_status::EngineState;
use crate::installed_engine::DesktopEngineLocatorOptions;
use crate::permission_events::wire_computer_permission_events;
use crate::permission_status::describe_engine_permissions;
use crate::registration_support::{skill_status_line, tool_activated_names};
use crate::runtime_loader::{
    ComputerHostApi, ComputerRuntimeLoader, ComputerRuntimeLoaderOptions,
    EngineChildFactory, ExtensionRuntimeHostBridge, RuntimeLoaderFn,
};
use crate::settings::resolve_omo_computer_settings;
use crate::telemetry::{
    create_computer_use_telemetry, ComputerUseTelemetryObservers,
};

pub const COMPUTER_USE_COMPONENT_NAME: &str = "computer-use";
pub const COMPUTER_UNAVAILABLE: &str = "Computer use is unavailable in this session.";

pub type SettingsLoader = Arc<dyn Fn(&Path, &str) -> Result<ComputerSettings, String> + Send + Sync>;

#[derive(Default, Clone)]
pub struct ComputerUseComponentOptions {
    pub platform: Option<String>,
    pub env: Option<OmoConfigEnv>,
    pub engine_child: Option<EngineChildFactory>,
    pub load_settings: Option<SettingsLoader>,
    pub telemetry_observers: Option<Arc<dyn ComputerUseTelemetryObservers>>,
    pub load_runtime: Option<RuntimeLoaderFn>,
}

pub fn default_load_settings(cwd: &Path, platform: &str) -> Result<ComputerSettings, String> {
    let loaded = load_senpi_omo_config(LoadOmoConfigOptions {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        ..Default::default()
    });
    let raw_computer = loaded.config.get("computer");
    resolve_omo_computer_settings(raw_computer, platform)
        .map_err(|e| format!("{e:?}"))
}

#[derive(Clone, Default)]
pub struct ComputerUseComponent {
    options: ComputerUseComponentOptions,
}

impl ComputerUseComponent {
    pub fn new(options: ComputerUseComponentOptions) -> Self {
        Self { options }
    }

    pub fn register(&self, api: &mut ExtensionApi) {
        let platform = self
            .options
            .platform
            .clone()
            .unwrap_or_else(|| std::env::consts::OS.to_string());
        let load_settings_fn = self
            .options
            .load_settings
            .clone()
            .unwrap_or_else(|| Arc::new(default_load_settings));

        let telem = Arc::new(create_computer_use_telemetry(
            &platform,
            self.options.telemetry_observers.clone(),
        ));

        let env_for_perms: BTreeMap<String, String> = self
            .options
            .env
            .clone()
            .unwrap_or_else(|| std::env::vars().collect());

        let report_permission = wire_computer_permission_events(api, &env_for_perms);

        let cwd = api.cwd.clone();
        let is_supported = is_supported_host(&platform);
        let host_bridge: Arc<dyn ComputerHostApi> = Arc::new(ExtensionRuntimeHostBridge::new(api.runtime.clone()));

        let available = if !is_supported {
            None
        } else {
            match load_settings_fn(&cwd, &platform) {
                Ok(s) if s.enabled => Some((Arc::clone(&host_bridge), s)),
                _ => None,
            }
        };

        let state_backend = Arc::new(std::sync::Mutex::new("unavailable".to_string()));
        let state_telemetry_ctx: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));
        let state_activation_reported = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let state_skill: Arc<tokio::sync::Mutex<Option<ContributedSkill>>> =
            Arc::new(tokio::sync::Mutex::new(None));

        let telemetry_for_err = Arc::clone(&telem);
        let state_ctx_for_err = Arc::clone(&state_telemetry_ctx);
        let state_backend_for_err = Arc::clone(&state_backend);

        let on_engine_error: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |code: String| {
            let ctx = state_ctx_for_err.lock().ok().and_then(|g| g.clone());
            let backend_str = state_backend_for_err.lock().ok().map(|g| g.clone()).unwrap_or_else(|| "unavailable".to_string());
            if let Some(session_id) = ctx {
                telemetry_for_err.engine_error(&session_id, &code, &serde_json::Value::String(backend_str));
            }
        });

        let runtime = available.as_ref().map(|(host, settings)| {
            Arc::new(ComputerRuntimeLoader::new(ComputerRuntimeLoaderOptions {
                host: Arc::clone(host),
                settings: settings.clone(),
                engine_child: self.options.engine_child.clone(),
                on_engine_error,
                on_permission_required: Some(report_permission),
                load_runtime: self.options.load_runtime.clone(),
            }))
        });



        let completions: CommandArgumentCompletions = Arc::new(|prefix: &str| {
            let p = prefix.trim().to_string();
            let items: Vec<AutocompleteItem> = COMPUTER_SUBCOMMANDS
                .iter()
                .filter(|name| name.starts_with(&p))
                .map(|name| AutocompleteItem {
                    value: name.to_string(),
                    label: name.to_string(),
                    description: None,
                })
                .collect();
            Box::pin(async move { Ok(Some(items)) })
        });

        let available_cmd = available.clone();
        let runtime_cmd = runtime.clone();
        let platform_cmd = platform.clone();
        let env_cmd = env_for_perms.clone();
        let telem_cmd = Arc::clone(&telem);
        let state_backend_cmd = Arc::clone(&state_backend);
        let state_ctx_cmd = Arc::clone(&state_telemetry_ctx);
        let state_activation_reported_cmd = Arc::clone(&state_activation_reported);
        let state_skill_cmd = Arc::clone(&state_skill);

        let handler: CommandContextHandler = Arc::new(move |args: &str, cmd_ctx: &ExtensionCommandContext| {
            let args = args.to_string();
            let available_cmd = available_cmd.clone();
            let runtime_cmd = runtime_cmd.clone();
            let platform_cmd = platform_cmd.clone();
            let env_cmd = env_cmd.clone();
            let telem = Arc::clone(&telem_cmd);
            let state_backend = Arc::clone(&state_backend_cmd);
            let state_ctx = Arc::clone(&state_ctx_cmd);
            let state_activation_reported = Arc::clone(&state_activation_reported_cmd);
            let state_skill = Arc::clone(&state_skill_cmd);
            let ui = cmd_ctx.ui.clone();
            let has_ui = cmd_ctx.has_ui;

            struct SessionManagerAdapter(Arc<dyn maho_ext_api::SessionManager>);
            impl ComputerSessionManager for SessionManagerAdapter {
                fn get_session_id(&self) -> String {
                    self.0.session_id().to_string()
                }
                fn get_session_dir(&self) -> PathBuf {
                    self.0.get_session_dir().unwrap_or_else(|| PathBuf::from("."))
                }
            }

            let host_ctx = ComputerHostContext {
                cwd: cmd_ctx.cwd.clone(),
                session_manager: Arc::new(SessionManagerAdapter(Arc::clone(&cmd_ctx.session_manager))),
                model: cmd_ctx.model.as_ref().map(|m| ComputerModel {
                    id: m.id.clone(),
                    provider: m.provider.clone(),
                    api: m.api.clone(),
                    compat: m.compat.as_ref().and_then(|c| serde_json::to_value(c).ok()),
                }),
            };

            Box::pin(async move {
                let (Some((host, settings)), Some(runtime)) = (available_cmd.as_ref(), runtime_cmd.as_ref()) else {
                    ui.notify(COMPUTER_UNAVAILABLE, NotificationType::Warning);
                    return Ok(());
                };

                let trimmed = args.trim().to_lowercase();
                let command = if trimmed.is_empty() { "status" } else { &trimmed };

                let session_id = host_ctx.session_manager.get_session_id();
                if let Ok(mut g) = state_ctx.lock() {
                    *g = Some(session_id.clone());
                }

                let rt = match runtime.load().await {
                    Ok(r) => r,
                    Err(e) => {
                        ui.notify(&format!("/computer {}: {e}", args.trim()), NotificationType::Error);
                        return Ok(());
                    }
                };

                let was_active = rt.handle.is_active();
                let text = run_computer_command(&args, &rt.handle, &host_ctx).await;

                if command == "on" && !was_active && rt.handle.is_active() && !state_activation_reported.load(Ordering::SeqCst) {
                    state_activation_reported.store(true, Ordering::SeqCst);
                    if let Ok(caps) = rt.service.capabilities().await {
                        if let Ok(mut b) = state_backend.lock() {
                            *b = caps.backend.clone();
                        }
                        telem.activation(&session_id, true, "command_on", &serde_json::Value::String(caps.backend.clone()));
                        telem.os_permissions(&session_id, &caps);
                    }
                } else if command == "off" && was_active && !rt.handle.is_active() {
                    state_activation_reported.store(false, Ordering::SeqCst);
                    let backend_str = state_backend
                        .lock()
                        .map(|g| g.clone())
                        .unwrap_or_else(|_| "unavailable".to_string());
                    telem.activation(&session_id, false, "command_off", &serde_json::Value::String(backend_str));
                }

                if command != "status" {
                    let level = if text == COMPUTER_COMMAND_USAGE {
                        NotificationType::Warning
                    } else {
                        NotificationType::Info
                    };
                    ui.notify(&text, level);
                    return Ok(());
                }

                let prelude = if host.get_active_tools().unwrap_or_default().iter().any(|t| t == COMPUTER_TOOL_NAME) {
                    "active"
                } else {
                    "inactive"
                };

                let loc_opts = DesktopEngineLocatorOptions {
                    platform: Some(platform_cmd.clone()),
                    ..Default::default()
                };
                let source = describe_engine_source(settings.engine_path.as_deref(), &env_cmd, &loc_opts);
                let permissions = if rt.handle.is_running().await {
                    String::new()
                } else {
                    let perm_text = describe_engine_permissions(settings.engine_path.as_deref(), &env_cmd, &loc_opts).await;
                    format!("\n{perm_text}")
                };

                let engine_state = rt.service.engine_state().await;
                let engine_str = if engine_state == EngineState::NotStarted {
                    format!("not started ({source})")
                } else {
                    engine_state.to_string()
                };

                let sk = state_skill.lock().await;
                let sk_line = skill_status_line(sk.as_ref());
                let status_msg = format!("{text}\nengine: {engine_str}\nprelude: {prelude}{sk_line}\nengine source: {source}{permissions}");
                ui.notify(&status_msg, NotificationType::Info);
                if !has_ui {
                    maho_ext_api::diagnostic(&status_msg);
                }
                Ok(())
            })
        });

        api.register_command_with_context_and_completions(
            "computer",
            Some("Computer use (experimental): on, off, status, stop, or resume (stop and resume are user-only)".to_string()),
            Some(COMPUTER_SUBCOMMANDS.join("|")),
            handler,
            completions,
        );

        let (Some((_host, settings)), Some(runtime)) = (available, runtime) else {
            return;
        };

        let mut comp_def = computer_tool_definition();
        let runtime_for_tool = Arc::clone(&runtime);
        comp_def.execute = Arc::new(move |call: ToolCall| {
            let rt_loader = Arc::clone(&runtime_for_tool);
            Box::pin(async move {
                let rt = rt_loader.load().await.map_err(|e| ToolError::Message(e.to_string()))?;
                (rt.computer_tool.execute)(call).await
            })
        });
        comp_def.permission_parser = Some(Arc::new(|input, cwd| {
            computer_permission_parser(COMPUTER_TOOL_NAME, input, cwd)
                .into_iter()
                .map(Into::into)
                .collect()
        }));
        api.register_tool(comp_def);

        if settings.cua_adapter {
            let mut cua_def = computer_actions_tool_definition();
            let runtime_for_cua = Arc::clone(&runtime);
            cua_def.execute = Arc::new(move |call: ToolCall| {
                let rt_loader = Arc::clone(&runtime_for_cua);
                Box::pin(async move {
                    let rt = rt_loader.load().await.map_err(|e| ToolError::Message(e.to_string()))?;
                    match &rt.computer_actions_tool {
                        Some(tool) => (tool.execute)(call).await,
                        None => Err(ToolError::Message("computer_actions tool not available".into())),
                    }
                })
            });
            cua_def.permission_parser = Some(Arc::new(|input, cwd| {
                computer_actions_permission_parser(COMPUTER_ACTIONS_TOOL_NAME, input, cwd)
            }));
            api.register_tool(cua_def);
        }

        let state_session_start = Arc::clone(&state_telemetry_ctx);
        api.on(EventKind::SessionStart, Arc::new(move |_event, ctx| {
            let session_id = ctx.session_manager.session_id().to_string();
            if let Ok(mut g) = state_session_start.lock() {
                *g = Some(session_id);
            }
            Box::pin(async move { Ok(EventResult::None) })
        }));

        let state_skill_disc = Arc::clone(&state_skill);
        let env_disc = env_for_perms.clone();
        let api_cwd = api.cwd.clone();
        let runtime_disc = api.runtime.clone();

        api.on(
            EventKind::ResourcesDiscover,
            Arc::new(move |event, ctx| {
                let state_skill_disc = Arc::clone(&state_skill_disc);
                let env_disc = env_disc.clone();
                let fallback_cwd = ctx.cwd.clone();
                let cwd = match event {
                    ExtensionEvent::ResourcesDiscover(d) => {
                        if !d.cwd.as_os_str().is_empty() {
                            d.cwd.clone()
                        } else if !api_cwd.as_os_str().is_empty() {
                            api_cwd.clone()
                        } else {
                            fallback_cwd
                        }
                    }
                    _ => {
                        if !api_cwd.as_os_str().is_empty() {
                            api_cwd.clone()
                        } else {
                            fallback_cwd
                        }
                    }
                };

                let host_cmds = host_commands_from_runtime(runtime_disc.clone());
                let cmds = host_cmds().ok().flatten();

                Box::pin(async move {
                    let disabled = read_disabled_skills(&cwd, &env_disc);
                    if disabled.iter().any(|d| d == COMPUTER_SKILL_NAME) {
                        let mut g = state_skill_disc.lock().await;
                        *g = Some(ContributedSkill::Disabled);
                        return Ok(EventResult::None);
                    }

                    let command_name = format!("skill:{COMPUTER_SKILL_NAME}");
                    let loaded = cmds.as_deref().and_then(|commands| {
                        commands.iter().find(|entry| entry.source == "skill" && entry.name == command_name)
                    });

                    if let Some(loaded_cmd) = loaded {
                        let path = materialize_computer_skill(None)
                            .map_err(|e| maho_ext_api::ExtensionFailure::new(e.to_string()))?;
                        let is_match = loaded_cmd
                            .source_info_path
                            .as_deref()
                            .map(std::path::Path::new)
                            .map(canonical_path)
                            == Some(canonical_path(&path));

                        if is_match {
                            let skill = ContributedSkill::Contributed { path: path.clone() };
                            let mut g = state_skill_disc.lock().await;
                            *g = Some(skill);
                            return Ok(EventResult::ResourcesDiscover(ResourcesDiscoverResult {
                                skill_paths: vec![ResourceDiscoverEntry {
                                    path: path.to_string_lossy().to_string(),
                                    scope: Some(SourceScope::System),
                                }],
                                ..Default::default()
                            }));
                        } else {
                            let skill = ContributedSkill::Yielded {
                                owner_path: loaded_cmd.source_info_path.clone(),
                            };
                            let mut g = state_skill_disc.lock().await;
                            *g = Some(skill);
                            return Ok(EventResult::None);
                        }
                    }

                    let path = materialize_computer_skill(None)
                        .map_err(|e| maho_ext_api::ExtensionFailure::new(e.to_string()))?;
                    let skill = ContributedSkill::Contributed { path: path.clone() };
                    let mut g = state_skill_disc.lock().await;
                    *g = Some(skill);

                    Ok(EventResult::ResourcesDiscover(ResourcesDiscoverResult {
                        skill_paths: vec![ResourceDiscoverEntry {
                            path: path.to_string_lossy().to_string(),
                            scope: Some(SourceScope::System),
                        }],
                        ..Default::default()
                    }))
                })
            }),
        );

        let rt_loader_act = Arc::clone(&runtime);
        let telem_act = Arc::clone(&telem);
        let state_ctx_act = Arc::clone(&state_telemetry_ctx);
        let state_act = Arc::clone(&state_activation_reported);
        let state_be_act = Arc::clone(&state_backend);

        let sub_tool_activated = api.events.on(
            "tool_activated",
            Arc::new(move |payload: &serde_json::Value| {
                let activated = tool_activated_names(payload);
                if !activated.iter().any(|name| name == COMPUTER_TOOL_NAME || name == COMPUTER_ACTIONS_TOOL_NAME) {
                    return;
                }
                let rt_loader = Arc::clone(&rt_loader_act);
                let telem = Arc::clone(&telem_act);
                let state_ctx = Arc::clone(&state_ctx_act);
                let state_act = Arc::clone(&state_act);
                let state_be = Arc::clone(&state_be_act);

                let session_id = state_ctx
                    .lock()
                    .ok()
                    .and_then(|g| g.clone())
                    .unwrap_or_else(|| "session-1".to_string());

                tokio::spawn(async move {
                    let rt = match rt_loader.load().await {
                        Ok(r) => r,
                        Err(_) => return,
                    };
                    if rt.handle.is_active() {
                        return;
                    }
                    let host_ctx = ComputerHostContext {
                        cwd: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
                        session_manager: Arc::new(SimpleSessionManager {
                            session_id: session_id.clone(),
                            session_dir: std::path::PathBuf::from("."),
                        }),
                        model: None,
                    };
                    let _ = rt.handle.activate(&host_ctx).await;
                    if state_act.load(Ordering::SeqCst) {
                        return;
                    }
                    state_act.store(true, Ordering::SeqCst);
                    match rt.service.capabilities().await {
                        Ok(caps) => {
                            if let Ok(mut b) = state_be.lock() {
                                *b = caps.backend.clone();
                            }
                            telem.activation(
                                &session_id,
                                true,
                                "tool_call",
                                &serde_json::Value::String(caps.backend.clone()),
                            );
                            telem.os_permissions(&session_id, &caps);
                        }
                        Err(_) => {
                            state_act.store(false, Ordering::SeqCst);
                        }
                    }
                });
            }),
        );
        std::mem::forget(sub_tool_activated);

        let state_exec_start = Arc::clone(&state_telemetry_ctx);
        let telemetry_exec_start = Arc::clone(&telem);
        api.on(EventKind::ToolExecutionStart, Arc::new(move |event, ctx| {
            let telemetry = Arc::clone(&telemetry_exec_start);
            let state = Arc::clone(&state_exec_start);
            // Only computer tools can open a permission request; skip cloning other tools' args.
            let captured = match event {
                ExtensionEvent::ToolExecutionStart { tool_call_id, tool_name, args, .. }
                    if matches_tool_name(tool_name, COMPUTER_TOOL_NAME) || matches_tool_name(tool_name, COMPUTER_ACTIONS_TOOL_NAME) =>
                {
                    Some((ctx.session_manager.session_id().to_string(), tool_call_id.clone(), tool_name.clone(), args.clone()))
                }
                _ => None,
            };
            Box::pin(async move {
                let Some((session_id, tool_call_id, tool_name, args)) = captured else {
                    return Ok(EventResult::None);
                };
                let payload = serde_json::json!({
                    "type": "tool_execution_start",
                    "toolCallId": tool_call_id,
                    "toolName": tool_name,
                    "args": args,
                });
                if telemetry.tool_execution_started(&payload)
                    && let Ok(mut g) = state.lock() {
                    *g = Some(session_id);
                }
                Ok(EventResult::None)
            })
        }));

        let state_exec_end = Arc::clone(&state_telemetry_ctx);
        let state_backend_exec_end = Arc::clone(&state_backend);
        let telemetry_exec_end = Arc::clone(&telem);
        api.on(EventKind::ToolExecutionEnd, Arc::new(move |event, ctx| {
            let telemetry = Arc::clone(&telemetry_exec_end);
            let state_ctx = Arc::clone(&state_exec_end);
            let state_backend = Arc::clone(&state_backend_exec_end);
            // Nothing awaits a permission verdict: skip cloning the result and building the payload.
            let captured = match event {
                ExtensionEvent::ToolExecutionEnd { tool_call_id, tool_name, result, is_error, .. } if telemetry.has_pending() => {
                    let session_id = state_ctx.lock().ok().and_then(|g| g.clone()).unwrap_or_else(|| ctx.session_manager.session_id().to_string());
                    let backend_str = state_backend.lock().ok().map(|g| g.clone()).unwrap_or_else(|| "unavailable".to_string());
                    Some((session_id, backend_str, tool_call_id.clone(), tool_name.clone(), result.clone(), *is_error))
                }
                _ => None,
            };
            Box::pin(async move {
                let Some((session_id, backend_str, tool_call_id, tool_name, result, is_error)) = captured else {
                    return Ok(EventResult::None);
                };
                let payload = serde_json::json!({
                    "type": "tool_execution_end",
                    "toolCallId": tool_call_id,
                    "toolName": tool_name,
                    "result": result,
                    "isError": is_error,
                });
                telemetry.permission_tier_denied(&payload, &session_id, &serde_json::Value::String(backend_str));
                Ok(EventResult::None)
            })
        }));

        let state_msg_end = Arc::clone(&state_telemetry_ctx);
        let state_backend_msg_end = Arc::clone(&state_backend);
        let telemetry_msg_end = Arc::clone(&telem);
        api.on(EventKind::MessageEnd, Arc::new(move |event, ctx| {
            let telemetry = Arc::clone(&telemetry_msg_end);
            let state_ctx = Arc::clone(&state_msg_end);
            let state_backend = Arc::clone(&state_backend_msg_end);
            // Only a toolResult message can settle a pending permission; skip serialising the rest.
            let captured = match event {
                ExtensionEvent::MessageEnd { message } if message.role() == "toolResult" && telemetry.has_pending() => {
                    let session_id = state_ctx.lock().ok().and_then(|g| g.clone()).unwrap_or_else(|| ctx.session_manager.session_id().to_string());
                    let backend_str = state_backend.lock().ok().map(|g| g.clone()).unwrap_or_else(|| "unavailable".to_string());
                    Some((session_id, backend_str, serde_json::to_value(message).unwrap_or_default()))
                }
                _ => None,
            };
            Box::pin(async move {
                let Some((session_id, backend_str, msg_val)) = captured else {
                    return Ok(EventResult::None);
                };
                let payload = serde_json::json!({
                    "type": "message_end",
                    "message": msg_val,
                });
                telemetry.permission_tier_denied(&payload, &session_id, &serde_json::Value::String(backend_str));
                Ok(EventResult::None)
            })
        }));

        let runtime_shutdown = Arc::clone(&runtime);
        api.on(EventKind::SessionShutdown, Arc::new(move |_event, _ctx| {
            let rt_loader = Arc::clone(&runtime_shutdown);
            Box::pin(async move {
                if let Some(fut) = rt_loader.started() {
                    let rt = fut.await.map_err(maho_ext_api::ExtensionFailure::new)?;
                    rt.handle.close().await.map_err(|e| maho_ext_api::ExtensionFailure::new(e.to_string()))?;
                }
                Ok(EventResult::None)
            })
        }));
    }
}

struct SimpleSessionManager {
    session_id: String,
    session_dir: std::path::PathBuf,
}
impl ComputerSessionManager for SimpleSessionManager {
    fn get_session_id(&self) -> String {
        self.session_id.clone()
    }
    fn get_session_dir(&self) -> std::path::PathBuf {
        self.session_dir.clone()
    }
}

impl Extension for ComputerUseComponent {
    fn register(&self, api: &mut ExtensionApi) {
        ComputerUseComponent::register(self, api);
    }
}
