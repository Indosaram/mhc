//! Port of `packages/coding-agent/src/extensions/llama/index.ts` (pin `fe8c564b`).

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use maho_ai::models::ModelsRefreshOptions;
use maho_ai::utils::abort::{AbortController, AbortSignal};
use maho_ext_api::{
    Extension, ExtensionApi, ExtensionCommandContext, ExtensionMode,
    ExtensionTuiHost, ExtensionUi, ModelRegistry, NotificationType,
};

use crate::client::{LlamaClient, LlamaModelInfo, format_bytes, normalize_llama_server_url};
use crate::huggingface::{HuggingFaceClient, HuggingFaceModel, find_hugging_face_token};
use crate::provider::{LLAMA_PROVIDER_ID, llama_provider_config};
use crate::ui::{
    LlamaManagerAction, LlamaUi, RenderBinding, RunProgressOptions, SearchFn, ShowLlamaOptions,
    UiFuture, model_is_loaded, run_with_progress, show_llama_ui,
};

fn is_connection_error(message: &str) -> bool {
    let message = message.to_lowercase();
    message.contains("fetch failed") || message.contains("timeout") || message.contains("network")
}

fn connection_error_message(error: &str) -> String {
    if is_connection_error(error) { "Could not connect to the server.".to_owned() } else { error.to_owned() }
}

/// Pinned `parseHuggingFaceModel`: `owner/repository[:quant]`.
fn parse_hugging_face_model(value: &str) -> (String, Option<String>) {
    let after_owner = value.find('/').map_or(0, |index| index + 1);
    match value[after_owner..].find(':') {
        Some(offset) => {
            let colon = after_owner + offset;
            (value[..colon].to_owned(), Some(value[colon + 1..].to_owned()))
        }
        None => (value.to_owned(), None),
    }
}

/// `AbortSignal.timeout(ms)`: a signal that aborts itself after the deadline.
fn timeout_signal(ms: u64) -> AbortSignal {
    let controller = AbortController::new();
    let signal = controller.signal();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        controller.abort(None);
    });
    signal
}

/// Pinned `configuredClient`: resolve the server URL and key from the registered provider auth.
async fn configured_client(ctx: &ExtensionCommandContext) -> Option<LlamaClient> {
    let result = ctx.model_registry.get_provider_auth(LLAMA_PROVIDER_ID).await.ok().flatten();
    let Some(result) = result else {
        ctx.ui.notify(&format!("Configure llama.cpp with /login {LLAMA_PROVIDER_ID}"), NotificationType::Warning);
        return None;
    };
    let configured_url = result
        .env
        .as_ref()
        .and_then(|env| env.get("LLAMA_BASE_URL"))
        .filter(|value| !value.is_empty())
        .cloned();
    let server_url = configured_url
        .unwrap_or_else(|| result.auth.base_url.clone().unwrap_or_default());
    let server_url = normalize_llama_server_url(&server_url).unwrap_or_else(|_| server_url);
    LlamaClient::new(&server_url, result.auth.api_key.as_deref()).ok()
}

/// Pinned `syncCatalog`: list (or reuse) the catalog, then refresh the provider catalog.
async fn sync_catalog(
    registry: &Arc<dyn ModelRegistry>,
    client: &LlamaClient,
    catalog: Option<Vec<LlamaModelInfo>>,
) -> Result<Vec<LlamaModelInfo>, String> {
    let signal = timeout_signal(15_000);
    let current = match catalog {
        Some(catalog) => catalog,
        None => client.list(false, Some(&signal)).await?,
    };
    let result = registry
        .refresh(ModelsRefreshOptions {
            allow_network: Some(true),
            providers: Some(vec![LLAMA_PROVIDER_ID.to_owned()]),
            force: None,
            signal: Some(signal),
        })
        .await
        .map_err(|error| error.message)?;
    if result.aborted {
        return Err("Model catalog refresh timed out.".to_owned());
    }
    if let Some(error) = result.errors.get(LLAMA_PROVIDER_ID) {
        return Err(error.to_string());
    }
    Ok(current)
}

