use crate::websearch::{
    search::provider_entry_label,
    types::{ConfigLoadFailureReason, ConfigLoadResult, RoutingStrategy},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusLevel {
    Info,
    Warning,
    Error,
}
pub type ProviderNativeBypass =
    std::sync::Arc<dyn Fn(Option<&maho_ext_api::Model>) -> bool + Send + Sync>;
pub struct WebsearchExtension {
    pub home: std::path::PathBuf,
    pub provider_native_bypass: ProviderNativeBypass,
}
impl maho_ext_api::Extension for WebsearchExtension {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        let state=std::sync::Arc::new(std::sync::Mutex::new(ConfigLoadResult::Err{reason:ConfigLoadFailureReason::MissingConfig,message:"Missing websearch config. Create .pi/websearch.json or ~/.pi/websearch.json before starting pi.".into(),source:None}));
        let native = std::sync::Arc::new(std::sync::Mutex::new(
            crate::server_search::ServerSearchState::default(),
        ));
        let captured = state.clone();
        let get_state: std::sync::Arc<dyn Fn() -> ConfigLoadResult + Send + Sync> =
            std::sync::Arc::new(move || {
                captured
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            });
        if let Err(error) = api.register_tool_with_renderers(
            crate::websearch::tool::create_web_search_tool(get_state.clone()),
            crate::websearch::renderers::renderers(),
        ) {
            std::panic::panic_any(error);
        }
        let execute_state = get_state.clone();
        let routing = std::sync::Arc::new(tokio::sync::Mutex::new((String::new(), None)));
        let definition = crate::websearch::tool::create_web_search_tool(get_state.clone());
        let scope = api.runtime.registration_scope();
        let execute_native = native.clone();
        let execute_home = self.home.clone();
        let execute_config = state.clone();
        let mut executor_api = maho_ext_api::ExtensionApi::new(
            api.registered.clone(),
            api.profile.clone(),
            api.events.clone(),
            scope.clone(),
        );
        executor_api
            .register_tool_with_extension_context(
                definition,
                std::sync::Arc::new(move |id, params, signal, on_update, ctx| {
                    let state = execute_state.clone();
                    let routing = routing.clone();
                    let native = execute_native.clone();
                    let home = execute_home.clone();
                    let config_state = execute_config.clone();
                    Box::pin(async move {
                        if signal.as_ref().is_some_and(|signal| signal.aborted()) {
                            return Err(maho_ext_api::ExtensionFailure::new(
                                "The operation was aborted.",
                            ));
                        }
                        // A function call can survive in history or arrive through eval even when the
                        // provider exposes server search. Execute it, rather than returning a bypass notice.
                        let bypass = matches!(
                            state(),
                            ConfigLoadResult::Err {
                                reason: ConfigLoadFailureReason::ProviderNativeBypass,
                                ..
                            }
                        );
                        let failed = {
                            let mut native = native
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            if bypass && let Some(model) = ctx.model.as_ref() {
                                native.fail(model);
                            }
                            native.failed(ctx.model.as_ref())
                        };
                        let state = if bypass || failed {
                            let mut loaded = refresh_state(&ctx.cwd, &home, false).await?;
                            if let ConfigLoadResult::Ok { config, .. } = &mut loaded {
                                config.auto = false;
                            }
                            *config_state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                loaded.clone();
                            std::sync::Arc::new(move || loaded.clone())
                                as std::sync::Arc<dyn Fn() -> ConfigLoadResult + Send + Sync>
                        } else {
                            state
                        };
                        let tool = crate::websearch::tool::create_web_search_tool_with_registry(
                            state,
                            ctx.model.clone(),
                            Some(ctx.model_registry.clone()),
                            routing,
                        );
                        let tool_signal = maho_ext_api::AbortSignal::default();
                        let link = signal.as_ref().map(|signal| {
                            let target = tool_signal.clone();
                            signal.add_abort_listener(move |_| target.abort())
                        });
                        struct Link(
                            Option<maho_ai::utils::abort::AbortSignal>,
                            Option<maho_ai::utils::abort::ListenerId>,
                        );
                        impl Drop for Link {
                            fn drop(&mut self) {
                                if let (Some(signal), Some(id)) = (&self.0, self.1) {
                                    signal.remove_abort_listener(id);
                                }
                            }
                        }
                        let _link = Link(signal.clone(), link);
                        if signal.as_ref().is_some_and(|signal| signal.aborted()) {
                            tool_signal.abort();
                        }
                        let convert =
                            |result: maho_ext_api::ToolResult| maho_ext_api::AgentToolResult {
                                is_error: result.is_error,
                                details: result.details.unwrap_or(serde_json::Value::Null),
                                ..maho_ext_api::AgentToolResult::text(
                                    result
                                        .content
                                        .iter()
                                        .filter_map(|block| match block {
                                            maho_ext_api::ToolContent::Text { text, .. } => {
                                                Some(text.as_str())
                                            }
                                            _ => None,
                                        })
                                        .collect::<Vec<_>>()
                                        .join("\n"),
                                )
                            };
                        let update: on_update_type::Update = on_update.map(|update| {
                            std::sync::Arc::new(move |result| {
                                update(convert(result));
                                Ok(())
                            }) as _
                        });
                        let result = (tool.execute)(maho_ext_api::ToolCall {
                            id,
                            params,
                            signal: tool_signal,
                            on_update: update,
                            context: Some(ctx),
                        })
                        .await
                        .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                        Ok(convert(result))
                    })
                }),
            )
            .unwrap_or_else(|error| std::panic::panic_any(error));
        scope
            .commit_registration()
            .unwrap_or_else(|error| std::panic::panic_any(error));
        register_websearch_command(api, get_state);
        for event in [
            maho_ext_api::EventKind::SessionStart,
            maho_ext_api::EventKind::ModelSelect,
            maho_ext_api::EventKind::BeforeAgentStart,
        ] {
            let state = state.clone();
            let home = self.home.clone();
            let bypass = self.provider_native_bypass.clone();
            let native = native.clone();
            api.on(event,std::sync::Arc::new(move |event,ctx| {
                let state=state.clone();let home=home.clone();let bypass=bypass.clone();let native=native.clone();
                Box::pin(async move {
                    if matches!(event,maho_ext_api::ExtensionEvent::BeforeAgentStart(event) if event.preview || event.trigger=="extension") {return Ok(maho_ext_api::EventResult::None);}
                    let session_start=matches!(event,maho_ext_api::ExtensionEvent::SessionStart(_));
                    let model=match event {maho_ext_api::ExtensionEvent::ModelSelect(event)=>Some(&event.model),_=>ctx.model.as_ref()};
                    let failed={let mut native=native.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if session_start{*native=Default::default();}else{native.new_turn();}native.failed(model)};
                    let mut next=refresh_state(&ctx.cwd,&home,!failed && bypass(model)).await?;
                    if failed && let ConfigLoadResult::Ok{config,..}=&mut next {config.auto=false;}
                    *state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=next.clone();
                    clear_ui(ctx);
                    if ctx.has_ui && let ConfigLoadResult::Err{reason,message,..}=&next && *reason!=ConfigLoadFailureReason::ProviderNativeBypass {ctx.ui.notify(message,maho_ext_api::NotificationType::Error);}
                    Ok(maho_ext_api::EventResult::None)
                })
            }));
        }
        register_fallback(
            api,
            state,
            native,
            self.home.clone(),
            self.provider_native_bypass.clone(),
        );
        api.on(
            maho_ext_api::EventKind::SessionShutdown,
            std::sync::Arc::new(|_, ctx| {
                Box::pin(async move {
                    clear_ui(ctx);
                    Ok(maho_ext_api::EventResult::None)
                })
            }),
        );
    }
}
fn register_fallback(
    api: &mut maho_ext_api::ExtensionApi,
    config: std::sync::Arc<std::sync::Mutex<ConfigLoadResult>>,
    native: std::sync::Arc<std::sync::Mutex<crate::server_search::ServerSearchState>>,
    home: std::path::PathBuf,
    bypass: ProviderNativeBypass,
) {
    use maho_ext_api::{AgentMessage, EventKind, EventResult, ExtensionEvent};
    use std::sync::Arc;
    let request_state = native.clone();
    api.on(
        EventKind::BeforeProviderRequest,
        Arc::new(move |event, ctx| {
            let state = request_state.clone();
            Box::pin(async move {
                let ExtensionEvent::BeforeProviderRequest { payload, model, .. } = event else {
                    return Ok(EventResult::None);
                };
                let model = model.as_ref().or(ctx.model.as_ref());
                let mut state = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.request_model = model.cloned();
                Ok(if state.failed(model) {
                    EventResult::ProviderPayload(crate::server_search::fallback_payload(payload))
                } else {
                    EventResult::None
                })
            })
        }),
    );
    let message_state = native.clone();
    api.on(
        EventKind::MessageEnd,
        Arc::new(move |event, ctx| {
            let state = message_state.clone();
            let config = config.clone();
            let home = home.clone();
            let bypass = bypass.clone();
            Box::pin(async move {
                let ExtensionEvent::MessageEnd {
                    message: AgentMessage::Llm(maho_ai::types::Message::Assistant(message)),
                } = event
                else {
                    return Ok(EventResult::None);
                };
                if !crate::server_search::search_failed(message) {
                    state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .completed(message);
                    return Ok(EventResult::None);
                }
                let model = {
                    let state = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state
                        .request_model
                        .as_ref()
                        .filter(|model| crate::server_search::matches_message(model, message))
                        .or_else(|| {
                            ctx.model.as_ref().filter(|model| {
                                crate::server_search::matches_message(model, message)
                            })
                        })
                        .cloned()
                };
                let Some(model) = model.filter(|model| bypass(Some(model))) else {
                    return Ok(EventResult::None);
                };
                if !state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .record_failure(&model, message)
                {
                    return Ok(EventResult::None);
                }
                if ctx.model.as_ref().is_some_and(|current| {
                    current.api == model.api
                        && current.provider == model.provider
                        && current.id == model.id
                        && current.base_url == model.base_url
                }) {
                    let mut loaded = refresh_state(&ctx.cwd, &home, false).await?;
                    if let ConfigLoadResult::Ok { config, .. } = &mut loaded {
                        config.auto = false;
                    }
                    *config
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = loaded;
                }
                if ctx.has_ui {
                    ctx.ui.notify(
                        "Native web search failed; using standalone web search.",
                        maho_ext_api::NotificationType::Warning,
                    );
                }
                Ok(EventResult::None)
            })
        }),
    );
    for kind in [EventKind::AgentEnd, EventKind::SessionAbort] {
        let state = native.clone();
        api.on(
            kind,
            Arc::new(move |event, _| {
                state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .end(event);
                Box::pin(async { Ok(EventResult::None) })
            }),
        );
    }
    let sender = maho_ext_api::ExtensionApi::new(
        api.registered.clone(),
        api.profile.clone(),
        api.events.clone(),
        api.runtime.clone(),
    );
    api.on(EventKind::AgentSettled, Arc::new(move |_, ctx| {
        let send = native.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take_continuation(ctx.model.as_ref());
        let result = if send {
            sender.send_message(maho_ext_api::CustomMessage {
                custom_type: "websearch-native-fallback".into(),
                content: vec![maho_ext_api::ToolContent::text("Continue the original request using the standalone web_search tool. Do not retry native server search on this turn.")],
                display: false,
                details: None,
            }, maho_ext_api::SendMessageOptions { trigger_turn: true, ..Default::default() })
        } else { Ok(()) };
        Box::pin(async move { result.map(|()| EventResult::None) })
    }));
}

