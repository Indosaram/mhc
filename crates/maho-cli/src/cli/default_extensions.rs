//! Concrete default factories shared by print, RPC and interactive startup.
use std::sync::Arc;
use maho_ext_api::{Extension, ExtensionActions, ExtensionFailure};
use maho_ext_host::loader::{NativeExtensionFactory, NativeAsyncExtensionFactory};

fn factory<E: Extension + 'static>(name: &str, extension: E) -> NativeExtensionFactory {
    NativeExtensionFactory { path: format!("<builtin:{name}>"), source_info: Default::default(), extension: Box::new(extension) }
}

pub fn async_factories(factories: Vec<NativeExtensionFactory>) -> Vec<NativeAsyncExtensionFactory> {
    factories.into_iter().map(|factory| {
        let extension: Arc<dyn Extension> = Arc::from(factory.extension);
        NativeAsyncExtensionFactory { path: factory.path, source_info: factory.source_info,
            factory: Arc::new(move |api| { extension.register(api); Box::pin(async { Ok(()) }) }) }
    }).collect()
}

pub fn assembled_factories(widget_sender: tokio::sync::mpsc::UnboundedSender<maho_interactive::interactive_extension_ui::UiRequest>, parent: Arc<std::sync::OnceLock<Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>>>) -> Vec<NativeAsyncExtensionFactory> {
    let mut assembled = factories(widget_sender, parent.clone());
    let env = std::env::vars().collect();
    let cwd = std::env::current_dir().unwrap_or_default();
    match super::omo_mount::OmoMount::for_parent(parent, &cwd, std::path::Path::new(&maho_core::config::get_agent_dir()), env) {
        Ok(mount) => assembled.extend(async_factories(vec![mount.factory()])),
        Err(error) => assembled.push(NativeAsyncExtensionFactory {
            path: "<builtin:omo>".into(), source_info: Default::default(),
            factory: Arc::new(move |_| { let error = error.clone(); Box::pin(async move { Err(ExtensionFailure::new(error)) }) }),
        }),
    }
    let paths: std::collections::BTreeSet<_> = assembled.iter().map(|factory| factory.path.clone()).collect();
    let native = super::extension_registry::native_extension_factories().into_iter()
        .filter(|factory| !paths.contains(&factory.path)).collect();
    assembled.extend(async_factories(native));
    assembled
}

pub fn factories(widget_sender: tokio::sync::mpsc::UnboundedSender<maho_interactive::interactive_extension_ui::UiRequest>, parent: Arc<std::sync::OnceLock<Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>>>) -> Vec<NativeAsyncExtensionFactory> {
    let fallback_parent = parent.clone();
    let mcp_gate: Arc<std::sync::Mutex<Option<maho_ext_mcp::service::McpNativeToolSearchGate>>> = Arc::new(std::sync::Mutex::new(None));
    let tool_search_gate = mcp_gate.clone();
    let tool_search_service: Arc<std::sync::Mutex<Option<super::tool_search::SharedToolSearchService>>> = Arc::new(std::sync::Mutex::new(None));
    let tool_search_slot = tool_search_service.clone();
    vec![
            factory("recommended-models", maho_ext_recommended_models::RecommendedModels),
            factory("permission-system", maho_ext_permission_system::PermissionSystem),
            factory("gpt-apply-patch", maho_ext_gpt_apply_patch::index::ApplyPatchExtension),
            factory("tool-search", ToolSearch { mcp_native_enabled: Arc::new(move || tool_search_gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().is_some_and(|gate| gate.enabled())), service: tool_search_slot }),
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
                        let prepared = super::super::utils::image_process::process_image(&bytes, &mime,
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
                fallback_parent.get().and_then(|parent| parent()).is_some_and(|parent| parent.model_registry().is_using_oauth(model))
            }) }),
            factory("compaction", maho_ext_compaction::CompactionExtension),
            factory("webfetch", maho_ext_webfetch::index::WebfetchExtension),
            factory("rules", maho_ext_rules::Rules),
            factory("goal", maho_ext_goal::GoalExtension::default()),
            factory("codemode", Codemode),
            factory("mcp", Mcp { gate: mcp_gate.clone(), tool_search: tool_search_service.clone() }),
        ].into_iter().map(|factory| {
            let extension: Arc<dyn Extension> = Arc::from(factory.extension);
            NativeAsyncExtensionFactory { path: factory.path, source_info: factory.source_info,
                factory: Arc::new(move |api| { extension.register(api); Box::pin(async { Ok(()) }) }) }
        }).collect()
}