/// Pinned `readCatalog`: retry the catalog read until the user closes the connection prompt.
async fn read_catalog(
    registry: &Arc<dyn ModelRegistry>,
    client: &LlamaClient,
    ui: &Arc<dyn LlamaUi>,
) -> Option<Vec<LlamaModelInfo>> {
    loop {
        match sync_catalog(registry, client, None).await {
            Ok(catalog) => return Some(catalog),
            Err(error) => {
                if !ui.connection_error(client.server_url.clone(), connection_error_message(&error)).await {
                    return None;
                }
            }
        }
    }
}

/// Pinned `restoreLoaded`: reload every previously loaded model and refresh the catalog.
async fn restore_loaded(
    notify: &Arc<dyn ExtensionUi>,
    registry: &Arc<dyn ModelRegistry>,
    client: &LlamaClient,
    loaded: &[LlamaModelInfo],
) -> Result<(), String> {
    notify.notify("Restoring previously loaded models", NotificationType::Info);
    for model in loaded {
        client.load_and_wait(&model.id, &mut |_| {}, None).await?;
    }
    sync_catalog(registry, client, None).await?;
    Ok(())
}

async fn load_model(
    notify: &Arc<dyn ExtensionUi>,
    registry: &Arc<dyn ModelRegistry>,
    client: &LlamaClient,
    ui: Arc<dyn LlamaUi>,
    catalog: Vec<LlamaModelInfo>,
    target: LlamaModelInfo,
) -> Result<(), String> {
    let loaded: Vec<LlamaModelInfo> = catalog
        .into_iter()
        .filter(|model| model.id != target.id && model_is_loaded(model))
        .collect();
    let mut replace = false;
    if !loaded.is_empty() {
        let choice = ui
            .select(
                format!("{} model{} loaded", loaded.len(), if loaded.len() == 1 { " is" } else { "s are" }),
                vec!["Unload all and load".to_owned(), "Keep loaded and load".to_owned(), "Cancel".to_owned()],
            )
            .await;
        match choice.as_deref() {
            None | Some("Cancel") => return Ok(()),
            Some(choice) => replace = choice == "Unload all and load",
        }
    }
    if replace {
        for model in &loaded {
            client.unload_and_wait(&model.id, None).await?;
        }
    }
    let run_client = client.clone();
    let cancel_client = client.clone();
    let run_target = target.id.clone();
    let cancel_target = target.id.clone();
    let outcome = run_with_progress(
        ui.clone(),
        RunProgressOptions::<LlamaModelInfo> {
            title: "Loading model".to_owned(),
            model: target.id.clone(),
            initial_message: "Starting…".to_owned(),
            cancel_title: "Stop loading?".to_owned(),
            cancel_message: target.id.clone(),
            run: Box::new(move |signal, mut update| {
                let client = run_client.clone();
                let model = run_target.clone();
                Box::pin(async move { client.load_and_wait(&model, &mut |progress| update(progress), Some(&signal)).await })
            }),
            cancel: Box::new(move || {
                let client = cancel_client.clone();
                let model = cancel_target.clone();
                Box::pin(async move { let _ = client.unload(&model, None).await; })
            }),
        },
    )
    .await;
    match outcome {
        Err(error) => {
            if replace {
                let _ = restore_loaded(notify, registry, client, &loaded).await;
            }
            Err(error)
        }
        Ok(None) => {
            if replace {
                restore_loaded(notify, registry, client, &loaded).await?;
            }
            Ok(())
        }
        Ok(Some(_)) => {
            let refreshed = sync_catalog(registry, client, None).await?;
            let loaded_model = refreshed.iter().find(|model| model.id == target.id);
            notify.notify(
                &if loaded_model.is_some_and(|model| model.status.value == "loaded") {
                    format!("Loaded {}", target.id)
                } else {
                    format!("Load started for {}", target.id)
                },
                NotificationType::Info,
            );
            Ok(())
        }
    }
}

async fn unload_model(
    notify: &Arc<dyn ExtensionUi>,
    registry: &Arc<dyn ModelRegistry>,
    client: &LlamaClient,
    ui: Arc<dyn LlamaUi>,
    model: LlamaModelInfo,
) -> Result<(), String> {
    if !ui.confirm("Unload model?".to_owned(), model.id.clone()).await {
        return Ok(());
    }
    client.unload_and_wait(&model.id, None).await?;
    sync_catalog(registry, client, None).await?;
    notify.notify(&format!("Unloaded {}", model.id), NotificationType::Info);
    Ok(())
}

