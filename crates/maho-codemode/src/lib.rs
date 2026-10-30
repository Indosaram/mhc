//! Code mode: the persistent `eval` tool. Hosts the bun/js kernel (`kernels`, `interpreters`),
//! the bridge that exposes session tools inside a cell (`bridge`, `host_sdk`), and the settings
//! loaded from `.omo/codemode.json` / `~/.omo/agent/codemode.json` (`config`).

pub mod bridge;
pub mod bridges;
pub mod completion;
pub mod config;
pub mod environments;
pub mod extension;
pub mod host_sdk;
pub mod interpreters;
pub mod kernels;
pub mod output;
pub mod prompt;
pub mod timeouts;
pub mod tool;

use maho_ext_api::{EventKind, EventResult, ExtensionApi, ExtensionContext, ExtensionFailure};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tool::{
    detached_cell_manager::{DetachedCellManagerOptions, EvalDetachedCellManager},
    eval_tool_options::CreateEvalToolOptions,
};

pub type ContextCompletion = Arc<
    dyn Fn(
            completion::handler::CompletionRequest,
            ExtensionContext,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<serde_json::Value, completion::handler::CompletionError>,
                    > + Send,
            >,
        > + Send
        + Sync,
>;

pub struct CodemodeExtensionOptions {
    pub image_sdk: Arc<dyn tool::image_resize::EvalImageSdk>,
    pub complete: ContextCompletion,
    pub home_dir: PathBuf,
    pub environment: config::settings::Environment,
    pub js_runtime: tool::types::EvalRuntimeInfo,
    /// `Some(passthrough)` scrubs secret-looking variables from the eval kernel environment.
    pub env_passthrough: Option<Vec<String>>,
}

struct ExtensionState {
    runtime: Option<extension::runtime_factory::SessionRuntime>,
    cells: Arc<Mutex<EvalDetachedCellManager>>,
    context: Option<ExtensionContext>,
    model_id: Option<String>,
    generation: u64,
    completion_handles: Arc<bridges::handles::CompletionHandleRegistry>,
    recovery: Option<Arc<extension::start_recovery::StartRecovery<ExtensionContext>>>,
    eval_execute: Option<maho_tools::definition::ToolExecutor>,
}

pub fn register(
    api: &mut ExtensionApi,
    options: CodemodeExtensionOptions,
) -> Result<(), ExtensionFailure> {
    let options = Arc::new(options);
    let proxy = Arc::new(extension::session_manager_proxy::SessionManagerProxy::default());
    let state = Arc::new(Mutex::new(ExtensionState {
        runtime: None,
        cells: Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default()))),
        context: None,
        model_id: None,
        generation: 0,
        completion_handles: Arc::new(Default::default()),
        recovery: None,
        eval_execute: None,
    }));
    let host = Arc::new(Mutex::new(ExtensionApi::new(
        api.registered.clone(),
        api.profile.clone(),
        api.events.clone(),
        api.runtime.clone(),
    )));
    let ticker_context = Arc::downgrade(&state);
    let ticker = Arc::new(Mutex::new(
        extension::eval_status_ticker::EvalStatusTicker::new_with_try_render(
            Arc::new(move |status| {
                let context = ticker_context
                    .upgrade()
                    .and_then(|state| state.lock().expect("extension state").context.clone());
                if let Some(context) = context {
                    context.ui.set_status(
                        extension::eval_status::EVAL_CELLS_STATUS_KEY,
                        status.as_deref(),
                    );
                }
                Ok(())
            }),
            Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system clock")
                    .as_secs_f64()
                    * 1000.0
            }),
        ),
    ));
    let notifier = Arc::new(Mutex::new(extension::eval_notifier::EvalNotifier::default()));
    let cells =
        create_extension_cells(&host, &state, &ticker, &notifier, &Default::default(), None);
    state.lock().expect("extension state").cells = cells;
    api.register_removed_tool_hint("exec","exec was removed; use eval({ language: \"js\", code }) instead. Long eval cells detach on their own and notify when complete.");
    api.register_removed_tool_hint("wait","wait was removed; detached eval cells notify when complete. Use eval({ action: \"peek\"|\"stop\", cell_id }) to inspect or stop one.");
    host.lock().expect("extension api").registered = api.registered.clone();
    let version = (options.js_runtime.name == "bun").then(|| options.js_runtime.version.clone());
    extension::skill_contribution::register_bun_skill_contribution(
        api,
        Arc::new(move || version.clone()),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"),
    );
    let start_host = Arc::downgrade(&host);
    let start_state = Arc::downgrade(&state);
    let start_proxy = proxy.clone();
    let start_options = options.clone();
    let start_ticker = ticker.clone();
    let start_notifier = notifier.clone();
    let start_session: extension::start_recovery::StartSession<ExtensionContext> = Arc::new(
        move |event, context| {
            let host = start_host.upgrade();
            let state = start_state.upgrade();
            let proxy = start_proxy.clone();
            let options = start_options.clone();
            let ticker = start_ticker.clone();
            let notifier = start_notifier.clone();
            Box::pin(async move {
                let host =
                    host.ok_or_else(|| ExtensionFailure::new("codemode extension is disposed"))?;
                let state =
                    state.ok_or_else(|| ExtensionFailure::new("codemode extension is disposed"))?;
                state.lock().expect("extension state").context = Some(context.clone());
                let previous = state.lock().expect("extension state").cells.clone();
                {
                    let mut state = state.lock().expect("extension state");
                    state.completion_handles.dispose();
                    state.completion_handles = Arc::new(Default::default());
                }
                EvalDetachedCellManager::dispose(&previous)
                    .await
                    .map_err(ExtensionFailure::new)?;
                let generation = proxy.begin_replacement();
                state.lock().expect("extension state").generation = generation;
                let mut detector = interpreters::detect::InterpreterDetector::new(
                    options.js_runtime.version.clone(),
                    cfg!(windows),
                );
                let mut prepared = extension::runtime_factory::prepare_runtime(
                    extension::runtime_factory::RuntimePreparationOptions {
                        cwd: &context.cwd,
                        home_dir: &options.home_dir,
                        environment: &options.environment,
                        event: &event,
                        session_file: context.session_manager.session_file(),
                        js_runtime: options.js_runtime.clone(),
                    },
                    &mut detector,
                )
                .await
                .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                prepared.settings.env_passthrough = options.env_passthrough.clone();
                prepared.settings.foreground_window_seconds =
                    config::settings::resolve_foreground_window_seconds(
                        &prepared.settings,
                        &options.environment,
                    );
                prepared.settings.run_budget_seconds = config::settings::resolve_run_budget_seconds(
                    &prepared.settings,
                    &options.environment,
                );
                prepared.settings.hard_limit_seconds = config::settings::resolve_hard_limit_seconds(
                    &prepared.settings,
                    &options.environment,
                );
                prepared.settings.max_detached_cells = config::settings::resolve_max_detached_cells(
                    &prepared.settings,
                    &options.environment,
                );
                let completion_state = Arc::downgrade(&state);
                let complete = options.complete.clone();
                let bridge_complete: bridge::http_server::BridgeCompletionHandler = Arc::new(
                    move |request| {
                        if request.cell_id.is_none() || request.context.is_none() {
                            return Box::pin(async {
                                Err(
                                    serde_json::json!({"name":"Error","message":"codemode invocation context is unavailable"}),
                                )
                            });
                        }
                        let context = completion_state.upgrade().and_then(|state| {
                            let state = state.lock().expect("extension state");
                            (state.generation == generation)
                                .then(|| state.context.clone())
                                .flatten()
                        });
                        let complete = complete.clone();
                        Box::pin(async move {
                            let mut context=context.ok_or_else(||serde_json::json!({"name":"Error","message":"codemode session has not started"}))?;
                            if let Some(invocation) = request.context {
                                context.model = invocation.model;
                                context.cwd = invocation.cwd;
                                context.thinking_level = invocation.thinking_level;
                                context.goal_store_file = invocation.goal_store_file;
                            }
                            complete_with_signal(&complete,completion::handler::CompletionRequest {prompt:request.prompt,model:None,system:None,schema:None,opts:request.opts},context,request.signal).await.map_err(|error|serde_json::json!({"name":"Error","message":error.to_string()}))
                        })
                    },
                );
                let runtime_api = {
                    let host = host.lock().expect("extension api");
                    Arc::new(ExtensionApi::new(
                        host.registered.clone(),
                        host.profile.clone(),
                        host.events.clone(),
                        host.runtime.clone(),
                    ))
                };
                let runtime_host = extension::runtime_factory::runtime_host_from_api(
                    runtime_api,
                    &context,
                    bridge_complete,
                )?;
                let runtime = extension::runtime_factory::create_runtime(prepared, runtime_host)
                    .await
                    .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                if !proxy.replace(generation, runtime.manager.clone()).await {
                    return Ok(());
                }
                notifier.lock().expect("notifier").reset();
                let cells = create_extension_cells(
                    &host,
                    &state,
                    &ticker,
                    &notifier,
                    &runtime.settings,
                    Some(runtime.artifacts_dir.clone()),
                );
                {
                    let mut state = state.lock().expect("extension state");
                    state.context = Some(context.clone());
                    state.model_id = context.model.as_ref().map(|model| model.id.clone());
                    state.cells = cells.clone();
                    state.runtime = Some(runtime);
                }
                cells
                    .lock()
                    .expect("cell manager")
                    .publish_wake_source_state();
                let mut api = host.lock().expect("extension api");
                install_eval(&mut api, &host, &state, &proxy, &options)?;
                Ok(())
            })
        },
    );
    let recovery = Arc::new(extension::start_recovery::StartRecovery::new(
        start_session,
        Arc::new(maho_ext_api::diagnostic),
    ));
    state.lock().expect("extension state").recovery = Some(recovery.clone());
    install_eval(api, &host, &state, &proxy, &options)?;
    host.lock().expect("extension api").registered = api.registered.clone();
    let start_recovery = recovery.clone();
    api.on(
        EventKind::SessionStart,
        Arc::new(move |_, context| {
            let recovery = start_recovery.clone();
            let context = context.clone();
            Box::pin(async move {
                let event = serde_json::json!({"sessionId": context.session_manager.session_id()});
                recovery.run_start(event, context).await?;
                Ok(EventResult::None)
            })
        }),
    );
    let veto_state = state.clone();
    api.on(
        EventKind::SessionBeforeReload,
        Arc::new(move |_, _| {
            let cells = veto_state.lock().expect("extension state").cells.clone();
            let result = reload_veto(&cells.lock().expect("cell manager"))
                .map_or(EventResult::None, EventResult::SessionBefore);
            Box::pin(async move { Ok(result) })
        }),
    );
    for event in [
        EventKind::SessionShutdown,
        EventKind::SessionBeforeSwitch,
        EventKind::SessionBeforeFork,
    ] {
        let state = state.clone();
        let proxy = proxy.clone();
        let ticker = ticker.clone();
        let recovery = recovery.clone();
        api.on(
            event,
            Arc::new(move |_, _| {
                let state = state.clone();
                let proxy = proxy.clone();
                let ticker = ticker.clone();
                recovery.session_ended();
                Box::pin(async move {
                    let cells = {
                        let mut state = state.lock().expect("extension state");
                        state.generation += 1;
                        state.runtime = None;
                        state.model_id = None;
                        state.completion_handles.dispose();
                        state.cells.clone()
                    };
                    ticker.lock().expect("ticker").stop();
                    let result = EvalDetachedCellManager::dispose(&cells).await;
                    state.lock().expect("extension state").context = None;
                    proxy.dispose().await;
                    result.map_err(ExtensionFailure::new)?;
                    Ok(EventResult::None)
                })
            }),
        );
    }
    let model_host = host.clone();
    let model_state = state.clone();
    let model_proxy = proxy.clone();
    let model_options = options.clone();
    api.on(
        EventKind::ModelSelect,
        Arc::new(move |event, context| {
            let result = (|| {
                let mut state = model_state.lock().expect("extension state");
                state.context = Some(context.clone());
                let id = match event {
                    maho_ext_api::ExtensionEvent::ModelSelect(event) => {
                        Some(event.model.id.clone())
                    }
                    _ => None,
                };
                if state.runtime.is_none() || id.is_none() || id == state.model_id {
                    return Ok(());
                }
                state.model_id = id;
                drop(state);
                let mut api = model_host.lock().expect("extension api");
                install_eval(
                    &mut api,
                    &model_host,
                    &model_state,
                    &model_proxy,
                    &model_options,
                )
            })();
            Box::pin(async move {
                result?;
                Ok(EventResult::None)
            })
        }),
    );
    let turn_host = host.clone();
    let turn_state = state.clone();
    let turn_proxy = proxy.clone();
    let turn_options = options.clone();
    api.on(
        EventKind::TurnStart,
        Arc::new(move |_, _| {
            let result = (|| {
                let state = turn_state.lock().expect("extension state");
                if state.runtime.is_none() {
                    return Ok(());
                }
                drop(state);
                let mut api = turn_host.lock().expect("extension api");
                install_eval(
                    &mut api,
                    &turn_host,
                    &turn_state,
                    &turn_proxy,
                    &turn_options,
                )
            })();
            Box::pin(async move {
                result?;
                Ok(EventResult::None)
            })
        }),
    );
    Ok(())
}

