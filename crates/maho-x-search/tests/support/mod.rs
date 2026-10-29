//! Test doubles: scripted HTTP, a fake credential store, and an extension context.
#![allow(dead_code)]

use maho_ext_api::*;
use maho_x_search::request::{HttpClient, HttpFuture, HttpRequest, HttpResponse};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// What the fake transport does with a request.
#[derive(Clone)]
pub enum Reply {
    Respond(HttpResponse),
    Fail(String),
    /// Never answers (for deadline and cancellation tests).
    Hang,
}

/// Records every request and answers with a scripted [`Reply`].
pub struct FakeHttp {
    pub reply: Mutex<Reply>,
    pub requests: Mutex<Vec<HttpRequest>>,
    /// Signalled once a request reached the transport.
    pub received: tokio::sync::Notify,
}

impl FakeHttp {
    pub fn new(reply: Reply) -> Arc<Self> {
        Arc::new(Self {
            reply: Mutex::new(reply),
            requests: Mutex::new(Vec::new()),
            received: tokio::sync::Notify::new(),
        })
    }
    pub fn json(status: u16, body: &serde_json::Value) -> Arc<Self> {
        Self::new(Reply::Respond(response(status, &body.to_string())))
    }
    pub fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().expect("requests").clone()
    }
}

pub fn response(status: u16, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        retry_after: None,
        body: body.to_owned(),
    }
}

impl HttpClient for FakeHttp {
    fn post(&self, request: HttpRequest) -> HttpFuture<'_> {
        self.requests.lock().expect("requests").push(request);
        let reply = self.reply.lock().expect("reply").clone();
        self.received.notify_one();
        Box::pin(async move {
            match reply {
                Reply::Respond(response) => Ok(response),
                Reply::Fail(message) => Err(message),
                Reply::Hang => std::future::pending().await,
            }
        })
    }
}

/// A credential store with a mutable `xai` entry.
pub struct FakeRegistry {
    pub stored: Mutex<Option<maho_ai::auth::types::CredentialType>>,
    pub key: Mutex<Result<Option<String>, String>>,
}

impl FakeRegistry {
    pub fn new(
        stored: Option<maho_ai::auth::types::CredentialType>,
        key: Result<Option<String>, String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            stored: Mutex::new(stored),
            key: Mutex::new(key),
        })
    }
}

impl ModelRegistry for FakeRegistry {
    fn get_all(&self) -> Vec<Model> {
        Vec::new()
    }
    fn get_available(&self) -> Vec<Model> {
        Vec::new()
    }
    fn find(&self, _: &str, _: &str) -> Option<Model> {
        None
    }
    fn has_configured_auth(&self, _: &Model) -> bool {
        false
    }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
    fn get_provider_auth<'a>(
        &'a self,
        provider: &'a str,
    ) -> ExtensionFuture<'a, Option<maho_ai::models::AuthResolution>> {
        let key = self.key.lock().expect("key").clone();
        Box::pin(async move {
            assert_eq!(provider, "xai");
            match key {
                Ok(api_key) => Ok(Some(maho_ai::models::AuthResolution {
                    auth: maho_ai::models::ProviderAuthResult {
                        api_key,
                        ..Default::default()
                    },
                    ..Default::default()
                })),
                Err(message) => Err(ExtensionFailure::new(message)),
            }
        })
    }
    fn get_stored_credential_type(
        &self,
        provider: &str,
    ) -> Result<Option<maho_ai::auth::types::CredentialType>, ExtensionFailure> {
        Ok(if provider == "xai" {
            *self.stored.lock().expect("stored")
        } else {
            None
        })
    }
}

struct TestSession;
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str {
        "session"
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}
impl SessionManager for TestSession {
    fn get_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_leaf_id(&self) -> Option<String> {
        None
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
}

struct TestUi;
impl ExtensionUi for TestUi {
    fn factories(&self) -> Option<&dyn ExtensionUiFactories> {
        None
    }
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, _: &str, _: NotificationType) {}
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

pub fn context(cwd: &Path, registry: Arc<dyn ModelRegistry>) -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(TestUi),
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: cwd.into(),
        agent_dir: cwd.into(),
        session_manager: Arc::new(TestSession),
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
        get_system_prompt_fn: Arc::new(String::new),
        get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

pub const PATH: &str = "<builtin:x-search>";

pub fn api(cwd: &Path) -> ExtensionApi {
    ExtensionApi::new(
        LoadedExtension::new(PATH, cwd.into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    )
}
