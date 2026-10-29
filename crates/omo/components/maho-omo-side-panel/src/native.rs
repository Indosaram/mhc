use crate::{
    commands::PanelCommandContext,
    constants::{SIDE_PANEL_DIFF_COMMAND, SIDE_PANEL_FLAG},
    context::PanelEventContext,
    native_readers::{NativeExec, NativeReaders, load_config},
    native_ui::NativePanelUi,
    runtime::{
        SidePanelController, SidePanelRuntimeOptions, TokioPanelTimers,
        create_side_panel_controller,
    },
    settings::resolve_omo_side_panel_settings,
};
use maho_ext_api::*;
use std::sync::{Arc, Mutex};

pub struct NativeSidePanel;
fn panel_enabled(
    settings: &crate::settings::OmoSidePanelSettings,
    flag: Option<FlagValue>,
) -> bool {
    match flag {
        Some(FlagValue::Boolean(value)) => value,
        Some(FlagValue::String(_)) | None => settings.enabled,
    }
}
impl Extension for NativeSidePanel {
    fn register(&self, api: &mut ExtensionApi) {
        api.register_flag(
            SIDE_PANEL_FLAG,
            FlagType::Boolean { default: None },
            Some("Force the omo side panel on for this run".into()),
        );
        let controller: Arc<Mutex<Option<Arc<SidePanelController>>>> = Arc::new(Mutex::new(None));
        let cwd = api.cwd.to_string_lossy().into_owned();
        match resolve_omo_side_panel_settings(&load_config(&cwd)) {
            Ok(settings) if panel_enabled(&settings, api.get_flag(SIDE_PANEL_FLAG)) => {
                let state = controller.clone();
                api.register_command(
                    SIDE_PANEL_DIFF_COMMAND,
                    Some("Open the diff of a file the side panel lists as changed.".into()),
                    None,
                    Arc::new(move |_, ctx| {
                        let current = state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone();
                        Box::pin(async move {
                            if let Some(current) = current {
                                current
                                    .run_diff_command(&PanelCommandContext {
                                        mode: Some("tui".into()),
                                        ui: Some(Arc::new(NativePanelUi(ctx.ui.clone()))),
                                    })
                                    .await;
                            } else {
                                ctx.ui.notify(
                                    "The side panel is off in this session.",
                                    NotificationType::Info,
                                );
                            }
                            Ok(())
                        })
                    }),
                );
            }
            Ok(_) => {}
            Err(error) => eprintln!("side panel: {error}"),
        }
        let runtime = api.runtime.clone();
        for kind in [
            EventKind::SessionStart,
            EventKind::TurnEnd,
            EventKind::AgentSettled,
            EventKind::MessageEnd,
            EventKind::ToolExecutionStart,
            EventKind::ToolExecutionEnd,
            EventKind::Input,
            EventKind::SessionShutdown,
        ] {
            let state = controller.clone();
            let runtime = runtime.clone();
            api.on(
                kind,
                Arc::new(move |event, ctx| {
                    let state = state.clone();
                    let flag = runtime.get_flag(SIDE_PANEL_FLAG);
                    let runtime = runtime.clone();
                    Box::pin(async move {
                        if matches!(event, ExtensionEvent::SessionShutdown(_)) {
                            if let Some(current) = state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .take()
                            {
                                current.teardown();
                            }
                            return Ok(EventResult::None);
                        }
                        if matches!(event, ExtensionEvent::SessionStart(_)) {
                            let previous = state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .take();
                            if let Some(previous) = previous {
                                previous.teardown();
                            }
                            if !ctx.has_ui || ctx.mode != ExtensionMode::Tui {
                                return Ok(EventResult::None);
                            }
                            let cwd = ctx.cwd.to_string_lossy().into_owned();
                            let settings = resolve_omo_side_panel_settings(&load_config(&cwd))
                                .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                            if !panel_enabled(&settings, flag) {
                                return Ok(EventResult::None);
                            }
                            let exec = match runtime.session_actions() {
                                Ok(actions) => Some(Arc::new(NativeExec(actions)) as Arc<dyn crate::commands::PanelExec>),
                                Err(error) => {
                                    eprintln!("side panel: files unavailable: {error}");
                                    None
                                }
                            };
                            let agent_dir = ctx.agent_dir.clone();
                            let usage_runtime = runtime.clone();
                            let usage = Arc::new(move |on_change| {
                                let get_runtime = usage_runtime.clone();
                                let subscribe_runtime = usage_runtime.clone();
                                crate::usage::NativeUsage::create(
                                    agent_dir.clone(),
                                    Arc::new(move || match get_runtime.message_actions() {
                                        Ok(actions) => actions.get_subscription_limits(),
                                        Err(error) => { eprintln!("side panel: subscription limits unavailable: {error}"); Vec::new() }
                                    }),
                                    Arc::new(move |listener| match subscribe_runtime.message_actions() {
                                        Ok(actions) => actions.on_subscription_limits(Arc::new(move |_| listener())),
                                        Err(error) => { eprintln!("side panel: subscription listener unavailable: {error}"); None }
                                    }),
                                    on_change,
                                )
                            });
                            let current = create_side_panel_controller(SidePanelRuntimeOptions {
                                cwd,
                                regular_notice_state_dir: None,
                                now: Arc::new(now),
                                readers: Arc::new(NativeReaders::new(exec.clone())),
                                exec,
                                timers: Arc::new(TokioPanelTimers),
                                defer: Arc::new(|callback| {
                                    tokio::spawn(async move {
                                        tokio::task::yield_now().await;
                                        callback();
                                    });
                                }),
                                usage: Some(usage),
                                track: None,
                                debug: Arc::new(|message| eprintln!("{message}")),
                            });
                            let previous = state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .replace(current.clone());
                            if let Some(previous) = previous {
                                previous.teardown();
                            }
                            current
                                .session_start(
                                    settings,
                                    &PanelEventContext {
                                        value: facts(ctx),
                                        ui: Some(Arc::new(NativePanelUi(ctx.ui.clone()))),
                                    },
                                )
                                .await;
                            return Ok(EventResult::None);
                        }
                        let current = state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone();
                        if let Some(current) = current {
                            match event {
                                ExtensionEvent::Input(_) => current.input(),
                                ExtensionEvent::ToolExecutionStart {
                                    tool_name, args, ..
                                } => current.tool_start(
                                    &serde_json::json!({"toolName":tool_name,"args":args}),
                                    facts(ctx),
                                ),
                                ExtensionEvent::ToolExecutionEnd { .. } => current.tool_end(),
                                _ => current.refresh(facts(ctx)),
                            }
                        }
                        Ok(EventResult::None)
                    })
                }),
            );
        }
    }
}
pub fn now() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
fn facts(ctx: &ExtensionContext) -> serde_json::Value {
    let mut value = serde_json::json!({"mode":"tui","hasUI":ctx.has_ui,"sessionId":ctx.session_manager.session_id(),"model":ctx.model.as_ref().map(|model|&model.id),"goalStoreFile":ctx.goal_store_file.as_ref().map(|path|path.to_string_lossy())});
    if let Ok(Some(usage)) = ctx.get_context_usage() {
        value["usage"] = serde_json::json!({"tokens":usage.tokens,"contextWindow":usage.context_window,"percent":usage.percent});
    }
    if let Some(totals) = ctx.session_manager.get_usage_totals() {
        value["totals"] = totals;
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_flags_override_the_persistent_enabled_switch() {
        let mut settings = crate::settings::OmoSidePanelSettings {
            enabled: false,
            ..Default::default()
        };
        assert!(!panel_enabled(&settings, None));
        assert!(panel_enabled(&settings, Some(FlagValue::Boolean(true))));
        settings.enabled = true;
        assert!(panel_enabled(&settings, None));
        assert!(!panel_enabled(&settings, Some(FlagValue::Boolean(false))));
    }
}
