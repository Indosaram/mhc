//! Offline fixture harness: no network, provider or auth call is made.

use maho_ai::types::{AssistantImages, ContentBlock, ImageContent, ImagesBackground, ImagesContext, ImagesModel, ImagesOptions, ImagesStopReason, TextContent, Usage};
use maho_ext_api::*;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

pub const PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl3T2QAAAAASUVORK5CYII=";

pub struct FixtureSession {
    pub entries: Vec<SessionEntry>,
}

impl ToolSessionManager for FixtureSession {
    fn session_id(&self) -> &str {
        "session"
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}

impl SessionManager for FixtureSession {
    fn get_entries(&self) -> Vec<SessionEntry> {
        self.entries.clone()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        self.entries.clone()
    }
    fn get_leaf_id(&self) -> Option<String> {
        None
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
}

pub struct FixtureRegistry {
    pub stored_api_key: bool,
    pub provider_api_key: Option<String>,
    pub provider_headers: Option<std::collections::BTreeMap<String, Option<String>>>,
    pub models: Vec<Model>,
}

impl Default for FixtureRegistry {
    fn default() -> Self {
        Self { stored_api_key: true, provider_api_key: Some("fixture-image-secret".into()), provider_headers: None, models: Vec::new() }
    }
}

impl ModelRegistry for FixtureRegistry {
    fn get_all(&self) -> Vec<Model> {
        self.models.clone()
    }
    fn get_available(&self) -> Vec<Model> {
        self.models.clone()
    }
    fn find(&self, _: &str, _: &str) -> Option<Model> {
        None
    }
    fn has_configured_auth(&self, _: &Model) -> bool {
        self.provider_api_key.is_some()
    }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
    fn get_stored_credential_type(&self, _: &str) -> Result<Option<maho_ai::auth::types::CredentialType>, ExtensionFailure> {
        Ok(self.stored_api_key.then_some(maho_ai::auth::types::CredentialType::ApiKey))
    }
    fn get_provider_auth<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<maho_ai::models::AuthResolution>> {
        let key = self.provider_api_key.clone();
        let headers = self.provider_headers.clone();
        Box::pin(async move {
            Ok(key.map(|api_key| maho_ai::models::AuthResolution {
                auth: maho_ai::models::ProviderAuthResult { api_key: Some(api_key), headers, base_url: None },
                env: None,
            }))
        })
    }
    fn get_api_key_and_headers<'a>(&'a self, model: &'a Model) -> ExtensionFuture<'a, ResolvedRequestAuth> {
        let key = self.provider_api_key.clone();
        let headers = self.provider_headers.clone();
        let provider = model.provider.clone();
        Box::pin(async move {
            Ok(ResolvedRequestAuth {
                auth: maho_ai::models::ProviderAuthResult { api_key: key, headers, base_url: Some(provider) },
                extra_body: None,
                upstream_model_id: None,
                service_tier: None,
                env: None,
            })
        })
    }
}

#[derive(Default)]
pub struct TestUi(pub Mutex<Vec<(String, NotificationType)>>);

impl ExtensionUi for TestUi {
    fn factories(&self) -> Option<&dyn ExtensionUiFactories> {
        Some(self)
    }
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, message: &str, kind: NotificationType) {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((message.into(), kind));
    }
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("UI not available".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

impl ExtensionUiFactories for TestUi {
    fn set_widget_factory(&self, _: &str, _: Option<TuiComponentFactory>, _: ExtensionWidgetOptions) {}
    fn set_header_factory(&self, _: Option<TuiComponentFactory>) {}
    fn set_footer_factory(&self, _: Option<FooterComponentFactory>) {}
    fn custom_factory(&self, _: CustomComponentFactory, _: CustomUiFactoryOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("UI not available".into()) })
    }
}

pub fn gateway_model() -> Model {
    serde_json::from_value(serde_json::json!({"id":"gpt-5","name":"GPT-5","api":"openai-completions","provider":"quotio-openai","baseUrl":"https://gateway.example/openai/v1","reasoning":false,"input":["text"],"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0},"contextWindow":100000,"maxTokens":1000})).expect("gateway model")
}

pub fn registry(gateway: bool) -> Arc<FixtureRegistry> {
    Arc::new(FixtureRegistry {
        stored_api_key: !gateway,
        provider_api_key: Some(if gateway { "gateway-secret".into() } else { "fixture-image-secret".into() }),
        provider_headers: None,
        models: if gateway { vec![gateway_model()] } else { Vec::new() },
    })
}

pub fn text_of(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn image_count(result: &AgentToolResult) -> usize {
    result.content.iter().filter(|block| matches!(block, ContentBlock::Image(_))).count()
}

pub async fn run(
    root: &Path,
    gateway: bool,
    stub: Arc<StubImages>,
    tool_call_id: &str,
    params: serde_json::Value,
) -> AgentToolResult {
    run_with(root, registry(gateway), stub, tool_call_id, params).await
}

pub async fn run_with(
    root: &Path,
    registry: Arc<dyn ModelRegistry>,
    stub: Arc<StubImages>,
    tool_call_id: &str,
    params: serde_json::Value,
) -> AgentToolResult {
    let scope = maho_ai::node::provider_scope::ProviderScope::new();
    let context = context(root, registry);
    let execute = executor("imagegen");
    let outcome = maho_ai::node::provider_scope::run_with_provider_scope_async(&scope, async {
        maho_ai::images_api_registry::register_images_api_provider("openai-images", stub.clone(), Some("tool-stub"))?;
        let result = execute(tool_call_id, params, None, None, &context).await?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(result)
    })
    .await;
    scope.close();
    outcome.expect("scope").expect("tool run")
}

pub fn context(cwd: &Path, registry: Arc<dyn ModelRegistry>) -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(TestUi::default()),
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: cwd.into(),
        agent_dir: cwd.into(),
        session_manager: Arc::new(FixtureSession { entries: Vec::new() }),
        model_registry: registry,
        model: None,
        thinking_level: None,
        service_tier: None,
        effective_service_tier: None,
        scoped_models: Vec::new(),
        goal_store_file: None,
        loaded_extension_paths: Vec::new(),
        signal: None,
        steering_signal: None,
        is_idle_fn: Arc::new(|| true),
        wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
        is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false),
        get_system_prompt_fn: Arc::new(|| String::new()),
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions::default()),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

pub fn executor(path: &str) -> ExtensionToolExecutor {
    let mut api = ExtensionApi::new(
        LoadedExtension::new(path, "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    maho_ext_imagegen::ImageGen::default().register(&mut api);
    api.runtime.extension_tool_executor(path, "generate_image").expect("registered generate_image executor")
}

#[derive(Default)]
pub struct StubImages {
    pub calls: Mutex<Vec<(ImagesModel, ImagesContext, ImagesOptions)>>,
    pub images: usize,
    pub revised_prompts: Mutex<Vec<String>>,
    pub usage: Option<Usage>,
    pub error: Option<String>,
    pub mime_type: Option<String>,
    pub background: Option<ImagesBackground>,
}

impl StubImages {
    pub fn one() -> Self {
        Self { images: 1, ..Default::default() }
    }
    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len()
    }
    pub fn first_call(&self) -> Option<(ImagesModel, ImagesContext, ImagesOptions)> {
        self.calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).first().cloned()
    }
}

impl maho_ai::types::ProviderImages for StubImages {
    fn generate_images<'a>(
        &'a self,
        model: &'a ImagesModel,
        context: &'a ImagesContext,
        options: Option<ImagesOptions>,
    ) -> maho_ai::types::BoxFuture<'a, AssistantImages> {
        let options = options.unwrap_or_default();
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((model.clone(), context.clone(), options.clone()));
        let images = self.images;
        let revised = self.revised_prompts.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let error = self.error.clone();
        let usage = self.usage;
        let background = self.background;
        let mime = self
            .mime_type
            .clone()
            .unwrap_or_else(|| format!("image/{}", options.extra.get("outputFormat").and_then(serde_json::Value::as_str).unwrap_or("png")));
        let api = model.api.clone();
        let provider = model.provider.clone();
        let id = model.id.clone();
        Box::pin(async move {
            let base = AssistantImages {
                api,
                provider,
                model: id,
                output: Vec::new(),
                response_id: None,
                usage: None,
                background: None,
                stop_reason: ImagesStopReason::Stop,
                error_message: None,
                timestamp: 0,
            };
            if let Some(error) = error {
                return AssistantImages { stop_reason: ImagesStopReason::Error, error_message: Some(error), ..base };
            }
            let mut output = Vec::new();
            for index in 0..images {
                if let Some(revised) = revised.get(index) {
                    output.push(ContentBlock::Text(TextContent { text: revised.clone(), ..Default::default() }));
                }
                output.push(ContentBlock::Image(ImageContent { data: PNG_BASE64.into(), mime_type: mime.clone() }));
            }
            AssistantImages { output, usage, background, ..base }
        })
    }
}
