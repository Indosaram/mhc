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
        let last_served_by = LastServedBy::default();
        let execute_served = last_served_by.clone();
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
                    let served = execute_served.clone();
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
                        // `onSearchComplete`: remember which route and model served a successful search.
                        if let Some(details) = result.details.as_ref().filter(|details| {
                            details.get("results").is_some() && details.get("error").is_none()
                        }) {
                            *served
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                Some(crate::websearch::route_attempts::attempt_route_label(
                                    details["provider"].as_str().unwrap_or_default(),
                                    None,
                                    details["entryId"].as_str(),
                                    details["model"].as_str(),
                                ));
                        }
                        Ok(convert(result))
                    })
                }),
            )
            .unwrap_or_else(|error| std::panic::panic_any(error));
        scope
            .commit_registration()
            .unwrap_or_else(|error| std::panic::panic_any(error));
        register_websearch_command_with(api, get_state, last_served_by.clone());
        register_moonshot_server_search(api);
        for event in [
            maho_ext_api::EventKind::SessionStart,
            maho_ext_api::EventKind::ModelSelect,
            maho_ext_api::EventKind::BeforeAgentStart,
        ] {
            let state = state.clone();
            let home = self.home.clone();
            let bypass = self.provider_native_bypass.clone();
            let native = native.clone();
            let served = last_served_by.clone();
            let runtime = api.runtime.clone();
            api.on(event,std::sync::Arc::new(move |event,ctx| {
                let state=state.clone();let home=home.clone();let bypass=bypass.clone();let native=native.clone();let served=served.clone();let runtime=runtime.clone();
                Box::pin(async move {
                    if matches!(event,maho_ext_api::ExtensionEvent::BeforeAgentStart(event) if event.preview || event.trigger=="extension") {return Ok(maho_ext_api::EventResult::None);}
                    let session_start=matches!(event,maho_ext_api::ExtensionEvent::SessionStart(_));
                    if session_start {*served.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=None;}
                    let model=match event {maho_ext_api::ExtensionEvent::ModelSelect(event)=>Some(&event.model),_=>ctx.model.as_ref()};
                    let failed={let mut native=native.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if session_start{*native=Default::default();}else{native.new_turn();}native.failed(model)};
                    let mut next=refresh_state(&ctx.cwd,&home,!failed && bypass(model)).await?;
                    let moonshot=!failed && crate::server_search::additional_server_search(model)==Some(crate::server_search::AdditionalServerSearch::Moonshot);
                    if let Ok(actions)=runtime.session_actions() && let Some(next)=moonshot_active_tools(actions.get_active_tools()?,moonshot) {actions.set_active_tools(next)?;}
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
/// Moonshot's built-in server search: the model calls `$web_search` and the official flow echoes the
/// generated arguments back to the same model, never making a separate request.
pub fn moonshot_server_search_tool() -> maho_tools::definition::ToolDefinition {
    let mut tool = maho_tools::definition::ToolDefinition::new(
        "$web_search",
        "Return the Moonshot built-in server search result to the same model.",
        serde_json::json!({"type":"object","additionalProperties":{}}),
        std::sync::Arc::new(|call| {
            Box::pin(async move {
                Ok(maho_tools::definition::ToolResult {
                    content: vec![maho_tools::definition::ToolContent::text(
                        call.params.to_string(),
                    )],
                    details: Some(call.params),
                    structured_content: None,
                    is_error: None,
                })
            })
        }),
    );
    tool.label = "Moonshot Server Search".into();
    tool.exposure = Some(maho_tools::definition::ToolExposure::Search);
    tool.allow_lazy_activation = Some(false);
    tool
}
fn register_moonshot_server_search(api: &mut maho_ext_api::ExtensionApi) {
    api.register_tool(moonshot_server_search_tool());
}
/// `$web_search` is active only while the session model is a Moonshot route whose server search has
/// not failed; the active tool list changes only when that answer changes.
pub fn moonshot_active_tools(active: Vec<String>, enabled: bool) -> Option<Vec<String>> {
    let present = active.iter().any(|name| name == "$web_search");
    if enabled == present {
        return None;
    }
    let mut next: Vec<String> = active
        .into_iter()
        .filter(|name| name != "$web_search")
        .collect();
    if enabled {
        next.push("$web_search".into());
    }
    Some(next)
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
    register_websearch_command_with(api, get_state, LastServedBy::default());
}
/// `lastServedBy`: the route label of the last successful search, reset each session.
pub type LastServedBy = std::sync::Arc<std::sync::Mutex<Option<String>>>;
pub fn register_websearch_command_with(
    api: &mut maho_ext_api::ExtensionApi,
    get_state: std::sync::Arc<dyn Fn() -> ConfigLoadResult + Send + Sync>,
    last_served_by: LastServedBy,
) {
    api.register_command(
        "websearch",
        Some("Show web search provider status".into()),
        None,
        std::sync::Arc::new(move |raw, ctx| {
            let get_state = get_state.clone();
            let last_served_by = last_served_by.clone();
            Box::pin(async move {
                let state = get_state();
                let native = match &state {
                    ConfigLoadResult::Ok { config, .. } => native_model_status(
                        config,
                        ctx.model.as_ref(),
                        &ctx.model_registry.get_available(),
                        last_served_by
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .as_deref(),
                    ),
                    ConfigLoadResult::Err { .. } => NativeStatus::default(),
                };
                let (message, level) = status_message_with(raw, &state, &native);
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
    status_message_with(raw_args, state, &NativeStatus::default())
}
/// `nativeModelStatus` (senpi#2340): the model native search runs on, a warning for an ignored
/// `nativeModel`, and the route and model that served the last search.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeStatus {
    pub text: String,
    pub warning: Option<String>,
}
pub fn native_model_status(
    config: &crate::websearch::types::WebsearchConfig,
    model: Option<&maho_ext_api::Model>,
    available: &[maho_ext_api::Model],
    last_served_by: Option<&str>,
) -> NativeStatus {
    let choice = config
        .auto
        .then(|| {
            crate::websearch::search_model::resolve_native_search_model(
                model,
                available,
                config.native_model.as_deref(),
            )
        })
        .flatten();
    let mut parts = Vec::new();
    if let Some(choice) = &choice {
        let fallback = choice
            .fallback_model
            .as_ref()
            .map_or_else(String::new, |model| format!(" (falls back to {model})"));
        parts.push(format!("native model={}{fallback}", choice.model));
    }
    if let Some(served) = last_served_by {
        parts.push(format!("last search via {served}"));
    }
    let text = parts
        .iter()
        .map(|part| format!(", {part}"))
        .collect::<String>();
    if let Some(warning) = choice.as_ref().and_then(|choice| choice.warning.clone()) {
        return NativeStatus {
            text,
            warning: Some(warning),
        };
    }
    if let Some(native_model) = config.native_model.as_deref()
        && choice.is_none()
    {
        let reason = if config.auto {
            "the session model has no hosted web search route"
        } else {
            "auto is disabled"
        };
        return NativeStatus {
            text,
            warning: Some(format!(
                "nativeModel \"{native_model}\" is ignored: {reason}."
            )),
        };
    }
    NativeStatus {
        text,
        warning: None,
    }
}
pub fn status_message_with(
    raw_args: &str,
    state: &ConfigLoadResult,
    native: &NativeStatus,
) -> (String, StatusLevel) {
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
            let summary = format!(
                "Web search active: strategy={strategy}, auto={}, providers={providers}{}",
                if config.auto { "enabled" } else { "disabled" },
                native.text
            );
            match &native.warning {
                Some(warning) => (
                    format!("{summary}\nWarning: {warning}"),
                    StatusLevel::Warning,
                ),
                None => (summary, StatusLevel::Info),
            }
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
        assert!(
            api.registered
                .tools
                .iter()
                .any(|tool| tool.definition.name == "$web_search")
        );
        assert_eq!(api.registered.commands[0].name, "websearch");
        for event in [
            maho_ext_api::EventKind::SessionStart,
            maho_ext_api::EventKind::ModelSelect,
            maho_ext_api::EventKind::SessionShutdown,
        ] {
            assert_eq!(api.registered.handlers[&event].len(), 1);
        }
    }
    fn proxy_model(
        provider: &str,
        id: &str,
        api: &str,
        base: &str,
        cost: (f64, f64),
    ) -> maho_ext_api::Model {
        serde_json::from_value(serde_json::json!({"provider":provider,"id":id,"name":id,"api":api,"baseUrl":base,"reasoning":false,"input":["text"],"cost":{"input":cost.0,"output":cost.1,"cacheRead":0,"cacheWrite":0},"contextWindow":200_000,"maxTokens":16_384})).unwrap()
    }
    fn native_config(native_model: &str) -> crate::websearch::types::WebsearchConfig {
        crate::websearch::types::WebsearchConfig {
            strategy: RoutingStrategy::Priority,
            fallback: true,
            auto: true,
            native_model: Some(native_model.into()),
            providers: vec![crate::websearch::types::SearchProviderEntry {
                config: crate::websearch::types::SearchProviderConfig::new(
                    crate::websearch::provider_endpoints::SearchProvider::DuckduckgoHtml,
                ),
                priority: None,
                weight: None,
                fallback_model: None,
            }],
        }
    }
    #[test]
    fn status_warns_that_another_providers_native_model_is_ignored() {
        let opus = proxy_model(
            "claude-proxy",
            "claude-opus-4-5",
            "anthropic-messages",
            "https://claude-proxy.example.com",
            (5., 25.),
        );
        let haiku = proxy_model(
            "claude-proxy",
            "claude-haiku-4-5",
            "anthropic-messages",
            "https://claude-proxy.example.com",
            (1., 5.),
        );
        let luna = proxy_model(
            "openai-proxy",
            "gpt-5.6-luna",
            "openai-responses",
            "https://openai-proxy.example.com/v1",
            (0.2, 1.2),
        );
        let config = native_config("gpt-5.6-luna");
        let native = native_model_status(&config, Some(&opus), &[opus.clone(), haiku, luna], None);
        let state = ConfigLoadResult::Ok {
            config,
            source: "fixture".into(),
        };
        let (message, level) = status_message_with("status", &state, &native);
        assert_eq!(level, StatusLevel::Warning);
        assert!(
            message.contains("nativeModel \"gpt-5.6-luna\" is ignored"),
            "{message}"
        );
        assert!(
            message.contains("native model=claude-opus-4-5"),
            "{message}"
        );
    }
    #[test]
    fn status_names_the_planned_and_the_serving_model() {
        let opus = proxy_model(
            "claude-proxy",
            "claude-opus-4-5",
            "anthropic-messages",
            "https://claude-proxy.example.com",
            (5., 25.),
        );
        let haiku = proxy_model(
            "claude-proxy",
            "claude-haiku-4-5",
            "anthropic-messages",
            "https://claude-proxy.example.com",
            (1., 5.),
        );
        let config = native_config("claude-haiku-4-5");
        let served = crate::websearch::route_attempts::attempt_route_label(
            "anthropic",
            None,
            Some("claude-proxy/native"),
            Some("claude-haiku-4-5"),
        );
        let native =
            native_model_status(&config, Some(&opus), &[opus.clone(), haiku], Some(&served));
        let state = ConfigLoadResult::Ok {
            config,
            source: "fixture".into(),
        };
        let (message, level) = status_message_with("status", &state, &native);
        assert_eq!(level, StatusLevel::Info);
        assert!(
            message.contains("native model=claude-haiku-4-5 (falls back to claude-opus-4-5)"),
            "{message}"
        );
        assert!(
            message.contains("last search via claude-proxy/native (claude-haiku-4-5)"),
            "{message}"
        );
    }
    #[test]
    fn native_model_with_auto_disabled_is_reported_as_ignored() {
        let mut config = native_config("claude-haiku-4-5");
        config.auto = false;
        let native = native_model_status(&config, None, &[], None);
        assert_eq!(
            native.warning.as_deref(),
            Some("nativeModel \"claude-haiku-4-5\" is ignored: auto is disabled.")
        );
    }
    #[tokio::test]
    async fn moonshot_server_search_echoes_generated_arguments() {
        let tool = moonshot_server_search_tool();
        assert_eq!(tool.name, "$web_search");
        assert_eq!(
            tool.exposure,
            Some(maho_tools::definition::ToolExposure::Search)
        );
        assert_eq!(tool.allow_lazy_activation, Some(false));
        let params = serde_json::json!({"search_result":{"search_id":"fixture"}});
        let result = (tool.execute)(maho_tools::definition::ToolCall {
            id: "call",
            params: params.clone(),
            signal: Default::default(),
            on_update: None,
            context: None,
        })
        .await
        .unwrap();
        assert_eq!(result.details, Some(params.clone()));
        let expected = params.to_string();
        assert!(
            matches!(&result.content[0], maho_tools::definition::ToolContent::Text { text, .. } if text == &expected)
        );
    }
    #[test]
    fn moonshot_tool_is_toggled_only_when_its_answer_changes() {
        assert_eq!(
            moonshot_active_tools(vec!["web_search".into()], false),
            None
        );
        assert_eq!(
            moonshot_active_tools(vec!["web_search".into()], true),
            Some(vec!["web_search".into(), "$web_search".into()])
        );
        assert_eq!(
            moonshot_active_tools(vec!["$web_search".into(), "web_search".into()], true),
            None
        );
        assert_eq!(
            moonshot_active_tools(vec!["$web_search".into(), "web_search".into()], false),
            Some(vec!["web_search".into()])
        );
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