/// A reload disposes the kernels together with every live cell, so it must wait for detached
/// cells to settle instead of cancelling them and the completion wake they owe the session.
fn reload_veto(cells: &EvalDetachedCellManager) -> Option<maho_ext_api::SessionBeforeEventResult> {
    let live = cells.live_cells(None, None);
    if live.is_empty() {
        return None;
    }
    let ids = live.iter().map(|cell| cell.cell_id.as_str()).collect::<Vec<_>>().join(", ");
    Some(maho_ext_api::SessionBeforeEventResult {
        cancel: Some(true),
        reason: Some(format!(
            "{} eval cell(s) still running: {ids} - wait for them to finish or stop them (eval action \"stop\") before reloading.",
            live.len()
        )),
        ..Default::default()
    })
}

fn create_extension_cells(
    host: &Arc<Mutex<ExtensionApi>>,
    state: &Arc<Mutex<ExtensionState>>,
    ticker: &Arc<Mutex<extension::eval_status_ticker::EvalStatusTicker>>,
    notifier: &Arc<Mutex<extension::eval_notifier::EvalNotifier>>,
    settings: &config::settings::CodemodeSettings,
    artifacts_dir: Option<PathBuf>,
) -> Arc<Mutex<EvalDetachedCellManager>> {
    let wake_host = Arc::downgrade(host);
    let notify_host = Arc::downgrade(host);
    let notify_state = Arc::downgrade(state);
    let notifier = notifier.clone();
    let ticker = ticker.clone();
    Arc::new(Mutex::new(EvalDetachedCellManager::new(
        DetachedCellManagerOptions {
            artifacts_dir,
            hard_limit_seconds: settings.hard_limit_seconds,
            run_budget_seconds: settings.run_budget_seconds,
            max_detached_cells: settings.max_detached_cells.ceil() as usize,
            on_status_change: Some(Arc::new(move |entries| {
                ticker.lock().expect("ticker").sync(entries)
            })),
            on_wake_source_state: Some(Arc::new(move |wake| {
                if let Some(host) = wake_host.upgrade()
                    && let Err(error) = extension::wake_source_state::emit_wake_source_state(
                        &host.lock().expect("extension api"),
                        &wake,
                    )
                {
                    maho_ext_api::diagnostic(&format!("codemode wake publication failed: {error}"));
                }
            })),
            notifier: Some(Arc::new(move |cells| {
                let Some(host) = notify_host.upgrade() else {
                    return Ok(());
                };
                let context = notify_state
                    .upgrade()
                    .and_then(|state| state.lock().expect("extension state").context.clone());
                notifier
                    .lock()
                    .expect("notifier")
                    .notify(
                        &host.lock().expect("extension api"),
                        context.as_ref(),
                        extension::eval_notifier::EvalNotifyMode::Wake,
                        &cells,
                    )
                    .map_err(|error| error.to_string())
            })),
            ..Default::default()
        },
    )))
}