mod on_update_type {
    pub type Update = Option<
        std::sync::Arc<
            dyn Fn(maho_ext_api::ToolResult) -> Result<(), maho_ext_api::ToolError> + Send + Sync,
        >,
    >;
}
fn clear_ui(ctx: &maho_ext_api::ExtensionContext) {
    if ctx.has_ui {
        ctx.ui.set_status("pi-websearch", None);
        ctx.ui.set_widget("pi-websearch", None, Default::default());
    }
}
async fn refresh_state(
    cwd: &std::path::Path,
    home: &std::path::Path,
    bypass: bool,
) -> Result<ConfigLoadResult, maho_ext_api::ExtensionFailure> {
    if bypass {
        Ok(ConfigLoadResult::Err {
            reason: ConfigLoadFailureReason::ProviderNativeBypass,
            message: "Native provider web search is handled by the built-in provider extension."
                .into(),
            source: None,
        })
    } else {
        crate::websearch::config::load_websearch_config(cwd, home)
            .await
            .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))
    }
}
pub fn register_websearch_command(
    api: &mut maho_ext_api::ExtensionApi,
    get_state: std::sync::Arc<dyn Fn() -> ConfigLoadResult + Send + Sync>,
) {
    api.register_command(
        "websearch",
        Some("Show web search provider status".into()),
        None,
        std::sync::Arc::new(move |raw, ctx| {
            let get_state = get_state.clone();
            Box::pin(async move {
                let (message, level) = status_message(raw, &get_state());
                ctx.ui.notify(
                    &message,
                    match level {
                        StatusLevel::Info => maho_ext_api::NotificationType::Info,
                        StatusLevel::Warning => maho_ext_api::NotificationType::Warning,
                        StatusLevel::Error => maho_ext_api::NotificationType::Error,
                    },
                );
                Ok(())
            })
        }),
    );
}
pub fn status_message(raw_args: &str, state: &ConfigLoadResult) -> (String, StatusLevel) {
    let args=raw_args.trim_matches(|character:char|matches!(character,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'));
    if !args.is_empty() && args != "status" {
        return ("Usage: /websearch status".into(), StatusLevel::Warning);
    }
    match state {
        ConfigLoadResult::Ok { config, .. } => {
            let strategy = match config.strategy {
                RoutingStrategy::Priority => "priority",
                RoutingStrategy::RoundRobin => "round-robin",
                RoutingStrategy::FillFirst => "fill-first",
            };
            let providers = config
                .providers
                .iter()
                .map(|entry| {
                    provider_entry_label(
                        entry.config.provider.as_str(),
                        entry.config.id.as_deref(),
                        None,
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            (
                format!(
                    "Web search active: strategy={strategy}, auto={}, providers={providers}",
                    if config.auto { "enabled" } else { "disabled" }
                ),
                StatusLevel::Info,
            )
        }
        ConfigLoadResult::Err {
            reason: ConfigLoadFailureReason::ProviderNativeBypass,
            message,
            ..
        } => (format!("Web search deferred: {message}"), StatusLevel::Info),
        ConfigLoadResult::Err { message, .. } => (
            format!("Web search inactive: {message}"),
            StatusLevel::Error,
        ),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn lifecycle_refresh_bypass_precedes_config_and_registration_is_lazy() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        let home = temp.path().join("home");
        tokio::fs::create_dir_all(cwd.join(".senpi")).await.unwrap();
        tokio::fs::write(cwd.join(".senpi/websearch.json"), "invalid")
            .await
            .unwrap();
        assert!(matches!(
            refresh_state(&cwd, &home, true).await.unwrap(),
            ConfigLoadResult::Err {
                reason: ConfigLoadFailureReason::ProviderNativeBypass,
                ..
            }
        ));
        assert!(matches!(
            refresh_state(&cwd, &home, false).await.unwrap(),
            ConfigLoadResult::Err {
                reason: ConfigLoadFailureReason::InvalidConfig,
                ..
            }
        ));
        let mut api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("websearch", Default::default(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        maho_ext_api::Extension::register(
            &WebsearchExtension {
                home,
                provider_native_bypass: std::sync::Arc::new(|_| {
                    panic!("registration must not probe native model")
                }),
            },
            &mut api,
        );
        assert_eq!(api.registered.tools[0].definition.name, "web_search");
        assert_eq!(api.registered.commands[0].name, "websearch");
        for event in [
            maho_ext_api::EventKind::SessionStart,
            maho_ext_api::EventKind::ModelSelect,
            maho_ext_api::EventKind::SessionShutdown,
        ] {
            assert_eq!(api.registered.handlers[&event].len(), 1);
        }
    }
    #[test]
    fn status_argument_uses_ecmascript_whitespace() {
        let state = ConfigLoadResult::Err {
            reason: ConfigLoadFailureReason::ProviderNativeBypass,
            message: "native".into(),
            source: None,
        };
        assert_eq!(
            status_message("\u{feff}status\u{feff}", &state).1,
            StatusLevel::Info
        );
        assert_eq!(
            status_message("\u{0085}status", &state).1,
            StatusLevel::Warning
        );
    }
    #[test]
    fn status_command_registers_native_handler() {
        let mut api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new(
                "websearch",
                std::path::PathBuf::new(),
                Default::default(),
            ),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        register_websearch_command(
            &mut api,
            std::sync::Arc::new(|| ConfigLoadResult::Err {
                reason: ConfigLoadFailureReason::MissingConfig,
                message: "missing".into(),
                source: None,
            }),
        );
        assert!(
            api.registered
                .commands
                .iter()
                .any(|command| command.name == "websearch")
        );
    }
    #[test]
    fn status_reason_controls_notification_level() {
        let state = ConfigLoadResult::Err {
            reason: ConfigLoadFailureReason::ProviderNativeBypass,
            message: "native".into(),
            source: None,
        };
        assert_eq!(status_message("status", &state).1, StatusLevel::Info);
        assert_eq!(status_message("reload", &state).1, StatusLevel::Warning);
        let state = ConfigLoadResult::Err {
            reason: ConfigLoadFailureReason::MissingConfig,
            message: "missing".into(),
            source: None,
        };
        assert_eq!(status_message("", &state).1, StatusLevel::Error);
    }
}