struct RuntimeActions(maho_ext_api::ExtensionRuntime);
pub type TaskParent = Arc<std::sync::OnceLock<Arc<dyn Fn() -> Option<maho_core::agent_session::AgentSession> + Send + Sync>>>;
struct Task {
    parent: TaskParent,
    runtime: Arc<maho_omo::OmoRuntime>,
    engine: Arc<std::sync::Mutex<Option<maho_omo_task::engine::TaskEngine>>>,
    coordinator: Arc<maho_omo::task_coordinator::TaskCoordinator>,
    environment: std::collections::BTreeMap<String, String>,
}

pub fn task_entry(parent: TaskParent, environment: std::collections::BTreeMap<String, String>) -> maho_omo::OmoSenpiComponent {
    let engine = Arc::new(std::sync::Mutex::new(None));
    let coordinator = Arc::new(std::sync::OnceLock::new());
    maho_omo::OmoSenpiComponent::from_context_register("task", move |api, runtime| {
        let coordinator = coordinator.get_or_init(|| Arc::new(maho_omo::task_coordinator::TaskCoordinator(runtime.context().idle_coordinator.clone()))).clone();
        Task { parent: parent.clone(), runtime: Arc::new(runtime.clone()), engine: engine.clone(), coordinator, environment: environment.clone() }.register(api);
    })
}
struct Registry(Arc<dyn maho_ext_api::ModelRegistry>);
impl senpi_task::host::SenpiModelRegistry for Registry {
    fn get_available(&self) -> Result<serde_json::Value, senpi_task::host::HostError> {
        serde_json::to_value(self.0.get_available()).map_err(|error| senpi_task::host::HostError { message: error.to_string() })
    }
    fn find(&self, provider: &str, model: &str) -> Option<serde_json::Value> { self.0.find(provider, model).map(|model| serde_json::to_value(model).expect("model serialization")) }
}
impl Extension for Task {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        let parent = self.parent.clone(); let registered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let shared = self.runtime.clone(); let engine_slot = self.engine.clone();
        let coordinator = self.coordinator.clone(); let environment = self.environment.clone();
        let actions = Arc::new(TaskActions(parent.clone()));
        let registration = Arc::new(std::sync::Mutex::new(maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new(&api.registered.identity.path, api.registered.registration_cwd.clone(), api.registered.source_info.clone()),
            api.profile.clone(), api.events.clone(), api.runtime.clone())));
        api.on(maho_ext_api::EventKind::SessionStart, Arc::new(move |event, ctx| {
            let parent = parent.clone(); let registered = registered.clone(); let registration = registration.clone();
            let actions = actions.clone();
            let shared = shared.clone(); let engine_slot = engine_slot.clone();
            let coordinator = coordinator.clone(); let environment = environment.clone();
            Box::pin(async move {
                let _turn = shared.enter_turn();
                let mut bound_context = ctx.clone(); shared.bind(&mut bound_context);
                let ctx = &bound_context;
                let Some(parent) = parent.get().and_then(|parent| parent()) else { return Ok(maho_ext_api::EventResult::None); };
                if registered.load(std::sync::atomic::Ordering::Acquire) { return Ok(maho_ext_api::EventResult::None); }
                let executor = tokio::runtime::Handle::current(); let weak = parent.weak_accessor();
                let parent_registry = super::task_runners::live_parent_registry(weak.clone());
                let process = super::task_runners::authenticated_rpc_options(super::task_runners::native_rpc_options(
                    std::env::current_exe().map_err(|error| ExtensionFailure::new(error.to_string()))?,
                    &maho_core::config::get_agent_dir(), environment, Vec::new()), weak.clone());
                let runners = maho_omo_task::engine_runners::build_task_runners(maho_omo_task::engine_runners::TaskRunnerBuildOptions {
                    shared_parent_tools: Vec::new(), get_shared_parent_tools: Some(super::task_runners::live_parent_tools(weak, executor.clone())), max_depth: 3,
                    create_session: super::task_session::factory(executor, parent.weak_accessor()), parent_registry, rpc_options: process.clone(),
                });
                let registry: Arc<dyn senpi_task::host::SenpiModelRegistry> = Arc::new(Registry(ctx.model_registry.clone()));
                let config = parent.with_settings_manager(|settings| settings.get_value("omo").cloned().unwrap_or_else(|| serde_json::json!({})));
                let engine = {
                    let mut slot = engine_slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let engine = slot.get_or_insert_with(|| maho_omo_task::engine::compose_task_engine_with_rpc_respawn(maho_omo_task::engine::ComposeTaskEngineDeps {
                        cwd: ctx.cwd.clone(), config, runners, actions: actions.clone(), coordinator: Some(coordinator.clone()),
                        resolve_registry: Arc::new(move || Some(registry.clone())),
                    }, Some(maho_omo_task::engine_runners::build_rpc_respawn_runner(process))));
                    super::omo_mount::retained_engine(engine)
                };
                let handlers = {
                    let mut api = registration.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let ownership = senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps {
                        state_dir: senpi_task::store::StateDirConfig { project_dir: ctx.cwd.clone(), task_state_dir: Some(engine.store.state_dir().into()) },
                        team_bounds: super::task_session::configured_team_bounds(&engine.config)?, load_runtime_state: None,
                    };
                    let manager = engine.manager.clone();
                    let spawn = senpi_task::tools::task::execute_spec::TaskToolDeps {
                        resolve_ancestry: Some(Arc::new(move |session| manager.list(&senpi_task::manager::types::ListScope::All).into_iter()
                            .find(|entry| entry.record.child_session_id.as_deref() == Some(session))
                            .map(|entry| senpi_task::tools::task::execute_spec::TaskAncestry { root_session_id: entry.record.root_session_id, depth: u64::from(entry.record.depth) }))),
                        load_skills: Some(senpi_task::tools::task::skills::create_fs_skill_loader(senpi_task::tools::task::skills::FsSkillLoaderOptions {
                            home_dir: Some(maho_core::config::home_dir().into()), agent_dir: Some(maho_core::config::get_agent_dir().into()), ..Default::default()
                        })),
                        agents: engine.agents.iter().map(|(name, agent)| (name.clone(), senpi_task::tools::task::execute_spec::TaskAgentDefinition { execution_mode: agent.execution_mode.clone() })).collect(),
                        omo_config: senpi_task::tools::task::execute_spec::TaskOmoConfig {
                            agents: engine.config.get("agents").and_then(serde_json::Value::as_object).map(|agents| agents.iter().map(|(name, config)| {
                                (name.clone(), senpi_task::tools::task::execute_spec::TaskOmoAgentConfig {
                                    execution_mode: config.get("execution_mode").and_then(serde_json::Value::as_str).and_then(senpi_task::manager::execution_mode::ExecutionMode::parse),
                                })
                            }).collect()).unwrap_or_default(),
                            task: Some(senpi_task::tools::task::execute_spec::TaskOmoTaskConfig {
                                default_execution_mode: engine.config["task"]["default_execution_mode"].as_str().and_then(senpi_task::manager::execution_mode::ExecutionMode::parse),
                            }),
                        },
                    };
                    let team_ownership = senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps {
                        state_dir: ownership.state_dir.clone(), team_bounds: ownership.team_bounds, load_runtime_state: None,
                    };
                    if let Some(component) = maho_omo_task::component::TaskComponent::register(&mut api, engine, spawn, ownership,
                        senpi_task::team::member_extension::identity::is_team_member_process_from_env())? {
                        super::task_session::mount_team_runtime(&mut api, &component, team_ownership, actions.clone(), coordinator.clone())?;
                    }
                    let handlers = std::mem::take(&mut api.registered.handlers);
                    for (kind, handlers) in handlers {
                        for original in handlers {
                            let runtime = shared.clone();
                            api.on(kind, Arc::new(move |event, context| {
                                let original = original.clone(); let runtime = runtime.clone();
                                let mut context = context.clone(); runtime.bind(&mut context);
                                Box::pin(async move { let _turn = runtime.enter_turn(); original(event, &context).await })
                            }));
                        }
                    }
                    api.registered.handlers.get(&maho_ext_api::EventKind::SessionStart).cloned().unwrap_or_default()
                };
                registered.store(true, std::sync::atomic::Ordering::Release);
                let mut captured = shared.captured_tools();
                captured.extend(registration.lock().unwrap_or_else(std::sync::PoisonError::into_inner).registered.tools.iter().map(|tool| tool.definition.clone()));
                shared.capture_tools(captured);
                for handler in handlers { handler(event, ctx).await?; }
                Ok(maho_ext_api::EventResult::None)
            })
        }));
    }
}
pub(super) struct TaskActions(pub(super) TaskParent);
impl TaskActions {
    fn actions(&self) -> Result<Arc<dyn ExtensionActions>, ExtensionFailure> {
        self.0.get().and_then(|parent| parent()).map(|parent| parent.extension_actions())
            .ok_or_else(|| ExtensionFailure::new("Task parent retired"))
    }
}
impl ExtensionActions for TaskActions {
    fn send_message(&self, message: maho_ext_api::CustomMessage, options: maho_ext_api::SendMessageOptions) -> Result<(), ExtensionFailure> { self.actions()?.send_message(message, options) }
    fn send_user_message(&self, content: maho_ext_api::UserMessageContent, options: maho_ext_api::SendUserMessageOptions) -> Result<(), ExtensionFailure> { self.actions()?.send_user_message(content, options) }
    fn append_entry(&self, kind: &str, data: Option<maho_ext_api::JsonValue>) -> Result<(), ExtensionFailure> { self.actions()?.append_entry(kind, data) }
    fn get_all_tools(&self) -> Result<Vec<maho_ext_api::ToolInfo>, ExtensionFailure> { self.actions()?.get_all_tools() }
}
struct Codemode;
struct Images;
impl maho_codemode::tool::image_resize::EvalImageSdk for Images {
    fn resize_image<'a>(&'a self, bytes: Vec<u8>, mime: &'a str, max_bytes: Option<usize>) -> maho_codemode::tool::image_resize::ImageFuture<'a, Option<maho_codemode::tool::image_resize::ResizedImage>> {
        Box::pin(async move {
            let mut resize = super::super::utils::image_resize_core::ImageResizeOptions::default();
            if let Some(max_bytes) = max_bytes { resize.max_bytes = max_bytes as f64; }
            let resized = super::super::utils::image_process::process_image(&bytes, mime,
                super::super::utils::image_process::ProcessImageOptions { auto_resize_images: Some(true), resize_options: Some(resize) }).await.map_err(|error| error)?;
            Ok(Some(maho_codemode::tool::image_resize::ResizedImage { data: resized.data, mime_type: resized.mime_type, dimension_note: resized.hints.join("\n") }))
        })
    }
    fn convert_to_png<'a>(&'a self, data: &'a str, _mime_type: &'a str) -> maho_codemode::tool::image_resize::ImageFuture<'a, Option<maho_codemode::tool::image_resize::EvalImageContent>> {
        Box::pin(async move {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD.decode(data).map_err(|error| error.to_string())?;
            Ok(super::super::utils::image_convert::convert_image_bytes_to_png(&bytes).map(|png| maho_codemode::tool::image_resize::EvalImageContent { data: base64::engine::general_purpose::STANDARD.encode(png), mime_type: "image/png".into() }))
        })
    }
}
impl Extension for Codemode {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        if let Err(error) = maho_codemode::register(api, maho_codemode::CodemodeExtensionOptions {
            image_sdk: Arc::new(Images), complete: Arc::new(|request, context| Box::pin(super::codemode_services::complete(request, context))),
            home_dir: maho_core::config::home_dir().into(), environment: std::env::vars().collect(),
            js_runtime: maho_codemode::tool::types::EvalRuntimeInfo { name: "Bun".into(), version: "1.4".into(), path: Some("bun".into()) },
        }) {
            std::panic::panic_any(error);
        }
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
struct ToolSearch { mcp_native_enabled: Arc<dyn Fn() -> bool + Send + Sync>, service: Arc<std::sync::Mutex<Option<super::tool_search::SharedToolSearchService>>> }
impl Extension for ToolSearch {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        // Create the ONE shared tool-search service and publish it for the MCP factory, so the
        // tool_search tool, the MCP catalog feed, the native gate and the adapter all read it.
        let service = Arc::new(tokio::sync::Mutex::new(maho_ext_tool_search::service::ToolSearchService::new(api.runtime.clone(), Arc::new(RuntimeActions(api.runtime.clone())))));
        maho_ext_tool_search::index::ToolSearchExtension { actions: Arc::new(RuntimeActions(api.runtime.clone())),
            mcp_native_enabled: self.mcp_native_enabled.clone() }.register_with_service(api, service.clone());
        *self.service.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(service);
    }
}
struct Mcp { gate: Arc<std::sync::Mutex<Option<maho_ext_mcp::service::McpNativeToolSearchGate>>>, tool_search: Arc<std::sync::Mutex<Option<super::tool_search::SharedToolSearchService>>> }
impl Extension for Mcp {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        let tool_search = self.tool_search.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let extension = maho_ext_mcp::index::McpExtension {
            registry: Arc::new(maho_ext_mcp::host_registry::HostMcpRegistry::default()), owner: 1, tool_search,
        };
        let service = extension.register_with_service(api);
        if let Ok(service) = service.try_lock() {
            *self.gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(service.native_tool_search_gate());
        }
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