fn install_eval(
    api: &mut ExtensionApi,
    host: &Arc<Mutex<ExtensionApi>>,
    state: &Arc<Mutex<ExtensionState>>,
    proxy: &Arc<extension::session_manager_proxy::SessionManagerProxy>,
    options: &Arc<CodemodeExtensionOptions>,
) -> Result<(), ExtensionFailure> {
    let settled_state = Arc::downgrade(state);
    let execution_state = Arc::downgrade(state);
    let mut state = state.lock().expect("extension state");
    let runtime = &state.runtime;
    let mut settings = runtime
        .as_ref()
        .map_or_else(config::settings::CodemodeSettings::default, |runtime| {
            runtime.settings.clone()
        });
    settings.foreground_window_seconds =
        config::settings::resolve_foreground_window_seconds(&settings, &options.environment);
    settings.run_budget_seconds =
        config::settings::resolve_run_budget_seconds(&settings, &options.environment);
    settings.hard_limit_seconds =
        config::settings::resolve_hard_limit_seconds(&settings, &options.environment);
    if let Some(runtime) = runtime {
        settings.languages = runtime.enabled_languages.clone();
    } else {
        settings.languages = config::settings::Languages { js: true };
    }
    let catalog_host = Arc::downgrade(host);
    let catalog = Arc::new(move || {
        catalog_host
            .upgrade()
            .ok_or_else(|| "codemode session manager is disposed".to_owned())?
            .lock()
            .expect("extension api")
            .get_all_tools()
            .map(|tools| {
                tools
                    .into_iter()
                    .map(|tool| bridges::schema_bridge::EvalSchemaToolInfo {
                        name: tool.name,
                        description: Some(tool.description),
                        parameters: Some(tool.parameters),
                        kernel_prelude: None,
                    })
                    .collect()
            })
            .map_err(|error| error.to_string())
    });
    let executor = runtime.as_ref().map_or_else(
        || {
            let api = Arc::new(ExtensionApi::new(
                api.registered.clone(),
                api.profile.clone(),
                api.events.clone(),
                api.runtime.clone(),
            ));
            Arc::new(extension::runtime_factory::RuntimeExecuteTool {
                api,
                active_tools: vec![],
            }) as Arc<dyn bridges::output_bridge::OutputExecuteTool>
        },
        |runtime| runtime.executor.clone(),
    );
    let cells = state.cells.clone();
    let weak_cells = Arc::downgrade(&cells);
    let complete = options.complete.clone();
    let completion_state = settled_state.clone();
    let completion_cells = weak_cells.clone();
    let completion: tool::cell_handler::CellCompletionHandler =
        Arc::new(move |request, signal, invocation| {
            let complete = complete.clone();
            let context = completion_state
                .upgrade()
                .zip(completion_cells.upgrade())
                .and_then(|(state, cells)| {
                    let state = state.lock().expect("extension state");
                    Arc::ptr_eq(&state.cells, &cells)
                        .then(|| state.context.clone())
                        .flatten()
                });
            Box::pin(async move {
                let mut context = context.ok_or_else(|| {
                    completion::handler::CompletionError("codemode session has not started".into())
                })?;
                if let Some(invocation) = invocation {
                    context.model = invocation.model;
                    context.cwd = invocation.cwd;
                    context.thinking_level = invocation.thinking_level;
                    context.goal_store_file = invocation.goal_store_file;
                }
                complete_with_signal(&complete, request, context, signal).await
            })
        });
    let events = api.events.clone();
    let on_settled = Arc::new(move |payload: serde_json::Value| {
        if let (Some(state), Some(cells)) = (settled_state.upgrade(), weak_cells.upgrade()) {
            let current = Arc::ptr_eq(&state.lock().expect("extension state").cells, &cells);
            if current {
                events.emit(tool::eval_execution_event::EVAL_EXECUTION_EVENT, &payload);
                events.emit("senpi:extension-rpc-event",&serde_json::json!({"name":tool::eval_execution_event::EVAL_EXECUTION_EVENT,"data":tool::eval_execution_event::to_eval_execution_rpc_payload(&payload)}));
            }
        }
    });
    let preludes_host = Arc::downgrade(host);
    let active_preludes_fn = Arc::new(move || {
        let host = preludes_host
            .upgrade()
            .ok_or_else(|| "codemode session manager is disposed".to_owned())?;
        let host = host.lock().expect("extension api");
        let active = host.get_active_tools().map_err(|error| error.to_string())?;
        let all = host
            .get_all_tools()
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|tool| bridges::schema_bridge::EvalSchemaToolInfo {
                name: tool.name,
                description: Some(tool.description),
                parameters: Some(tool.parameters),
                kernel_prelude: tool.kernel_prelude,
            })
            .collect::<Vec<_>>();
        Ok(extension::kernel_preludes::active_kernel_preludes(
            &active, &all,
        ))
    });
    let prompt_active = api.get_active_tools().unwrap_or_default();
    let prompt_tools = api
        .get_all_tools()
        .unwrap_or_default()
        .into_iter()
        .map(|tool| bridges::schema_bridge::EvalSchemaToolInfo {
            name: tool.name,
            description: Some(tool.description),
            parameters: Some(tool.parameters),
            kernel_prelude: tool.kernel_prelude,
        })
        .collect::<Vec<_>>();
    let prompt_preludes =
        extension::kernel_preludes::active_kernel_preludes(&prompt_active, &prompt_tools);
    let prompt = prompt::eval_prompt::EvalPromptOptions {
        model_id: state.model_id.clone(),
        spawns: runtime.as_ref().is_some_and(|runtime| runtime.spawns),
        monitor: runtime.is_some() && monitor_is_registered(api),
        spawn_default_agent: Some(settings.task_tools.task.clone()),
        host_line: Some(host_line(&options.js_runtime)),
        js_runtime: Some(options.js_runtime.clone()),
        bun_skill_path: active_bun_skill_path(&options.js_runtime),
        kernel_preludes: prompt_preludes,
        ..Default::default()
    };
    let mut definition = tool::eval_tool::create_eval_tool(Arc::new(CreateEvalToolOptions {
        kernel_manager: proxy.clone(),
        executor,
        list_tools: Some(catalog),
        complete: Some(completion),
        settings,
        artifacts_dir: runtime
            .as_ref()
            .map(|runtime| runtime.artifacts_dir.clone()),
        image_sdk: options.image_sdk.clone(),
        cell_manager: cells,
        on_cell_settled: Some(on_settled),
        prompt,
        runtimes: runtime.as_ref().map_or_else(HashMap::new, |runtime| {
            runtime.runtimes.iter().cloned().collect()
        }),
        mode: state
            .context
            .as_ref()
            .map_or("print", |context| {
                if matches!(
                    context.mode,
                    maho_ext_api::ExtensionMode::Print | maho_ext_api::ExtensionMode::Json
                ) {
                    "print"
                } else {
                    "interactive"
                }
            })
            .into(),
        active_kernel_preludes: Some(active_preludes_fn),
        eval_handle_host: api.runtime.eval_handle_host()?,
        owner_session_id: state
            .context
            .as_ref()
            .map(|context| context.session_manager.session_id().to_owned()),
        completion_handles: state.completion_handles.clone(),
    }))
    .map_err(ExtensionFailure::new)?;
    state.eval_execute = Some(definition.execute.clone());
    let recovery = state.recovery.clone();
    drop(state);
    definition.execute = Arc::new(move |call| {
        let state = execution_state.upgrade();
        let recovery = recovery.clone();
        Box::pin(async move {
            use maho_tools::definition::{ToolCall, ToolError};
            let state = state
                .ok_or_else(|| ToolError::Message("codemode session manager is disposed".into()))?;
            let execute = state
                .lock()
                .expect("extension state")
                .eval_execute
                .clone()
                .ok_or_else(|| ToolError::Message("codemode session has not started".into()))?;
            let retry = ToolCall {
                id: call.id,
                params: call.params.clone(),
                signal: call.signal.clone(),
                on_update: call.on_update.clone(),
                context: call.context,
            };
            let result = execute(call).await;
            match &result {
                Err(ToolError::Message(message))
                    if matches!(
                        message.as_str(),
                        "codemode session has not started" | "codemode session manager is disposed"
                    ) => {}
                _ => return result,
            }
            let context = state.lock().expect("extension state").context.clone();
            let (Some(recovery), Some(mut context)) = (recovery, context) else {
                return result;
            };
            if let Some(invocation) = retry.context {
                if invocation.session_manager().session_id() != context.session_manager.session_id()
                {
                    return result;
                }
                context.cwd = invocation.cwd().into();
                context.model = invocation.model().cloned();
                context.thinking_level = invocation.thinking_level();
                context.goal_store_file = invocation.goal_store_file().map(PathBuf::from);
            }
            let session_id = context.session_manager.session_id().to_owned();
            if !recovery
                .recover(context, &session_id)
                .await
                .map_err(|error| ToolError::Message(error.to_string()))?
            {
                return result;
            }
            let execute = state
                .lock()
                .expect("extension state")
                .eval_execute
                .clone()
                .ok_or_else(|| ToolError::Message("codemode session has not started".into()))?;
            execute(retry).await
        })
    });
    api.register_tool_with_renderers(definition, tool::render::eval_tool_renderers())
}

