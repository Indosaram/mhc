//! Concrete default factories shared by print, RPC and interactive startup.
use std::sync::Arc;
use maho_ext_api::{Extension, ExtensionActions, ExtensionFailure};
use maho_ext_host::loader::{NativeExtensionFactory, NativeAsyncExtensionFactory};

fn factory<E: Extension + 'static>(name: &str, extension: E) -> NativeExtensionFactory {
    NativeExtensionFactory { path: format!("<builtin:{name}>"), source_info: Default::default(), extension: Box::new(extension) }
}

pub fn factories(widget_sender: tokio::sync::mpsc::UnboundedSender<maho_interactive::interactive_extension_ui::UiRequest>, parent: Arc<std::sync::OnceLock<std::sync::Weak<maho_core::agent_session::AgentSession>>>) -> Vec<NativeAsyncExtensionFactory> {
    let fallback_parent = parent.clone();
    vec![
            factory("recommended-models", maho_ext_recommended_models::RecommendedModels),
            factory("permission-system", maho_ext_permission_system::PermissionSystem),
            factory("patch", maho_ext_gpt_apply_patch::index::ApplyPatchExtension),
            factory("tool-search", ToolSearch),
            factory("todotools", Todo(widget_sender)),
            factory("websearch", maho_ext_websearch::index::WebsearchExtension { home: maho_core::config::home_dir().into(),
                provider_native_bypass: Arc::new(|model| {
                    maho_ext_anthropic_web_search::supports_native_anthropic_web_search(model)
                        || maho_ext_openai_web_search::supports_native_openai_web_search(model)
                            && maho_ext_openai_web_search::parse_enabled(std::env::var("PI_OPENAI_WEB_SEARCH").ok().as_deref())
                }),
            }),
            factory("look-at", maho_ext_look_at::index::LookAtExtension {
                runner: maho_ext_look_at::runner::create_vision_runner(Arc::new(|ctx, model, context, options| ctx.model_registry.stream_simple(model, context, Some(options))),
                    Some(Arc::new(|bytes, mime, options| Box::pin(async move {
                        use base64::Engine;
                        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
                        let prepared = super::super::utils::image_process::process_image(&encoded, &mime,
                            super::super::utils::image_process::ProcessImageOptions { auto_resize_images: Some(options.auto_resize_images), ..Default::default() }).await.map_err(ExtensionFailure::new)?;
                        Ok(maho_ext_look_at::image_input::ProcessedImage { data: prepared.data, mime_type: prepared.mime_type, hints: prepared.hints })
                    })))),
            }),
            factory("account", maho_ext_account::Account),
            factory("ask-user", maho_ext_ask_user::AskUser),
            factory("video-in", maho_ext_video_in::VideoIn),
            factory("anthropic-web-search", maho_ext_anthropic_web_search::AnthropicWebSearch),
            factory("openai-web-search", maho_ext_openai_web_search::OpenAiWebSearch),
            factory("imagegen", maho_ext_imagegen::ImageGen::default()),
            factory("openai-image-gen", maho_ext_openai_image_gen::OpenAiImageGen),
            factory("herdr", maho_ext_herdr::Herdr),
            factory("btw", maho_ext_btw::Btw::default()),
            factory("cache-keepalive", maho_ext_cache_keepalive::CacheKeepalive::default()),
            factory("help", maho_ext_help::Help { display: Arc::new(|ctx, markdown| Box::pin(async move {
                ctx.ui.select(&markdown, &["Close".into()], Default::default()).await; Ok(())
            })) }),
            factory("history-search", maho_ext_history_search::HistorySearch {
                session_dir: Arc::new(|ctx| ctx.session_manager.get_session_dir().ok_or_else(|| ExtensionFailure::new("History search requires a session directory"))),
                default_sessions_root: Arc::new(|| std::path::PathBuf::from(maho_core::config::get_agent_dir()).join("sessions")),
                select: Arc::new(|ctx, entries| Box::pin(async move {
                    let choices = entries.iter().enumerate().map(|(index, entry)| format!("{}: {}", index + 1, entry.text)).collect::<Vec<_>>();
                    let selected = ctx.ui.select("Prompt history", &choices, Default::default()).await;
                    Ok(selected.and_then(|selected| choices.iter().position(|choice| *choice == selected)).map(|index| entries[index].clone()))
                })),
            }),
            factory("model-fallback", maho_ext_model_fallback::ModelFallback { is_using_oauth: Arc::new(move |model| {
                fallback_parent.get().and_then(std::sync::Weak::upgrade).is_some_and(|parent| parent.model_registry().is_using_oauth(model))
            }) }),
            factory("compaction", maho_ext_compaction::CompactionExtension),
            factory("webfetch", maho_ext_webfetch::index::WebfetchExtension),
            factory("codemode", Codemode),
            factory("task", Task(parent)),
            factory("mcp", Mcp),
        ].into_iter().map(|factory| {
            let extension: Arc<dyn Extension> = Arc::from(factory.extension);
            NativeAsyncExtensionFactory { path: factory.path, source_info: factory.source_info,
                factory: Arc::new(move |api| { extension.register(api); Box::pin(async { Ok(()) }) }) }
        }).collect()
}