async fn download_model(
    notify: &Arc<dyn ExtensionUi>,
    registry: &Arc<dyn ModelRegistry>,
    client: &LlamaClient,
    ui: Arc<dyn LlamaUi>,
) -> Result<(), String> {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let hugging_face = Arc::new(HuggingFaceClient::new(find_hugging_face_token(&env), None));
    let search: SearchFn = {
        let hugging_face = hugging_face.clone();
        Arc::new(move |query: String, signal: AbortSignal| -> UiFuture<'static, Result<Vec<HuggingFaceModel>, String>> {
            let hugging_face = hugging_face.clone();
            Box::pin(async move { hugging_face.search(&query, Some(&signal)).await })
        })
    };
    let Some(selected) = ui.search_models(search).await else {
        return Ok(());
    };
    let (repository, mut quantization) = parse_hugging_face_model(&selected);
    ui.show_status("Loading model details".to_owned(), repository.clone());
    let details = hugging_face.details(&repository, None).await?;
    if details.gated.is_some() {
        let approval = if details.gated.as_deref() == Some("manual") { "Manual approval is required" } else { "Accept the access terms" };
        let choice = ui
            .select(
                format!(
                    "Hugging Face access required\n{}\n\n{} at:\nhttps://huggingface.co/{}\n\nThe llama.cpp server needs HF_TOKEN with access.",
                    details.id, approval, details.id
                ),
                vec!["Continue".to_owned(), "Back".to_owned()],
            )
            .await;
        if choice.as_deref() != Some("Continue") {
            return Ok(());
        }
    }
    if quantization.is_none() && !details.quantizations.is_empty() {
        let options: Vec<String> = details
            .quantizations
            .iter()
            .map(|entry| {
                let detail = [
                    entry.size.map(|size| format_bytes(size as f64)),
                    (entry.name == "Q4_K_M").then(|| "recommended".to_owned()),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
                if detail.is_empty() { entry.name.clone() } else { format!("{} · {}", entry.name, detail) }
            })
            .collect();
        let Some(choice) = ui.select(format!("Select quantization\n{}", details.id), options.clone()).await else {
            return Ok(());
        };
        quantization = options
            .iter()
            .position(|option| *option == choice)
            .and_then(|index| details.quantizations.get(index))
            .map(|entry| entry.name.clone());
        if quantization.is_none() {
            return Ok(());
        }
    }
    let model = match &quantization {
        Some(quantization) => format!("{}:{}", details.id, quantization),
        None => details.id.clone(),
    };
    let run_client = client.clone();
    let cancel_client = client.clone();
    let run_model = model.clone();
    let cancel_model = model.clone();
    let outcome = run_with_progress(
        ui.clone(),
        RunProgressOptions::<Vec<LlamaModelInfo>> {
            title: "Downloading model".to_owned(),
            model: model.clone(),
            initial_message: "Starting…".to_owned(),
            cancel_title: "Stop download?".to_owned(),
            cancel_message: model.clone(),
            run: Box::new(move |signal, mut update| {
                let client = run_client.clone();
                let model = run_model.clone();
                Box::pin(async move { client.download_and_wait(&model, &mut |progress| update(progress), Some(&signal)).await })
            }),
            cancel: Box::new(move || {
                let client = cancel_client.clone();
                let model = cancel_model.clone();
                Box::pin(async move { let _ = client.unload(&model, None).await; })
            }),
        },
    )
    .await?;
    if let Some(catalog) = outcome {
        sync_catalog(registry, client, Some(catalog)).await?;
        notify.notify(&format!("Downloaded {model}"), NotificationType::Info);
    }
    Ok(())
}