/// Whether the session registry holds `monitor`; false when the runtime cannot be read yet.
fn monitor_is_registered(api: &ExtensionApi) -> bool {
    api.get_all_tools()
        .map(|tools| tools.iter().any(|tool| tool.name == "monitor"))
        .unwrap_or(false)
}

fn active_bun_skill_path(js_runtime: &tool::types::EvalRuntimeInfo) -> Option<String> {
    let version = (js_runtime.name == "bun").then_some(js_runtime.version.as_str());
    extension::skill_contribution::active_bun_skill_path(
        version,
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"),
    )
    .map(|path| path.to_string_lossy().into_owned())
}

fn host_line(js_runtime: &tool::types::EvalRuntimeInfo) -> String {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    };
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    [
        Some(format!("{platform} {arch}")),
        CPU_MODEL.get_or_init(cpu_model).clone(),
        Some(format!("{cores} cores")),
        Some(format!("{} {}", js_runtime.name, js_runtime.version)),
    ]
    .into_iter()
    .flatten()
    .filter(|part| !part.trim().is_empty())
    .collect::<Vec<_>>()
    .join(" \u{00b7} ")
}

/// The CPU model never changes within a process; read it once instead of on every `TurnStart`.
static CPU_MODEL: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

fn cpu_model() -> Option<String> {
    if cfg!(target_os = "macos") {
        let output = std::process::Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .filter(|output| output.status.success())?;
        return Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|model| !model.is_empty());
    }
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    cpuinfo
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| matches!(key.trim(), "model name" | "Hardware" | "Processor"))
                .map(|(_, value)| value.trim().to_owned())
        })
        .filter(|model| !model.is_empty())
}