struct RuntimeActions(maho_ext_api::ExtensionRuntime);
struct Task(Arc<std::sync::OnceLock<std::sync::Weak<maho_core::agent_session::AgentSession>>>);
struct Registry(Arc<dyn maho_ext_api::ModelRegistry>);
impl senpi_task::host::SenpiModelRegistry for Registry {
    fn get_available(&self) -> Result<serde_json::Value, senpi_task::host::HostError> {
        serde_json::to_value(self.0.get_available()).map_err(|error| senpi_task::host::HostError { message: error.to_string() })
    }
    fn find(&self, provider: &str, model: &str) -> Option<serde_json::Value> { self.0.find(provider, model).map(|model| serde_json::to_value(model).expect("model serialization")) }
}
impl Extension for Task {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        let parent = self.0.clone(); let registered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let actions = Arc::new(RuntimeActions(api.runtime.clone()));
        let registration = Arc::new(std::sync::Mutex::new(maho_ext_api::ExtensionApi::new(api.registered.clone(), api.profile.clone(), api.events.clone(), api.runtime.clone())));
        api.on(maho_ext_api::EventKind::SessionStart, Arc::new(move |event, ctx| {
            let parent = parent.clone(); let registered = registered.clone(); let registration = registration.clone();
            let actions = actions.clone();
            Box::pin(async move {
                let Some(parent) = parent.get().and_then(std::sync::Weak::upgrade) else { return Ok(maho_ext_api::EventResult::None); };
                if registered.load(std::sync::atomic::Ordering::Acquire) { return Ok(maho_ext_api::EventResult::None); }
                let executor = tokio::runtime::Handle::current(); let weak = Arc::downgrade(&parent);
                let parent_registry = super::task_runners::parent_registry_scope(weak.clone());
                let process = super::task_runners::authenticated_rpc_options(super::task_runners::native_rpc_options(
                    std::env::current_exe().map_err(|error| ExtensionFailure::new(error.to_string()))?,
                    &maho_core::config::get_agent_dir(), std::env::vars().collect(), Vec::new()), weak.clone());
                let runners = maho_omo_task::engine_runners::build_task_runners(maho_omo_task::engine_runners::TaskRunnerBuildOptions {
                    shared_parent_tools: Vec::new(), get_shared_parent_tools: Some(super::task_runners::live_parent_tools(weak, executor.clone())), max_depth: 3,
                    create_session: super::task_session::factory(executor, Arc::downgrade(&parent)), parent_registry, rpc_options: process.clone(),
                });
                let registry: Arc<dyn senpi_task::host::SenpiModelRegistry> = Arc::new(Registry(ctx.model_registry.clone()));
                let config = parent.with_settings_manager(|settings| settings.get_value("omo").cloned().unwrap_or_else(|| serde_json::json!({})));
                let engine = maho_omo_task::engine::compose_task_engine_with_rpc_respawn(maho_omo_task::engine::ComposeTaskEngineDeps {
                    cwd: ctx.cwd.clone(), config, runners, actions, coordinator: None,
                    resolve_registry: Arc::new(move || Some(registry.clone())),
                }, Some(maho_omo_task::engine_runners::build_rpc_respawn_runner(process)));
                let handlers = {
                    let mut api = registration.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let ownership = senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps {
                        state_dir: senpi_task::store::StateDirConfig { project_dir: ctx.cwd.clone(), task_state_dir: Some(engine.store.state_dir().into()) },
                        team_bounds: senpi_task::team::runtime_config::TeamTaskBounds { max_members: 8, max_parallel_members: 4, max_wall_clock_minutes: 60 }, load_runtime_state: None,
                    };
                    maho_omo_task::component::TaskComponent::register(&mut api, engine, Default::default(), ownership,
                        std::env::var("OMO_TEAM_MEMBER").is_ok())?;
                    api.registered.handlers.get(&maho_ext_api::EventKind::SessionStart).cloned().unwrap_or_default()
                };
                registered.store(true, std::sync::atomic::Ordering::Release);
                for handler in handlers { handler(event, ctx).await?; }
                Ok(maho_ext_api::EventResult::None)
            })
        }));
    }
}
struct Codemode;
struct Images;
impl maho_codemode::tool::image_resize::EvalImageSdk for Images {
    fn resize_image<'a>(&'a self, base64: &'a str, mime: &'a str) -> maho_codemode::tool::image_resize::ImageFuture<'a, maho_codemode::tool::image_resize::ResizedImage> {
        Box::pin(async move {
            let resized = super::super::utils::image_process::process_image(base64, mime, Default::default()).await?;
            Ok(maho_codemode::tool::image_resize::ResizedImage { data: resized.data, mime_type: resized.mime_type, dimension_note: resized.hints.join("\n") })
        })
    }
}
impl Extension for Codemode {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        maho_codemode::register(api, maho_codemode::CodemodeExtensionOptions {
            image_sdk: Arc::new(Images), complete: Arc::new(|request, context| Box::pin(super::codemode_services::complete(request, context))),
            home_dir: maho_core::config::home_dir().into(), environment: std::env::vars().collect(),
            js_runtime: maho_codemode::tool::types::EvalRuntimeInfo { name: "Bun".into(), version: "1.4".into(), path: Some("bun".into()) },
        });
    }
}
impl RuntimeActions {
    fn actions(&self) -> Result<Arc<dyn ExtensionActions>, ExtensionFailure> {
        self.0.message_actions()
    }
}
impl ExtensionActions for RuntimeActions {
    fn send_message(&self, message: maho_ext_api::CustomMessage, options: maho_ext_api::SendMessageOptions) -> Result<(), ExtensionFailure> { self.actions()?.send_message(message, options) }
    fn send_user_message(&self, content: maho_ext_api::UserMessageContent, options: maho_ext_api::SendUserMessageOptions) -> Result<(), ExtensionFailure> { self.actions()?.send_user_message(content, options) }
    fn append_entry(&self, kind: &str, data: Option<maho_ext_api::JsonValue>) -> Result<(), ExtensionFailure> { self.actions()?.append_entry(kind, data) }
    fn get_all_tools(&self) -> Result<Vec<maho_ext_api::ToolInfo>, ExtensionFailure> { self.actions()?.get_all_tools() }
}
struct ToolSearch;
struct Mcp;
impl Extension for Mcp {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        maho_ext_mcp::index::register_mcp_lifecycle(api, Arc::new(maho_ext_mcp::host_registry::HostMcpRegistry::default()), 1);
    }
}
impl Extension for ToolSearch {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        maho_ext_tool_search::index::ToolSearchExtension { actions: Arc::new(RuntimeActions(api.runtime.clone())),
            mcp_native_enabled: Arc::new(|| std::env::var("MAHO_MCP_NATIVE_TOOL_SEARCH").is_ok_and(|value| value == "1")) }.register(api);
    }
}
struct Todo(tokio::sync::mpsc::UnboundedSender<maho_interactive::interactive_extension_ui::UiRequest>);
impl Extension for Todo {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        maho_ext_todotools::index::NativeTodotoolsExtension {
            actions: Arc::new(RuntimeActions(api.runtime.clone())), widget_sender: self.0.clone(),
            copy_markdown: Arc::new(|text| Box::pin(async move {
                super::super::utils::clipboard::copy_to_clipboard(&text).await.map_err(ExtensionFailure::new)
            })),
        }.register(api);
    }
}