/// The pinned `/llama` management loop, driven on the runtime against the channel UI.
async fn run_flow(
    registry: Arc<dyn ModelRegistry>,
    notify: Arc<dyn ExtensionUi>,
    client: LlamaClient,
    ui: Arc<dyn LlamaUi>,
) {
    let Some(mut catalog) = read_catalog(&registry, &client, &ui).await else {
        return;
    };
    loop {
        let action = ui.show_models(client.server_url.clone(), catalog.clone()).await;
        if matches!(action, LlamaManagerAction::Close) {
            return;
        }
        let mut action_error: Option<String> = None;
        match &action {
            LlamaManagerAction::Download => {
                if let Err(error) = download_model(&notify, &registry, &client, ui.clone()).await {
                    action_error = Some(error);
                }
            }
            LlamaManagerAction::Model(model) if model_is_loaded(model) => {
                if let Err(error) = unload_model(&notify, &registry, &client, ui.clone(), model.clone()).await {
                    action_error = Some(error);
                }
            }
            LlamaManagerAction::Model(model) if model.status.value == "unloaded" => {
                if let Err(error) = load_model(&notify, &registry, &client, ui.clone(), catalog.clone(), model.clone()).await {
                    action_error = Some(error);
                }
            }
            LlamaManagerAction::Model(model) => {
                notify.notify(&format!("{} is {}", model.id, model.status.value), NotificationType::Warning);
            }
            LlamaManagerAction::Close => return,
        }
        let Some(refreshed) = read_catalog(&registry, &client, &ui).await else {
            return;
        };
        catalog = refreshed;
        if let Some(error) = action_error.filter(|error| !is_connection_error(error)) {
            notify.notify(&error, NotificationType::Error);
        }
    }
}

/// The UI-thread repaint binding, built from the production `ExtensionUi::request_render` seam.
fn ui_render_binding(ui: Arc<dyn ExtensionUi>) -> RenderBinding {
    Arc::new(move |_host: &dyn ExtensionTuiHost| -> Rc<dyn Fn()> {
        let ui = ui.clone();
        Rc::new(move || {
            let _ = ui.request_render();
        })
    })
}

pub struct LlamaExtension;

impl Extension for LlamaExtension {
    fn register(&self, api: &mut ExtensionApi) {
        if let Err(error) = api.register_provider(LLAMA_PROVIDER_ID, llama_provider_config()) {
            api.register_command(
                "llama",
                Some("Manage llama.cpp router models".to_owned()),
                None,
                Arc::new(move |_args, _ctx| {
                    let error = error.clone();
                    Box::pin(async move { Err(error) })
                }),
            );
            return;
        }
        api.register_command_with_context(
            "llama",
            Some("Manage llama.cpp router models".to_owned()),
            None,
            Arc::new(|_args, ctx| Box::pin(async move {
                if ctx.mode != ExtensionMode::Tui {
                    ctx.ui.notify("/llama is available in interactive mode", NotificationType::Warning);
                    return Ok(());
                }
                let Some(client) = configured_client(ctx).await else {
                    return Ok(());
                };
                let registry = ctx.model_registry.clone();
                let notify = ctx.ui.clone();
                let render = ui_render_binding(ctx.ui.clone());
                let flow_client = client;
                show_llama_ui(
                    ctx.context.clone(),
                    ShowLlamaOptions {
                        render,
                        flow: Box::new(move |ui| Box::pin(async move { run_flow(registry, notify, flow_client, ui).await })),
                    },
                )
                .await?;
                Ok(())
            })),
        );
    }
}