async fn complete_with_signal(
    complete: &ContextCompletion,
    request: completion::handler::CompletionRequest,
    mut context: ExtensionContext,
    signal: maho_ai::utils::abort::AbortSignal,
) -> Result<serde_json::Value, completion::handler::CompletionError> {
    let request = completion::handler::normalize_request(request);
    let tier = completion::handler::resolve_completion_tier(request.model.as_deref())?;
    if tier != completion::handler::CompletionTier::Default {
        context.model = Some(completion::handler::resolve_requested_model(
            tier,
            context.model.as_ref(),
            &context.model_registry.get_available(),
        )?);
    }
    let cancellation = maho_ext_api::AbortSignal::default();
    context.signal = Some(cancellation.clone());
    if signal.aborted() {
        cancellation.abort();
    }
    let operation = complete(request, context);
    await_completion(operation, cancellation, signal).await
}

async fn await_completion(
    operation: impl std::future::Future<
        Output = Result<serde_json::Value, completion::handler::CompletionError>,
    >,
    cancellation: maho_ext_api::AbortSignal,
    signal: maho_ai::utils::abort::AbortSignal,
) -> Result<serde_json::Value, completion::handler::CompletionError> {
    struct CancellationGuard(Option<maho_ext_api::AbortSignal>);
    impl Drop for CancellationGuard {
        fn drop(&mut self) {
            if let Some(signal) = &self.0 {
                signal.abort();
            }
        }
    }
    let mut guard = CancellationGuard(Some(cancellation));
    tokio::pin!(operation);
    tokio::select! {
        result=&mut operation=>{guard.0.take();result},
        ()=signal.cancelled()=>Err(completion::handler::CompletionError("completion() request aborted.".into()))
    }
}

#[cfg(test)]
mod host_line_tests {
    use super::*;

    #[test]
    fn host_line_names_platform_cores_and_js_runtime() {
        let line = host_line(&tool::types::EvalRuntimeInfo {
            name: "bun".into(),
            version: "1.4.0".into(),
            path: None,
            isolation: None,
        });
        assert!(line.contains(" cores"), "{line}");
        assert!(line.ends_with(" \u{00b7} bun 1.4.0"), "{line}");
    }
}

#[cfg(test)]
mod reload_veto_tests {
    use super::*;
    use tool::types::{EvalLanguage, EvalToolInput};

    #[tokio::test]
    async fn live_detached_cell_vetoes_reload_until_it_settles() {
        let mut cells = EvalDetachedCellManager::new(Default::default());
        assert!(reload_veto(&cells).is_none());
        let input = EvalToolInput { language: EvalLanguage::Js, code: "1".into(), summary: "check".into(), action: None, timeout: None, on_timeout: None, reset: None, isolate: None };
        let cell = cells.create("cell-1".into(), input).expect("create cell");
        cells.bind_kernel(&cell, Arc::new(|| maho_ext_api::AgentToolResult::text("")), Arc::new(|| (None, vec![])));
        assert!(cells.detach(&cell));
        let veto = reload_veto(&cells).expect("a live detached cell must veto reload");
        assert_eq!(veto.cancel, Some(true));
        assert!(veto.reason.expect("veto reason").contains("cell-1"));
        assert!(cells.cancel_without_interrupt(&cell));
        assert!(reload_veto(&cells).is_none());
        cells.flush_notifications().await.expect("notification flush");
    }
}

#[cfg(test)]
mod completion_cancellation_tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_drops_completion_that_ignores_signal() {
        struct Release(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for Release {
            fn drop(&mut self) {
                if let Some(sender) = self.0.take() {
                    let _ = sender.send(());
                }
            }
        }
        let (sender, released) = tokio::sync::oneshot::channel();
        let guard = Release(Some(sender));
        let operation = async move {
            let _guard = guard;
            std::future::pending().await
        };
        let cancellation = maho_ext_api::AbortSignal::default();
        let controller = maho_ai::utils::abort::AbortController::new();
        let outcome = {
            let future = await_completion(operation, cancellation.clone(), controller.signal());
            tokio::pin!(future);
            assert!(
                std::future::poll_fn(|cx| std::task::Poll::Ready(future.as_mut().poll(cx)))
                    .await
                    .is_pending()
            );
            controller.abort(None);
            tokio::time::timeout(std::time::Duration::from_secs(1), future).await
        };
        assert!(cancellation.is_aborted());
        assert!(
            outcome
                .expect("cancelled completion must not await ignored work")
                .is_err()
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), released)
            .await
            .unwrap()
            .unwrap();
    }
}