pub fn llama() -> Box<dyn Extension> {
    Box::new(LlamaExtension)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use maho_ext_api::{
        BuildSystemPromptOptions, ComponentFactory, CustomComponentFactory, CustomUiFactoryOptions,
        CustomUiOptions, EditMessageOptions, EditMessageResult, EventBus, ExtensionApi,
        ExtensionCommandContext, ExtensionCommandContextActions, ExtensionContext, ExtensionFailure,
        ExtensionFuture, ExtensionRuntime, ExtensionSessionProfile, ExtensionTreeNavigationOptions, ExtensionUi,
        ExtensionUiDialogOptions, ExtensionUiFactories, ExtensionWidgetOptions, FooterComponentFactory,
        ForkOptions, JsonValue, LoadedExtension, Model, ModelRegistry, NewSessionOptions,
        NotificationType, SessionEntry, SessionManager, SessionNavigationResult, SourceInfo,
        SwitchSessionOptions, Theme, ToolSessionManager, TuiComponentFactory, UiFuture, WidgetContent,
    };

    #[test]
    fn parse_hugging_face_model_splits_quantization_after_the_owner() {
        assert_eq!(parse_hugging_face_model("owner/repo"), ("owner/repo".to_owned(), None));
        assert_eq!(parse_hugging_face_model("owner/repo:Q4_K_M"), ("owner/repo".to_owned(), Some("Q4_K_M".to_owned())));
        assert_eq!(parse_hugging_face_model("repo"), ("repo".to_owned(), None));
        assert_eq!(parse_hugging_face_model("a/b:q:x"), ("a/b".to_owned(), Some("q:x".to_owned())));
    }

    #[test]
    fn connection_errors_map_to_the_pinned_message() {
        assert_eq!(connection_error_message("fetch failed"), "Could not connect to the server.");
        assert_eq!(connection_error_message("request timeout"), "Could not connect to the server.");
        assert_eq!(connection_error_message("network down"), "Could not connect to the server.");
        assert_eq!(connection_error_message("HTTP 500"), "HTTP 500");
    }

    struct FauxSession;
    impl ToolSessionManager for FauxSession {
        fn session_id(&self) -> &str { "llama-flow-test" }
        fn session_file(&self) -> Option<&std::path::Path> { None }
    }
    impl SessionManager for FauxSession {
        fn get_entries(&self) -> Vec<SessionEntry> { Vec::new() }
        fn get_branch(&self) -> Vec<SessionEntry> { Vec::new() }
        fn get_leaf_id(&self) -> Option<String> { None }
        fn get_session_name(&self) -> Option<String> { None }
    }

    struct FauxRegistry;
    impl ModelRegistry for FauxRegistry {
        fn get_all(&self) -> Vec<Model> { Vec::new() }
        fn get_available(&self) -> Vec<Model> { Vec::new() }
        fn find(&self, _: &str, _: &str) -> Option<Model> { None }
        fn has_configured_auth(&self, _: &Model) -> bool { true }
        fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(Some("key".to_owned())) }) }
        fn get_provider_auth<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<maho_ai::models::AuthResolution>> {
            Box::pin(async {
                Ok(Some(maho_ai::models::AuthResolution {
                    auth: maho_ai::models::ProviderAuthResult { api_key: Some("key".to_owned()), headers: None, base_url: Some("http://127.0.0.1:8080".to_owned()) },
                    env: None,
                }))
            })
        }
    }

    struct FauxFactories { custom_calls: AtomicUsize }
    impl ExtensionUiFactories for FauxFactories {
        fn set_widget_factory(&self, _: &str, _: Option<TuiComponentFactory>, _: ExtensionWidgetOptions) {}
        fn set_header_factory(&self, _: Option<TuiComponentFactory>) {}
        fn set_footer_factory(&self, _: Option<FooterComponentFactory>) {}
        fn custom_factory(&self, _: CustomComponentFactory, _: CustomUiFactoryOptions) -> ExtensionFuture<'_, JsonValue> {
            self.custom_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(JsonValue::Null) })
        }
    }

    struct FauxUi { factories: FauxFactories, notifies: AtomicUsize }
    impl ExtensionUi for FauxUi {
        fn factories(&self) -> Option<&dyn ExtensionUiFactories> { Some(&self.factories) }
        fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
        fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
        fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
        fn notify(&self, _: &str, _: NotificationType) { self.notifies.fetch_add(1, Ordering::SeqCst); }
        fn set_status(&self, _: &str, _: Option<&str>) {}
        fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
        fn set_header(&self, _: Option<ComponentFactory>) {}
        fn set_footer(&self, _: Option<ComponentFactory>) {}
        fn set_title(&self, _: &str) {}
        fn paste_to_editor(&self, _: &str) {}
        fn set_editor_text(&self, _: &str) {}
        fn get_editor_text(&self) -> String { String::new() }
        fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err(ExtensionFailure::new("unavailable")) }) }
        fn theme(&self) -> Theme { Theme::default() }
    }

    struct FauxActions;
    impl ExtensionCommandContextActions for FauxActions {
        fn wait_for_idle(&self) -> ExtensionFuture<'_, ()> { Box::pin(async { Ok(()) }) }
        fn new_session(&self, _: NewSessionOptions) -> ExtensionFuture<'_, SessionNavigationResult> { Box::pin(async { Ok(SessionNavigationResult::default()) }) }
        fn fork<'a>(&'a self, _: &'a str, _: ForkOptions) -> ExtensionFuture<'a, SessionNavigationResult> { Box::pin(async { Ok(SessionNavigationResult::default()) }) }
        fn navigate_tree<'a>(&'a self, _: &'a str, _: ExtensionTreeNavigationOptions) -> ExtensionFuture<'a, SessionNavigationResult> { Box::pin(async { Ok(SessionNavigationResult::default()) }) }
        fn edit_assistant_message<'a>(&'a self, _: &'a str, _: &'a str, _: EditMessageOptions) -> ExtensionFuture<'a, EditMessageResult> { Box::pin(async { Ok(EditMessageResult::default()) }) }
        fn edit_user_message<'a>(&'a self, _: &'a str, _: &'a str, _: EditMessageOptions) -> ExtensionFuture<'a, EditMessageResult> { Box::pin(async { Ok(EditMessageResult::default()) }) }
        fn switch_session<'a>(&'a self, _: &'a str, _: SwitchSessionOptions) -> ExtensionFuture<'a, SessionNavigationResult> { Box::pin(async { Ok(SessionNavigationResult::default()) }) }
        fn reload(&self) -> ExtensionFuture<'_, ()> { Box::pin(async { Ok(()) }) }
    }

    fn command_context(dir: &std::path::Path, mode: ExtensionMode) -> (ExtensionCommandContext, Arc<FauxUi>) {
        let ui = Arc::new(FauxUi { factories: FauxFactories { custom_calls: AtomicUsize::new(0) }, notifies: AtomicUsize::new(0) });
        let context = ExtensionContext {
            ui: ui.clone(), mode, has_ui: mode == ExtensionMode::Tui, cwd: dir.into(), agent_dir: dir.into(),
            session_manager: Arc::new(FauxSession), model_registry: Arc::new(FauxRegistry), model: None, thinking_level: None,
            service_tier: None, effective_service_tier: None, scoped_models: Vec::new(), goal_store_file: None,
            loaded_extension_paths: Vec::new(), signal: None, steering_signal: None,
            is_idle_fn: Arc::new(|| true), wait_for_idle_fn: Arc::new(|| Box::pin(async {})), is_project_trusted_fn: Arc::new(|| true),
            is_compacting_fn: Arc::new(|| false), get_system_prompt_fn: Arc::new(String::new),
            get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default), registered_mcp_servers: Vec::new(),
            update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None, compaction_signal: Default::default(),
        };
        (ExtensionCommandContext { context, actions: Arc::new(FauxActions), runtime: ExtensionRuntime::default() }, ui)
    }

    fn llama_command(dir: &std::path::Path) -> maho_ext_api::CommandContextHandler {
        let mut api = ExtensionApi::new(
            LoadedExtension::new("<inline:llama.cpp>", dir.into(), SourceInfo::default()),
            ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default(),
        );
        LlamaExtension.register(&mut api);
        api.registered.command_context_handlers.get("llama").cloned().expect("the llama command is registered")
    }

    #[tokio::test]
    async fn registered_llama_command_drives_the_ui_flow() {
        let dir = tempfile::tempdir().expect("dir");
        let handler = llama_command(dir.path());
        let (ctx, ui) = command_context(dir.path(), ExtensionMode::Tui);
        handler("", &ctx).await.expect("the /llama handler succeeds");
        assert_eq!(ui.factories.custom_calls.load(Ordering::SeqCst), 1, "the registered command must start the llama UI flow");
    }

    #[tokio::test]
    async fn llama_command_is_inert_outside_the_tui() {
        let dir = tempfile::tempdir().expect("dir");
        let handler = llama_command(dir.path());
        let (ctx, ui) = command_context(dir.path(), ExtensionMode::Print);
        handler("", &ctx).await.expect("the /llama handler succeeds");
        assert_eq!(ui.factories.custom_calls.load(Ordering::SeqCst), 0, "a non-interactive mode must not start the UI flow");
        assert_eq!(ui.notifies.load(Ordering::SeqCst), 1, "a non-interactive mode must notify instead");
    }
}
