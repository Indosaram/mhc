//! Rust mirror of senpi core/extensions/types.ts at fe8c564b.
//! Session, resource, registry and theme ports live here to keep the dependency graph acyclic.
use std::{collections::BTreeMap, fmt, future::Future, path::{Path, PathBuf}, pin::Pin, sync::{Arc, Mutex}};
pub use maho_agent::types::{AgentEvent, AgentMessage};
pub use maho_agent::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback};
pub use maho_tools::tool_definition_wrapper::wrap_tool_definition;
pub use maho_ai::{model::Model, types::{JsonValue, ThinkingLevel, Usage, ImageContent}};
pub use maho_ai::types::{Message, UserMessage, UserContent, AssistantMessage, ContentBlock};
pub use maho_tools::{ToolContext, ToolDefinition, FilesystemPolicy, FilesystemPolicyChecker, FilesystemPolicyDecision, FilesystemPolicyRequest};
pub use maho_tools::definition::{AbortSignal, ToolContent, ToolResult, ToolSessionManager, ToolExposure, ToolExecutionMode, ToolError, ToolCall};
pub use maho_tools::filesystem_policy::FilesystemOperation;
pub use maho_tui::tui::Component;
pub use maho_tui::autocomplete::AutocompleteItem;
pub use maho_tools::{bash::BashToolInput, read::ReadToolInput, edit::EditToolInput, write::WriteToolInput, grep::index::GrepToolInput, find::FindToolInput, ls::LsToolInput};

pub fn define_tool(tool: ToolDefinition) -> ToolDefinition { tool }
pub fn is_tool_call_event_type(name: &str, event: &ToolCallEvent) -> bool { event.tool_name == name }
pub fn is_bash_tool_result(event: &ToolResultEvent) -> bool { event.tool_name == "bash" }
pub fn is_power_shell_tool_result(event: &ToolResultEvent) -> bool { event.tool_name == "powershell" }
pub fn is_read_tool_result(event: &ToolResultEvent) -> bool { event.tool_name == "read" }
pub fn is_edit_tool_result(event: &ToolResultEvent) -> bool { event.tool_name == "edit" }
pub fn is_write_tool_result(event: &ToolResultEvent) -> bool { event.tool_name == "write" }
pub fn is_grep_tool_result(event: &ToolResultEvent) -> bool { event.tool_name == "grep" }
pub fn is_find_tool_result(event: &ToolResultEvent) -> bool { event.tool_name == "find" }
pub fn is_ls_tool_result(event: &ToolResultEvent) -> bool { event.tool_name == "ls" }

pub type ExtensionFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ExtensionFailure>> + Send + 'a>>;
pub type UiFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type ToolHookStatusUpdater = Arc<dyn Fn(&str) + Send + Sync>;
pub type ExtensionToolExecutor = Arc<dyn for<'a> Fn(&'a str, JsonValue, Option<maho_ai::utils::abort::AbortSignal>, Option<maho_agent::types::AgentToolUpdateCallback>, &'a ExtensionContext) -> ExtensionFuture<'a, AgentToolResult> + Send + Sync>;
#[derive(Clone, Debug, PartialEq)]
pub struct TypedAgentToolResult<TDetails> {
    pub content: Vec<ContentBlock>, pub details: TDetails, pub usage: Option<Usage>,
    pub added_tool_names: Option<Vec<String>>, pub terminate: Option<bool>, pub is_error: Option<bool>,
}
impl<TDetails: serde::Serialize> TypedAgentToolResult<TDetails> {
    pub fn into_agent_result(self) -> Result<AgentToolResult, ExtensionFailure> {
        Ok(AgentToolResult { content: self.content, details: serde_json::to_value(self.details).map_err(|error| ExtensionFailure::new(error.to_string()))?,
            usage: self.usage, added_tool_names: self.added_tool_names, terminate: self.terminate, is_error: self.is_error })
    }
}
impl<TDetails: serde::de::DeserializeOwned> TypedAgentToolResult<TDetails> {
    pub fn from_agent_result(result: &AgentToolResult) -> Result<Self, ExtensionFailure> {
        Ok(Self { content: result.content.clone(), details: serde_json::from_value(result.details.clone()).map_err(|error| ExtensionFailure::new(error.to_string()))?,
            usage: result.usage, added_tool_names: result.added_tool_names.clone(), terminate: result.terminate, is_error: result.is_error })
    }
}
pub type TypedToolUpdateCallback<TDetails> = Arc<dyn Fn(TypedAgentToolResult<TDetails>) -> Result<(), ExtensionFailure> + Send + Sync>;
pub type TypedExtensionToolExecutor<TArgs, TDetails> = Arc<dyn for<'a> Fn(&'a str, TArgs, Option<maho_ai::utils::abort::AbortSignal>, Option<TypedToolUpdateCallback<TDetails>>, &'a ExtensionContext) -> ExtensionFuture<'a, TypedAgentToolResult<TDetails>> + Send + Sync>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToolRenderResultOptions { pub expanded: bool, pub is_partial: bool }
pub struct ToolRenderContext<TState, TArgs> {
    pub args: TArgs,
    pub tool_call_id: String,
    pub invalidate: std::rc::Rc<dyn Fn()>,
    pub last_component: Option<Box<dyn Component>>,
    pub state: TState,
    pub cwd: PathBuf,
    pub execution_started: bool,
    pub args_complete: bool,
    pub is_partial: bool,
    pub expanded: bool,
    pub show_images: bool,
    pub image_protocol: Option<maho_tui::image_stub::ImageProtocol>,
    pub is_error: bool,
    pub has_result: Option<bool>,
    pub spinner_frame: Option<usize>,
}
pub type ToolCallRenderer<TState, TArgs> = Arc<dyn Fn(&TArgs, &Theme, &mut ToolRenderContext<TState, TArgs>) -> Box<dyn Component> + Send + Sync>;
pub type ToolResultRenderer<TState, TArgs> = Arc<dyn Fn(&AgentToolResult, ToolRenderResultOptions, &Theme, &mut ToolRenderContext<TState, TArgs>) -> Box<dyn Component> + Send + Sync>;
pub type TypedToolResultRenderer<TState, TArgs, TDetails> = Arc<dyn Fn(&TypedAgentToolResult<TDetails>, ToolRenderResultOptions, &Theme, &mut ToolRenderContext<TState, TArgs>) -> Box<dyn Component> + Send + Sync>;
pub fn typed_tool_result_renderer<TState: 'static, TArgs: 'static, TDetails: serde::de::DeserializeOwned + 'static>(renderer: TypedToolResultRenderer<TState, TArgs, TDetails>) -> ToolResultRenderer<TState, TArgs> {
    Arc::new(move |result, options, theme, context| {
        match TypedAgentToolResult::from_agent_result(result) {
            Ok(result) => renderer(&result, options, theme, context),
            Err(error) => std::panic::panic_any(error),
        }
    })
}
pub struct ToolRenderers<TState, TArgs> {
    pub render_call: Option<ToolCallRenderer<TState, TArgs>>,
    pub render_result: Option<ToolResultRenderer<TState, TArgs>>,
}
pub struct ToolRendererSession<TState, TArgs> {
    pub renderers: Arc<ToolRenderers<TState, TArgs>>,
    pub context: ToolRenderContext<TState, TArgs>,
}
pub struct ToolRendererSlots<TState, TArgs> {
    pub session: ToolRendererSession<TState, TArgs>,
    call_component: Option<Box<dyn Component>>,
    result_component: Option<Box<dyn Component>>,
}
impl<TState, TArgs> ToolRendererSession<TState, TArgs> {
    pub fn into_slots(mut self) -> ToolRendererSlots<TState, TArgs> {
        let call_component = self.context.last_component.take();
        ToolRendererSlots { session: self, call_component, result_component: None }
    }
}
impl<TState, TArgs: Clone> ToolRendererSlots<TState, TArgs> {
    pub fn render_call(&mut self, theme: &Theme, width: usize) -> Option<Vec<String>> {
        self.session.context.last_component = self.call_component.take();
        let lines = self.session.render_call(theme, width);
        self.call_component = self.session.context.last_component.take();
        lines
    }
    pub fn render_result(&mut self, result: &AgentToolResult, theme: &Theme, width: usize) -> Option<Vec<String>> {
        self.session.context.last_component = self.result_component.take();
        let lines = self.session.render_result(result, theme, width);
        self.result_component = self.session.context.last_component.take();
        lines
    }
}
impl<TState, TArgs: Clone> ToolRendererSession<TState, TArgs> {
    pub fn render_call(&mut self, theme: &Theme, width: usize) -> Option<Vec<String>> {
        let renderer = self.renderers.render_call.as_ref()?;
        let args = self.context.args.clone();
        let mut component = renderer(&args, theme, &mut self.context);
        let lines = component.render(width);
        self.context.last_component = Some(component);
        Some(lines)
    }
    pub fn render_result(&mut self, result: &AgentToolResult, theme: &Theme, width: usize) -> Option<Vec<String>> {
        let renderer = self.renderers.render_result.as_ref()?;
        let options = ToolRenderResultOptions { expanded: self.context.expanded, is_partial: self.context.is_partial };
        let mut component = renderer(result, options, theme, &mut self.context);
        let lines = component.render(width);
        self.context.last_component = Some(component);
        Some(lines)
    }
}

pub type LazyToolActivator = Arc<dyn Fn(&str) -> bool + Send + Sync>;
pub type ShortcutHandler = Arc<dyn for<'a> Fn(&'a ExtensionContext) -> ExtensionFuture<'a, ()> + Send + Sync>;
#[derive(Clone)]
pub struct ExtensionShortcut { pub shortcut: String, pub description: Option<String>, pub handler: ShortcutHandler, pub extension_path: String }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkdownMessageType { User, Assistant, AssistantThinking }
#[derive(Clone, Debug)]
pub struct MarkdownTransformContext { pub message_type: MarkdownMessageType, pub is_streaming: bool, pub available_width: usize }
pub type MarkdownTransformer = Arc<dyn Fn(&str, &MarkdownTransformContext) -> String + Send + Sync>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactReadKind { Docs, Resource, Skill, Memory }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactReadClassification { pub kind: CompactReadKind, pub label: String, pub headline: Option<String> }
pub type ReadClassifier = Arc<dyn Fn(&Path, &Path) -> Option<CompactReadClassification> + Send + Sync>;
pub type ExtensionRpcRequestHandler = Arc<dyn Fn(JsonValue) -> ExtensionFuture<'static, JsonValue> + Send + Sync>;
pub type ProviderStream = Arc<dyn Fn(&Model, &maho_ai::types::Context, Option<maho_ai::types::SimpleStreamOptions>) -> maho_ai::types::AssistantMessageEventStream + Send + Sync>;
pub type ProviderRefresh = Arc<dyn Fn(maho_ai::models::RefreshModelsContext) -> ExtensionFuture<'static, Vec<ProviderModelConfig>> + Send + Sync>;
#[derive(Clone, Debug)]
pub struct ProviderModelConfig {
    pub id: String, pub name: String, pub upstream_model_id: Option<String>, pub api: Option<String>, pub base_url: Option<String>,
    pub reasoning: bool, pub recover_text_tool_calls: Option<bool>, pub thinking_level_map: Option<maho_ai::types::ThinkingLevelMap>,
    pub input: Vec<maho_ai::types::InputModality>, pub cost: maho_ai::types::ModelCost, pub context_window: u64, pub max_tokens: u64,
    pub headers: Option<BTreeMap<String, String>>, pub extra_body: Option<JsonValue>, pub compat: Option<maho_ai::model::ModelCompat>,
}
#[derive(Clone, Default)]
pub struct ProviderConfig {
    pub name: Option<String>, pub base_url: Option<String>, pub api_key: Option<String>, pub api: Option<String>,
    pub stream_simple: Option<ProviderStream>, pub headers: Option<BTreeMap<String, String>>, pub extra_body: Option<JsonValue>,
    pub auth_header: Option<bool>, pub models: Option<Vec<ProviderModelConfig>>, pub refresh_models: Option<ProviderRefresh>,
    pub oauth: Option<Arc<dyn maho_ai::auth::types::OAuthAuth>>, pub fallback_eligible: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}
#[derive(Clone, Debug, Default)]
pub struct ProviderModelOptions {
    pub service_tier: Option<maho_ai::types::ServiceTierPreference>,
    pub prompt_preset: Option<String>,
    pub sampling_params: Option<BTreeMap<String, JsonValue>>,
    pub cache_retention: Option<maho_ai::types::CacheRetention>,
}
/// Complete provider options without changing existing config struct literals.
#[derive(Clone, Default)]
pub struct ProviderConfigOptions {
    pub config: ProviderConfig,
    pub model_options: BTreeMap<String, ProviderModelOptions>,
    pub retry_policy: Option<maho_ai::utils::retry_profile::types::RetryPolicyProfile>,
}
pub trait ExtensionOAuthConfig: Send + Sync {
    fn name(&self) -> &str;
    fn is_subscription(&self) -> bool { false }
    fn uses_callback_server(&self) -> Option<bool> { None }
    fn login<'a>(&'a self, callbacks: &'a dyn maho_ai::oauth::OAuthLoginCallbacks) -> ExtensionFuture<'a, maho_ai::oauth::OAuthCredentials>;
    fn refresh_token<'a>(&'a self, credentials: &'a maho_ai::oauth::OAuthCredentials, signal: &'a maho_ai::utils::abort::AbortSignal) -> ExtensionFuture<'a, maho_ai::oauth::OAuthCredentials>;
    fn get_api_key(&self, credentials: &maho_ai::oauth::OAuthCredentials) -> String;
    fn modify_models(&self, models: Vec<Model>, _: &maho_ai::oauth::OAuthCredentials) -> Vec<Model> { models }
}
#[derive(Clone)]
pub enum ProviderRegistration { Config { name: String, config: Box<ProviderConfig> }, ConfigOptions { name: String, options: Box<ProviderConfigOptions> }, Native(Arc<dyn maho_ai::models::Provider>) }
/// Object-only request fields for callers that want schema constraints at construction.
#[derive(Clone, Default)]
pub struct ProviderObjectConfig {
    pub config: ProviderConfig,
    pub extra_body: Option<BTreeMap<String, JsonValue>>,
    pub model_extra_bodies: BTreeMap<String, BTreeMap<String, JsonValue>>,
}
impl ProviderRegistration {
    pub fn name(&self) -> &str { match self { Self::Config { name, .. } | Self::ConfigOptions { name, .. } => name, Self::Native(provider) => provider.id() } }
}
pub trait ExtensionProviderActions: Send + Sync {
    fn register_provider(&self, registration: ProviderRegistration, extension_path: &str) -> Result<(), ExtensionFailure>;
    fn unregister_provider(&self, name: &str, extension_path: &str) -> Result<(), ExtensionFailure>;
}
pub struct ReadClassifierSubscription { state: Arc<Mutex<RuntimeState>>, id: u64 }
impl Drop for ReadClassifierSubscription {
    fn drop(&mut self) { self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).read_classifiers.retain(|(id, _)| *id != self.id); }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionFailure { pub message: String, pub stack: Option<String> }
impl ExtensionFailure {
    pub fn new(message: impl Into<String>) -> Self { Self { message: message.into(), stack: None } }
}
impl fmt::Display for ExtensionFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.message) }
}
impl std::error::Error for ExtensionFailure {}
impl From<String> for ExtensionFailure { fn from(message: String) -> Self { Self::new(message) } }
impl From<&str> for ExtensionFailure { fn from(message: &str) -> Self { Self::new(message) } }

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EventKind {
    ProjectTrust, ResourcesDiscover, SessionStart, SessionInfoChanged, SessionParked, SessionResumed,
    SessionBeforeSwitch, SessionBeforeFork, SessionBeforeReload, SessionBeforeCompact, SessionCompact,
    SessionCompactFailed, SessionShutdown, SessionAbort, SessionExtensionsRemoved, SessionBeforeTree, SessionTree,
    Context, BeforeProviderRequest, BeforeProviderHeaders, AfterProviderResponse, BeforeAgentStart,
    AgentStart, AgentEnd, AgentSettled, UiPromptStart, UiPromptEnd, TurnStart, TurnEnd, MessageStart,
    MessageUpdate, MessageEnd, ToolExecutionStart, ToolExecutionUpdate, ToolExecutionEnd, ModelSelect,
    SystemPromptChange, ThinkingLevelSelect, ToolCall, ToolResult, UserBash, Input, InputDisposition,
}
impl EventKind {
    pub const ALL: [Self; 43] = [Self::ProjectTrust, Self::ResourcesDiscover, Self::SessionStart,
        Self::SessionInfoChanged, Self::SessionParked, Self::SessionResumed, Self::SessionBeforeSwitch,
        Self::SessionBeforeFork, Self::SessionBeforeReload, Self::SessionBeforeCompact, Self::SessionCompact,
        Self::SessionCompactFailed, Self::SessionShutdown, Self::SessionAbort, Self::SessionExtensionsRemoved,
        Self::SessionBeforeTree, Self::SessionTree, Self::Context, Self::BeforeProviderRequest,
        Self::BeforeProviderHeaders, Self::AfterProviderResponse, Self::BeforeAgentStart, Self::AgentStart,
        Self::AgentEnd, Self::AgentSettled, Self::UiPromptStart, Self::UiPromptEnd, Self::TurnStart, Self::TurnEnd,
        Self::MessageStart, Self::MessageUpdate, Self::MessageEnd, Self::ToolExecutionStart,
        Self::ToolExecutionUpdate, Self::ToolExecutionEnd, Self::ModelSelect, Self::SystemPromptChange,
        Self::ThinkingLevelSelect, Self::ToolCall, Self::ToolResult, Self::UserBash, Self::Input, Self::InputDisposition];
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProjectTrust => "project_trust", Self::ResourcesDiscover => "resources_discover",
            Self::SessionStart => "session_start", Self::SessionInfoChanged => "session_info_changed",
            Self::SessionParked => "session_parked", Self::SessionResumed => "session_resumed",
            Self::SessionBeforeSwitch => "session_before_switch", Self::SessionBeforeFork => "session_before_fork",
            Self::SessionBeforeReload => "session_before_reload", Self::SessionBeforeCompact => "session_before_compact",
            Self::SessionCompact => "session_compact", Self::SessionCompactFailed => "session_compact_failed",
            Self::SessionShutdown => "session_shutdown", Self::SessionAbort => "session_abort",
            Self::SessionExtensionsRemoved => "session_extensions_removed", Self::SessionBeforeTree => "session_before_tree",
            Self::SessionTree => "session_tree", Self::Context => "context", Self::BeforeProviderRequest => "before_provider_request",
            Self::BeforeProviderHeaders => "before_provider_headers", Self::AfterProviderResponse => "after_provider_response",
            Self::BeforeAgentStart => "before_agent_start", Self::AgentStart => "agent_start", Self::AgentEnd => "agent_end",
            Self::AgentSettled => "agent_settled", Self::UiPromptStart => "ui_prompt_start", Self::UiPromptEnd => "ui_prompt_end",
            Self::TurnStart => "turn_start", Self::TurnEnd => "turn_end", Self::MessageStart => "message_start",
            Self::MessageUpdate => "message_update", Self::MessageEnd => "message_end", Self::ToolExecutionStart => "tool_execution_start",
            Self::ToolExecutionUpdate => "tool_execution_update", Self::ToolExecutionEnd => "tool_execution_end",
            Self::ModelSelect => "model_select", Self::SystemPromptChange => "system_prompt_change",
            Self::ThinkingLevelSelect => "thinking_level_select", Self::ToolCall => "tool_call", Self::ToolResult => "tool_result",
            Self::UserBash => "user_bash", Self::Input => "input", Self::InputDisposition => "input_disposition",
        }
    }
    pub fn parse(name: &str) -> Option<Self> { Self::ALL.into_iter().find(|kind| kind.as_str() == name) }
    pub const fn is_session_before(self) -> bool {
        matches!(self, Self::SessionBeforeSwitch | Self::SessionBeforeFork | Self::SessionBeforeReload | Self::SessionBeforeCompact | Self::SessionBeforeTree)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ExtensionMode { Tui, Rpc, AppServer, Json, #[default] Print }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SourceScope { User, Project, #[default] Temporary, System }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SourceOrigin { Package, #[default] TopLevel }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceInfo { pub path: String, pub source: String, pub scope: SourceScope, pub origin: SourceOrigin, pub base_dir: Option<String> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill { pub name: String, pub description: String, pub file_path: String, pub base_dir: String, pub source_info: SourceInfo, pub disable_model_invocation: bool }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceDiscoverEntry { pub path: String, pub scope: Option<SourceScope> }
impl From<String> for ResourceDiscoverEntry { fn from(path: String) -> Self { Self { path, scope: None } } }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredResourceEntry { pub path: String, pub scope: Option<SourceScope>, pub extension_path: String }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourcesDiscoverResult { pub skill_paths: Vec<ResourceDiscoverEntry>, pub prompt_paths: Vec<ResourceDiscoverEntry>, pub theme_paths: Vec<ResourceDiscoverEntry>, pub hook_paths: Vec<ResourceDiscoverEntry> }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiscoveredResources { pub skill_paths: Vec<DiscoveredResourceEntry>, pub prompt_paths: Vec<DiscoveredResourceEntry>, pub theme_paths: Vec<DiscoveredResourceEntry>, pub hook_paths: Vec<DiscoveredResourceEntry> }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Theme { pub name: Option<String>, pub colors: BTreeMap<String, String>, pub backgrounds: BTreeMap<String, String>, pub vars: BTreeMap<String, String> }
#[derive(Clone, Debug)]
pub struct ScopedModel { pub model: Model, pub thinking_level: Option<ThinkingLevel>, pub service_tier: Option<ServiceTier> }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceTier { Auto, Flex, Priority }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialAccountSource { Login, Import, Env }
impl CredentialAccountSource {
    pub const fn as_str(self) -> &'static str {
        match self { Self::Login => "login", Self::Import => "import", Self::Env => "env" }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialAccountSummary {
    pub name: String, pub display_name: Option<String>, pub source: CredentialAccountSource,
    pub blocked: bool, pub pinned: bool,
}

/// Host implementations adapt their owning registry, without an ext-api -> core edge.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedRequestAuth {
    pub auth: maho_ai::models::ProviderAuthResult,
    pub extra_body: Option<serde_json::Map<String, JsonValue>>,
    pub upstream_model_id: Option<String>,
    pub service_tier: Option<maho_ai::types::ServiceTierPreference>,
    pub env: Option<maho_ai::types::ProviderEnv>,
}

pub trait ModelRegistry: Send + Sync {
    fn get_all(&self) -> Vec<Model>;
    fn get_available(&self) -> Vec<Model>;
    fn find(&self, provider: &str, id: &str) -> Option<Model>;
    fn has_configured_auth(&self, model: &Model) -> bool;
    fn get_api_key_for_provider<'a>(&'a self, provider: &'a str) -> ExtensionFuture<'a, Option<String>>;
    fn get_provider_auth<'a>(&'a self, _provider: &'a str) -> ExtensionFuture<'a, Option<maho_ai::models::AuthResolution>> {
        Box::pin(async { Err(ExtensionFailure::new("Provider auth is not supported by this model registry")) })
    }
    fn get_stored_credential_type(&self, _provider: &str) -> Result<Option<maho_ai::auth::types::CredentialType>, ExtensionFailure> {
        Err(ExtensionFailure::new("Stored credential metadata is not supported by this model registry"))
    }
    fn stream_simple(&self, _model: &Model, _context: &maho_ai::types::Context, _options: Option<maho_ai::types::SimpleStreamOptions>) -> Result<maho_ai::utils::event_stream::AssistantMessageEventStream, ExtensionFailure> {
        Err(ExtensionFailure::new("Configured streaming is not supported by this model registry"))
    }
    fn get_api_key_and_headers<'a>(&'a self, _model: &'a Model) -> ExtensionFuture<'a, ResolvedRequestAuth> {
        Box::pin(async { Err(ExtensionFailure::new("Model request auth is not supported by this model registry")) })
    }
    fn get_credential_accounts<'a>(&'a self, _provider: &'a str) -> ExtensionFuture<'a, Vec<CredentialAccountSummary>> {
        Box::pin(async { Err(ExtensionFailure::new("Credential account listing is not supported by this model registry")) })
    }
    fn pin_credential_account<'a>(&'a self, _provider: &'a str, _name: Option<&'a str>) -> ExtensionFuture<'a, ()> {
        Box::pin(async { Err(ExtensionFailure::new("Credential account pinning is not supported by this model registry")) })
    }
    fn remove_credential_account<'a>(&'a self, _provider: &'a str, _name: &'a str) -> ExtensionFuture<'a, ()> {
        Box::pin(async { Err(ExtensionFailure::new("Credential account removal is not supported by this model registry")) })
    }
    fn rename_credential_account<'a>(&'a self, _provider: &'a str, _name: &'a str, _display_name: Option<&'a str>) -> ExtensionFuture<'a, ()> {
        Box::pin(async { Err(ExtensionFailure::new("Credential account renaming is not supported by this model registry")) })
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct SessionEntry { pub id: String, pub parent_id: Option<String>, pub timestamp: String, pub kind: String, pub data: JsonValue }
pub trait SessionManager: ToolSessionManager {
    fn get_entries(&self) -> Vec<SessionEntry>;
    fn get_branch(&self) -> Vec<SessionEntry>;
    fn get_leaf_id(&self) -> Option<String>;
    fn get_session_name(&self) -> Option<String>;
    fn extension_context_actions(&self) -> Option<&dyn ExtensionContextActions> { None }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextUsage { pub tokens: Option<u64>, pub context_window: u64, pub percent: Option<f64> }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReloadVetoDecision { pub cancelled: bool, pub reason: Option<String> }
#[derive(Clone, Debug, PartialEq)]
pub struct PromptCacheKeepAliveSettings { pub enabled: bool, pub max_requests_per_session: u64, pub max_cost_usd_per_session: f64, pub margin_seconds: f64 }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LookAtSettings { pub enabled: bool, pub models: Option<Vec<String>> }
#[derive(Clone, Debug, PartialEq)]
pub struct AskUserSettings { pub enabled: bool, pub timeout_minutes: f64 }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageSettings { pub auto_resize: bool, pub block_images: bool }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedHookSources {
    pub cwd: PathBuf, pub agent_dir: PathBuf, pub global_hooks_path: PathBuf, pub project_hooks_path: PathBuf,
    pub global_settings_hooks: Option<JsonValue>, pub project_settings_hooks: Option<JsonValue>,
    pub global_hook_source_paths: Vec<PathBuf>, pub project_hook_source_paths: Vec<PathBuf>,
    pub pre_session_hook_source_paths: Vec<PathBuf>, pub runtime_hook_source_paths: Vec<PathBuf>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackRevertPolicy { CooldownExpiry, Never }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryFallbackSettings { pub model_fallback: bool, pub chains: BTreeMap<String, Vec<String>>, pub revert_policy: FallbackRevertPolicy }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryFallbackStatus { pub active: bool, pub current_model: Option<String>, pub original_selector: Option<String>, pub pinned: bool }
pub trait ExtensionSessionSettings: Send + Sync {
    fn get_retry_fallback_settings(&self) -> RetryFallbackSettings;
    fn set_fallback_chain<'a>(&'a self, key: &'a str, entries: &'a [String]) -> ExtensionFuture<'a, ()>;
    fn remove_fallback_chain<'a>(&'a self, key: &'a str) -> ExtensionFuture<'a, ()>;
    fn set_model_fallback_enabled(&self, enabled: bool) -> ExtensionFuture<'_, ()>;
    fn set_fallback_revert_policy(&self, policy: FallbackRevertPolicy) -> ExtensionFuture<'_, ()>;
    fn reload(&self) -> ExtensionFuture<'_, ()>;
    fn get_fallback_status(&self) -> Option<RetryFallbackStatus>;
}
#[derive(Clone, Default)]
pub struct CompactOptions { pub custom_instructions: Option<String>, pub on_complete: Option<Arc<dyn Fn(CompactionResult) + Send + Sync>>, pub on_error: Option<Arc<dyn Fn(ExtensionFailure) + Send + Sync>> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WarmAnchorSnapshot { pub first_kept_entry_id: String, pub prefix_entry_ids: Vec<String>, pub latest_compaction_entry_id: Option<String> }
#[derive(Clone, Debug)]
pub struct ApplyCompactionOptions { pub reason: CompactionReason, pub expected_revision: Option<u64>, pub expected_warm_anchor: Option<WarmAnchorSnapshot>, pub signal: Option<AbortSignal> }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyCompactionResult { Applied, Stale, Rejected }
#[derive(Clone, Debug)]
pub struct BeginCompactionOptions { pub reason: CompactionReason }
#[derive(Clone, Debug)]
pub struct UpdateCompactionOptions { pub reason: CompactionReason, pub signal: Option<AbortSignal>, pub delta: Option<String>, pub text: Option<String> }
#[derive(Clone, Debug)]
pub struct EndCompactionOptions { pub reason: CompactionReason, pub signal: Option<AbortSignal>, pub aborted: Option<bool>, pub error_message: Option<String> }
pub type ProviderHeaderTransform = Arc<dyn Fn(BTreeMap<String, Option<String>>) -> ExtensionFuture<'static, BTreeMap<String, Option<String>>> + Send + Sync>;
pub struct ProviderRequestPreparation {
    pub messages: Vec<AgentMessage>,
    pub transform_payload: Arc<dyn Fn(JsonValue) -> ExtensionFuture<'static, JsonValue> + Send + Sync>,
    pub transform_headers: ProviderHeaderTransform,
}
#[derive(Clone, Debug, Default)]
pub struct KernelToolInvokeScope { pub allow: Option<Vec<String>>, pub deny: Option<Vec<String>> }
#[derive(Clone, Debug, Default)]
pub struct KernelToolInvokeOptions { pub signal: Option<AbortSignal>, pub scope: Option<KernelToolInvokeScope> }
#[derive(Clone, Debug)]
pub struct KernelToolInvokeRequest { pub name: String, pub kernel_generation: u64, pub definition_revision: u64, pub args: JsonValue, pub call_id: String }
pub trait ExtensionKernelTools: Send + Sync {
    fn invoke_scope(&self) -> bool;
    fn describe<'a>(&'a self, names: &'a [String]) -> ExtensionFuture<'a, JsonValue>;
    fn invoke(&self, request: KernelToolInvokeRequest, options: KernelToolInvokeOptions) -> ExtensionFuture<'_, JsonValue>;
}
pub trait ExtensionContextActions: Send + Sync {
    fn assert_active(&self) -> Result<(), ExtensionFailure> { Ok(()) }
    fn set_approved_monitor_parent(&self, _tool_call_id: &str, _input: &JsonValue, _parent: &Path) -> Result<(), ExtensionFailure> {
        Err(ExtensionFailure::new("Monitor admission attachment is not supported by this extension context"))
    }
    fn take_approved_monitor_parent(&self, _tool_call_id: &str, _input: &JsonValue) -> Result<Option<PathBuf>, ExtensionFailure> {
        Err(ExtensionFailure::new("Monitor admission identity is not supported by this extension context"))
    }
    fn get_model(&self) -> Option<Model>;
    fn get_service_tier(&self) -> Option<ServiceTier>;
    fn get_effective_service_tier(&self) -> Option<ServiceTier> { self.get_service_tier() }
    fn get_scoped_models(&self) -> Vec<ScopedModel>;
    fn get_agent_dir(&self) -> PathBuf;
    fn is_idle(&self) -> bool;
    fn is_project_trusted(&self) -> bool;
    fn get_signal(&self) -> Option<AbortSignal>;
    fn get_steering_signal(&self) -> Option<AbortSignal> { None }
    fn get_thinking_level(&self) -> Option<ThinkingLevel> { None }
    fn abort(&self, source: Option<AbortSource>);
    fn has_pending_messages(&self) -> bool;
    fn request_reload(&self) -> ExtensionFuture<'_, ()>;
    fn is_compacting(&self) -> bool;
    fn check_reload_veto(&self) -> ExtensionFuture<'_, ReloadVetoDecision>;
    fn shutdown(&self);
    fn get_context_usage(&self) -> Option<ContextUsage>;
    fn get_compaction_settings(&self) -> CompactionSettings;
    fn get_resolved_compaction_settings(&self) -> Option<ResolvedCompactionSettings> { None }
    fn get_compaction_preparation(&self) -> Option<CompactionPreparationDetails> { None }
    fn get_prompt_cache_safe_wait_seconds(&self) -> Option<f64>;
    fn get_prompt_cache_goal_backstop_max_seconds(&self) -> f64;
    fn get_prompt_cache_keep_alive_settings(&self) -> PromptCacheKeepAliveSettings;
    fn get_look_at_settings(&self) -> LookAtSettings;
    fn get_ask_user_settings(&self) -> AskUserSettings;
    fn get_image_settings(&self) -> ImageSettings;
    fn session_settings(&self) -> &dyn ExtensionSessionSettings;
    fn compact(&self, options: CompactOptions);
    fn prepare_provider_request(&self, messages: Vec<AgentMessage>) -> ExtensionFuture<'_, ProviderRequestPreparation>;
    fn begin_compaction(&self, options: BeginCompactionOptions) -> Option<AbortSignal>;
    fn update_compaction(&self, options: UpdateCompactionOptions);
    fn end_compaction(&self, options: EndCompactionOptions);
    fn get_message_revision(&self) -> u64;
    fn apply_compaction(&self, result: CompactionResult, options: ApplyCompactionOptions) -> ExtensionFuture<'_, ApplyCompactionResult>;
    fn get_system_prompt(&self) -> String;
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions;
    fn get_loaded_hook_sources(&self) -> LoadedHookSources;
    fn get_registered_mcp_servers(&self) -> Option<Vec<RegisteredMcpServerDeclaration>> { None }
    fn kernel_tools(&self) -> Option<&dyn ExtensionKernelTools>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WidgetPlacement { #[default] AboveEditor, BelowEditor }
impl WidgetPlacement { pub const fn as_str(self) -> &'static str { match self { Self::AboveEditor => "aboveEditor", Self::BelowEditor => "belowEditor" } } }
#[derive(Clone, Debug, Default)]
pub struct ExtensionUiDialogOptions { pub signal: Option<AbortSignal>, pub timeout_ms: Option<u64> }
#[derive(Clone, Debug, Default)]
pub struct ExtensionWidgetOptions { pub placement: WidgetPlacement }
#[derive(Clone, Debug, Default)]
pub struct WorkingIndicatorOptions { pub frames: Option<Vec<String>>, pub interval_ms: Option<u64> }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalInputResult { pub consume: Option<bool>, pub data: Option<String> }
pub type TerminalInputHandler = Arc<dyn Fn(&str) -> Option<TerminalInputResult> + Send + Sync>;
pub type UiUnsubscribe = Box<dyn FnOnce() + Send>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionOption { pub label: String, pub description: Option<String> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question { pub id: String, pub header: String, pub question: String, pub options: Vec<QuestionOption>, pub multi_select: bool }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionRequest { pub request_id: String, pub questions: Vec<Question>, pub wait_for_answer: bool, pub timeout_ms: u64 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestionStatus { Answered, CommentSubmitted, TimedOut, Cancelled, OrphanedAfterRestart, Unavailable }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuestionAnswer { pub selected: Vec<String>, pub text: Option<String> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionResponse { pub status: QuestionStatus, pub answers: BTreeMap<String, QuestionAnswer>, pub comment: Option<String>, pub unanswered: Vec<String>, pub auto_resolved_after_ms: Option<u64> }
#[derive(Clone, Debug, Default)]
pub struct QuestionDraft { pub answers: Option<BTreeMap<String, QuestionAnswer>>, pub comment: Option<String> }
#[derive(Clone, Default)]
pub struct QuestionOptions { pub dialog: ExtensionUiDialogOptions, pub on_progress: Option<Arc<dyn Fn(QuestionDraft) + Send + Sync>> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeInfo { pub name: String, pub path: Option<PathBuf> }
#[derive(Clone, Debug)]
pub enum ThemeSelection { Name(String), Theme(Theme) }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetThemeResult { pub success: bool, pub error: Option<String> }
pub type AutocompleteProviderFactory = Arc<dyn Fn(Box<dyn maho_tui::autocomplete::AutocompleteProvider>) -> Box<dyn maho_tui::autocomplete::AutocompleteProvider> + Send + Sync>;
pub type EditorFactory = Arc<dyn Fn(std::rc::Rc<dyn maho_tui::components::editor::EditorTuiHost>, maho_tui::components::editor::EditorTheme, &maho_tui::keybindings::KeybindingsManager) -> Box<dyn maho_tui::editor_component::EditorComponent> + Send + Sync>;
pub trait ExtensionUiActions: Send + Sync {
    fn question(&self, request: QuestionRequest, options: QuestionOptions) -> ExtensionFuture<'_, QuestionResponse>;
    fn on_terminal_input(&self, handler: TerminalInputHandler) -> UiUnsubscribe;
    fn set_working_message(&self, message: Option<&str>);
    fn set_working_visible(&self, visible: bool);
    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>);
    fn set_hidden_thinking_label(&self, label: Option<&str>);
    fn editor<'a>(&'a self, title: &'a str, prefill: Option<&'a str>) -> ExtensionFuture<'a, Option<String>>;
    fn add_autocomplete_provider(&self, factory: AutocompleteProviderFactory);
    fn set_editor_component(&self, factory: Option<EditorFactory>);
    fn get_editor_component(&self) -> Option<EditorFactory>;
    fn get_all_themes(&self) -> Vec<ThemeInfo>;
    fn get_theme(&self, name: &str) -> Option<Theme>;
    fn set_theme(&self, theme: ThemeSelection) -> SetThemeResult;
    fn get_tools_expanded(&self) -> bool;
    fn set_tools_expanded(&self, expanded: bool);
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationType { Info, Warning, Error }
/// Components retain senpi's render/invalidate/input contract; creation occurs on the UI thread.
pub type ComponentFactory = Arc<dyn Fn(&Theme) -> Box<dyn Component> + Send + Sync>;
pub trait ExtensionTuiHost {
    fn request_render(&self);
    fn dimensions(&self) -> (u16, u16);
}
pub trait ReadonlyFooterDataProvider {
    fn get_git_branch(&self) -> Option<String>;
    fn get_extension_statuses(&self) -> BTreeMap<String, String>;
    fn get_available_provider_count(&self) -> usize;
    fn on_branch_change(&self, callback: Arc<dyn Fn() + Send + Sync>) -> UiUnsubscribe;
}
pub type TuiComponentFactory = Arc<dyn Fn(&dyn ExtensionTuiHost, &Theme) -> Box<dyn Component> + Send + Sync>;
pub type FooterComponentFactory = Arc<dyn Fn(&dyn ExtensionTuiHost, &Theme, &dyn ReadonlyFooterDataProvider) -> Box<dyn Component> + Send + Sync>;
pub type CustomUiDone = std::rc::Rc<dyn Fn(JsonValue)>;
pub type CustomComponentFuture<'a> = Pin<Box<dyn Future<Output = Result<Box<dyn Component>, ExtensionFailure>> + 'a>>;
pub type CustomComponentFactory = Arc<dyn for<'a> Fn(&'a dyn ExtensionTuiHost, &'a Theme, &'a maho_tui::keybindings::KeybindingsManager, CustomUiDone) -> CustomComponentFuture<'a> + Send + Sync>;
pub type OverlayOptionsFactory = Arc<dyn Fn() -> maho_tui::tui::OverlayOptions + Send + Sync>;
#[derive(Clone)]
pub enum ExtensionOverlayOptions { Static(OverlayOptionsFactory), Dynamic(OverlayOptionsFactory) }
#[derive(Clone, Default)]
pub struct CustomUiFactoryOptions {
    pub overlay: bool,
    pub overlay_options: Option<ExtensionOverlayOptions>,
    pub on_handle: Option<Arc<dyn Fn(maho_tui::tui::OverlayHandle) + Send + Sync>>,
}
pub trait ExtensionUiFactories: Send + Sync {
    fn set_widget_factory(&self, key: &str, factory: Option<TuiComponentFactory>, options: ExtensionWidgetOptions);
    fn set_header_factory(&self, factory: Option<TuiComponentFactory>);
    fn set_footer_factory(&self, factory: Option<FooterComponentFactory>);
    fn custom_factory(&self, factory: CustomComponentFactory, options: CustomUiFactoryOptions) -> ExtensionFuture<'_, JsonValue>;
}
#[derive(Clone)]
pub enum WidgetContent { Lines(Vec<String>), Component(ComponentFactory) }
#[derive(Clone, Debug, Default)]
pub struct CustomUiOptions { pub overlay: bool, pub overlay_options: Option<JsonValue> }
pub trait ExtensionUi: Send + Sync {
    fn actions(&self) -> Option<&dyn ExtensionUiActions> { None }
    fn factories(&self) -> Option<&dyn ExtensionUiFactories> { None }
    fn set_widget_factory(&self, key: &str, factory: Option<TuiComponentFactory>, options: ExtensionWidgetOptions) -> Result<(), ExtensionFailure> {
        self.factories().ok_or_else(|| ExtensionFailure::new("Widget factories are not available"))?.set_widget_factory(key, factory, options);
        Ok(())
    }
    fn set_header_factory(&self, factory: Option<TuiComponentFactory>) -> Result<(), ExtensionFailure> {
        self.factories().ok_or_else(|| ExtensionFailure::new("Header factories are not available"))?.set_header_factory(factory);
        Ok(())
    }
    fn set_footer_factory(&self, factory: Option<FooterComponentFactory>) -> Result<(), ExtensionFailure> {
        self.factories().ok_or_else(|| ExtensionFailure::new("Footer factories are not available"))?.set_footer_factory(factory);
        Ok(())
    }
    fn custom_factory(&self, factory: CustomComponentFactory, options: CustomUiFactoryOptions) -> ExtensionFuture<'_, JsonValue> {
        match self.factories() { Some(factories) => factories.custom_factory(factory, options), None => Box::pin(async { Err(ExtensionFailure::new("Custom UI factories are not available")) }) }
    }
    fn question(&self, request: QuestionRequest, options: QuestionOptions) -> ExtensionFuture<'_, QuestionResponse> {
        match self.actions() { Some(actions) => actions.question(request, options), None => Box::pin(async move { Ok(QuestionResponse { status: QuestionStatus::Unavailable, answers: BTreeMap::new(), comment: None, unanswered: request.questions.into_iter().map(|question| question.id).collect(), auto_resolved_after_ms: None }) }) }
    }
    fn on_terminal_input(&self, handler: TerminalInputHandler) -> Result<UiUnsubscribe, ExtensionFailure> { Ok(self.actions().ok_or_else(|| ExtensionFailure::new("Terminal input is not available"))?.on_terminal_input(handler)) }
    fn set_working_message(&self, message: Option<&str>) -> Result<(), ExtensionFailure> { self.actions().ok_or_else(|| ExtensionFailure::new("Working indicator is not available"))?.set_working_message(message); Ok(()) }
    fn set_working_visible(&self, visible: bool) -> Result<(), ExtensionFailure> { self.actions().ok_or_else(|| ExtensionFailure::new("Working indicator is not available"))?.set_working_visible(visible); Ok(()) }
    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>) -> Result<(), ExtensionFailure> { self.actions().ok_or_else(|| ExtensionFailure::new("Working indicator is not available"))?.set_working_indicator(options); Ok(()) }
    fn set_hidden_thinking_label(&self, label: Option<&str>) -> Result<(), ExtensionFailure> { self.actions().ok_or_else(|| ExtensionFailure::new("Thinking label is not available"))?.set_hidden_thinking_label(label); Ok(()) }
    fn editor<'a>(&'a self, title: &'a str, prefill: Option<&'a str>) -> ExtensionFuture<'a, Option<String>> { match self.actions() { Some(actions) => actions.editor(title, prefill), None => Box::pin(async { Err(ExtensionFailure::new("Editor is not available")) }) } }
    fn add_autocomplete_provider(&self, factory: AutocompleteProviderFactory) -> Result<(), ExtensionFailure> { self.actions().ok_or_else(|| ExtensionFailure::new("Autocomplete is not available"))?.add_autocomplete_provider(factory); Ok(()) }
    fn set_editor_component(&self, factory: Option<EditorFactory>) -> Result<(), ExtensionFailure> { self.actions().ok_or_else(|| ExtensionFailure::new("Custom editor is not available"))?.set_editor_component(factory); Ok(()) }
    fn get_editor_component(&self) -> Result<Option<EditorFactory>, ExtensionFailure> { Ok(self.actions().ok_or_else(|| ExtensionFailure::new("Custom editor is not available"))?.get_editor_component()) }
    fn get_all_themes(&self) -> Result<Vec<ThemeInfo>, ExtensionFailure> { Ok(self.actions().ok_or_else(|| ExtensionFailure::new("Theme catalog is not available"))?.get_all_themes()) }
    fn get_theme(&self, name: &str) -> Result<Option<Theme>, ExtensionFailure> { Ok(self.actions().ok_or_else(|| ExtensionFailure::new("Theme catalog is not available"))?.get_theme(name)) }
    fn set_theme(&self, theme: ThemeSelection) -> Result<SetThemeResult, ExtensionFailure> { Ok(self.actions().ok_or_else(|| ExtensionFailure::new("Theme selection is not available"))?.set_theme(theme)) }
    fn get_tools_expanded(&self) -> Result<bool, ExtensionFailure> { Ok(self.actions().ok_or_else(|| ExtensionFailure::new("Tool expansion is not available"))?.get_tools_expanded()) }
    fn set_tools_expanded(&self, expanded: bool) -> Result<(), ExtensionFailure> { self.actions().ok_or_else(|| ExtensionFailure::new("Tool expansion is not available"))?.set_tools_expanded(expanded); Ok(()) }
    fn select<'a>(&'a self, title: &'a str, options: &'a [String], opts: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>>;
    fn confirm<'a>(&'a self, title: &'a str, message: &'a str, opts: ExtensionUiDialogOptions) -> UiFuture<'a, bool>;
    fn input<'a>(&'a self, title: &'a str, placeholder: Option<&'a str>, opts: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>>;
    fn notify(&self, message: &str, kind: NotificationType);
    fn set_status(&self, key: &str, text: Option<&str>);
    fn set_widget(&self, key: &str, content: Option<WidgetContent>, options: ExtensionWidgetOptions);
    fn set_header(&self, factory: Option<ComponentFactory>);
    fn set_footer(&self, factory: Option<ComponentFactory>);
    fn set_title(&self, title: &str);
    fn paste_to_editor(&self, text: &str);
    fn set_editor_text(&self, text: &str);
    fn get_editor_text(&self) -> String;
    fn custom(&self, factory: ComponentFactory, options: CustomUiOptions) -> ExtensionFuture<'_, JsonValue>;
    fn theme(&self) -> Theme;
}

/// Context actions are live host callbacks rather than frozen snapshots.
#[derive(Clone)]
pub struct ExtensionContext {
    pub ui: Arc<dyn ExtensionUi>, pub mode: ExtensionMode, pub has_ui: bool, pub cwd: PathBuf,
    pub agent_dir: PathBuf, pub session_manager: Arc<dyn SessionManager>, pub model_registry: Arc<dyn ModelRegistry>,
    pub model: Option<Model>, pub thinking_level: Option<ThinkingLevel>, pub service_tier: Option<ServiceTier>,
    pub effective_service_tier: Option<ServiceTier>, pub scoped_models: Vec<ScopedModel>, pub goal_store_file: Option<PathBuf>,
    pub loaded_extension_paths: Vec<String>, pub signal: Option<AbortSignal>, pub steering_signal: Option<AbortSignal>,
    pub is_idle_fn: Arc<dyn Fn() -> bool + Send + Sync>,
    pub wait_for_idle_fn: Arc<dyn Fn() -> UiFuture<'static, ()> + Send + Sync>,
    pub is_project_trusted_fn: Arc<dyn Fn() -> bool + Send + Sync>,
    pub is_compacting_fn: Arc<dyn Fn() -> bool + Send + Sync>,
    pub get_system_prompt_fn: Arc<dyn Fn() -> String + Send + Sync>,
    pub get_system_prompt_options_fn: Arc<dyn Fn() -> BuildSystemPromptOptions + Send + Sync>,
    pub registered_mcp_servers: Vec<RegisteredMcpServerDeclaration>,
    pub update_tool_hook_status: Option<ToolHookStatusUpdater>,
}
impl ExtensionContext {
    fn assert_active_or_panic(&self) {
        if let Some(actions) = self.session_manager.extension_context_actions()
            && let Err(error) = actions.assert_active() { std::panic::panic_any(error); }
    }
    pub fn actions(&self) -> Result<&dyn ExtensionContextActions, ExtensionFailure> {
        let actions = self.session_manager.extension_context_actions().ok_or_else(|| ExtensionFailure::new("Extension context actions are not bound"))?;
        actions.assert_active()?; Ok(actions)
    }
    pub fn current_model(&self) -> Result<Option<Model>, ExtensionFailure> {
        Ok(match self.session_manager.extension_context_actions() { Some(actions) => { actions.assert_active()?; actions.get_model() }, None => self.model.clone() })
    }
    pub fn current_service_tier(&self) -> Result<Option<ServiceTier>, ExtensionFailure> {
        Ok(match self.session_manager.extension_context_actions() { Some(actions) => { actions.assert_active()?; actions.get_service_tier() }, None => self.service_tier })
    }
    pub fn current_effective_service_tier(&self) -> Result<Option<ServiceTier>, ExtensionFailure> {
        Ok(match self.session_manager.extension_context_actions() { Some(actions) => { actions.assert_active()?; actions.get_effective_service_tier() }, None => self.effective_service_tier.or(self.service_tier) })
    }
    pub fn current_steering_signal(&self) -> Result<Option<AbortSignal>, ExtensionFailure> {
        Ok(match self.session_manager.extension_context_actions() { Some(actions) => { actions.assert_active()?; actions.get_steering_signal() }, None => self.steering_signal.clone() })
    }
    pub fn current_model_registry(&self) -> Result<Arc<dyn ModelRegistry>, ExtensionFailure> {
        if let Some(actions) = self.session_manager.extension_context_actions() { actions.assert_active()?; }
        Ok(Arc::clone(&self.model_registry))
    }
    pub fn current_ui(&self) -> Result<Arc<dyn ExtensionUi>, ExtensionFailure> {
        if let Some(actions) = self.session_manager.extension_context_actions() { actions.assert_active()?; }
        Ok(Arc::clone(&self.ui))
    }
    pub fn abort(&self, source: Option<AbortSource>) -> Result<(), ExtensionFailure> { self.actions()?.abort(source); Ok(()) }
    pub fn has_pending_messages(&self) -> Result<bool, ExtensionFailure> { Ok(self.actions()?.has_pending_messages()) }
    pub async fn request_reload(&self) -> Result<(), ExtensionFailure> { self.actions()?.request_reload().await }
    pub async fn check_reload_veto(&self) -> Result<ReloadVetoDecision, ExtensionFailure> {
        let result = self.actions()?.check_reload_veto().await?;
        self.actions()?;
        Ok(result)
    }
    pub fn shutdown(&self) -> Result<(), ExtensionFailure> { self.actions()?.shutdown(); Ok(()) }
    pub fn get_context_usage(&self) -> Result<Option<ContextUsage>, ExtensionFailure> { Ok(self.actions()?.get_context_usage()) }
    pub fn get_compaction_settings(&self) -> Result<CompactionSettings, ExtensionFailure> { Ok(self.actions()?.get_compaction_settings()) }
    pub fn get_resolved_compaction_settings(&self) -> Result<Option<ResolvedCompactionSettings>, ExtensionFailure> { Ok(self.actions()?.get_resolved_compaction_settings()) }
    pub fn get_compaction_preparation(&self) -> Result<Option<CompactionPreparationDetails>, ExtensionFailure> { Ok(self.actions()?.get_compaction_preparation()) }
    pub fn get_prompt_cache_safe_wait_seconds(&self) -> Result<Option<f64>, ExtensionFailure> { Ok(self.actions()?.get_prompt_cache_safe_wait_seconds()) }
    pub fn get_prompt_cache_goal_backstop_max_seconds(&self) -> Result<f64, ExtensionFailure> { Ok(self.actions()?.get_prompt_cache_goal_backstop_max_seconds()) }
    pub fn get_prompt_cache_keep_alive_settings(&self) -> Result<PromptCacheKeepAliveSettings, ExtensionFailure> { Ok(self.actions()?.get_prompt_cache_keep_alive_settings()) }
    pub fn get_look_at_settings(&self) -> Result<LookAtSettings, ExtensionFailure> { Ok(self.actions()?.get_look_at_settings()) }
    pub fn get_ask_user_settings(&self) -> Result<AskUserSettings, ExtensionFailure> { Ok(self.actions()?.get_ask_user_settings()) }
    pub fn get_image_settings(&self) -> Result<ImageSettings, ExtensionFailure> { Ok(self.actions()?.get_image_settings()) }
    pub fn session_settings(&self) -> Result<&dyn ExtensionSessionSettings, ExtensionFailure> { Ok(self.actions()?.session_settings()) }
    pub fn compact(&self, options: CompactOptions) -> Result<(), ExtensionFailure> { self.actions()?.compact(options); Ok(()) }
    pub async fn prepare_provider_request(&self, messages: Vec<AgentMessage>) -> Result<ProviderRequestPreparation, ExtensionFailure> {
        let result = self.actions()?.prepare_provider_request(messages).await?;
        self.actions()?;
        Ok(result)
    }
    pub fn begin_compaction(&self, options: BeginCompactionOptions) -> Result<Option<AbortSignal>, ExtensionFailure> { Ok(self.actions()?.begin_compaction(options)) }
    pub fn update_compaction(&self, options: UpdateCompactionOptions) -> Result<(), ExtensionFailure> { self.actions()?.update_compaction(options); Ok(()) }
    pub fn end_compaction(&self, options: EndCompactionOptions) -> Result<(), ExtensionFailure> { self.actions()?.end_compaction(options); Ok(()) }
    pub fn get_message_revision(&self) -> Result<u64, ExtensionFailure> { Ok(self.actions()?.get_message_revision()) }
    pub async fn apply_compaction(&self, result: CompactionResult, options: ApplyCompactionOptions) -> Result<ApplyCompactionResult, ExtensionFailure> {
        let result = self.actions()?.apply_compaction(result, options).await?;
        self.actions()?;
        Ok(result)
    }
    pub fn get_loaded_hook_sources(&self) -> Result<LoadedHookSources, ExtensionFailure> { Ok(self.actions()?.get_loaded_hook_sources()) }
    pub fn kernel_tools(&self) -> Result<Option<&dyn ExtensionKernelTools>, ExtensionFailure> { Ok(self.actions()?.kernel_tools()) }
    pub fn set_approved_monitor_parent(&self, tool_call_id: &str, input: &JsonValue, parent: &Path) -> Result<(), ExtensionFailure> {
        self.actions()?.set_approved_monitor_parent(tool_call_id, input, parent)
    }
    pub fn take_approved_monitor_parent(&self, tool_call_id: &str, input: &JsonValue) -> Result<Option<PathBuf>, ExtensionFailure> {
        self.actions()?.take_approved_monitor_parent(tool_call_id, input)
    }
    pub fn is_idle(&self) -> bool { self.assert_active_or_panic(); self.session_manager.extension_context_actions().map_or_else(|| (self.is_idle_fn)(), ExtensionContextActions::is_idle) }
    pub async fn wait_for_idle(&self) { self.assert_active_or_panic(); (self.wait_for_idle_fn)().await; self.assert_active_or_panic(); }
    pub fn is_project_trusted(&self) -> bool { self.assert_active_or_panic(); self.session_manager.extension_context_actions().map_or_else(|| (self.is_project_trusted_fn)(), ExtensionContextActions::is_project_trusted) }
    pub fn is_compacting(&self) -> bool { self.assert_active_or_panic(); self.session_manager.extension_context_actions().map_or_else(|| (self.is_compacting_fn)(), ExtensionContextActions::is_compacting) }
    pub fn get_system_prompt(&self) -> String { self.assert_active_or_panic(); (self.get_system_prompt_fn)() }
    pub fn get_system_prompt_options(&self) -> BuildSystemPromptOptions { self.assert_active_or_panic(); (self.get_system_prompt_options_fn)() }
    pub fn get_registered_mcp_servers(&self) -> Vec<RegisteredMcpServerDeclaration> {
        self.assert_active_or_panic();
        self.session_manager.extension_context_actions().and_then(ExtensionContextActions::get_registered_mcp_servers).unwrap_or_else(|| self.registered_mcp_servers.clone())
    }
}
impl ToolContext for ExtensionContext {
    fn cwd(&self) -> &Path { self.assert_active_or_panic(); &self.cwd }
    fn model(&self) -> Option<&Model> { self.assert_active_or_panic(); self.model.as_ref() }
    fn thinking_level(&self) -> Option<ThinkingLevel> { self.assert_active_or_panic(); self.thinking_level }
    fn session_manager(&self) -> &dyn ToolSessionManager { self.assert_active_or_panic(); self.session_manager.as_ref() }
    fn goal_store_file(&self) -> Option<&Path> { self.assert_active_or_panic(); self.goal_store_file.as_deref() }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BuildSystemPromptOptions { pub cwd: PathBuf, pub custom_prompt: Option<String>, pub append_system_prompt: Option<String>, pub tools: Vec<String>, pub skills: Vec<Skill>, pub context_files: Vec<ContextFile> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextFile { pub path: String, pub content: String }
#[derive(Clone, Debug, PartialEq)]
pub struct CustomMessage { pub custom_type: String, pub content: Vec<ToolContent>, pub display: bool, pub details: Option<JsonValue> }
#[derive(Clone, Debug)]
pub struct BeforeAgentStartEvent { pub prompt: String, pub images: Option<Vec<ImageContent>>, pub system_prompt: String, pub system_prompt_options: BuildSystemPromptOptions }
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BeforeAgentStartEventResult { pub message: Option<CustomMessage>, pub system_prompt: Option<String> }
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BeforeAgentStartCombinedResult { pub messages: Vec<CustomMessage>, pub system_prompt: Option<String> }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputSource { Interactive, Rpc, Extension }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamingBehavior { Steer, FollowUp }
#[derive(Clone, Debug)]
pub struct InputEvent { pub input_id: String, pub text: String, pub images: Option<Vec<ImageContent>>, pub source: InputSource, pub streaming_behavior: Option<StreamingBehavior> }
#[derive(Clone, Debug, PartialEq)]
pub enum InputEventResult { Continue, Transform { text: String, images: Option<Vec<ImageContent>> }, Handled }
#[derive(Clone, Debug)]
pub struct ToolCallEvent { pub tool_call_id: String, pub tool_name: String, pub input: JsonValue }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolCallEventResult { pub block: Option<bool>, pub reason: Option<String>, pub terminate: Option<bool> }
#[derive(Clone, Debug)]
pub struct ToolResultEvent { pub tool_call_id: String, pub tool_name: String, pub input: JsonValue, pub content: Vec<ToolContent>, pub details: Option<JsonValue>, pub is_error: bool, pub usage: Option<Usage> }
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolResultEventResult { pub content: Option<Vec<ToolContent>>, pub details: Option<JsonValue>, pub is_error: Option<bool>, pub usage: Option<Usage> }
#[derive(Clone, Debug)]
pub struct ModelSelectEvent { pub model: Model, pub previous_model: Option<Model>, pub source: ModelSelectSource, pub system_prompt: String, pub system_prompt_options: BuildSystemPromptOptions }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelSelectSource { Set, Cycle, Restore, Fallback, FallbackRevert }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelSelectEventResult { pub system_prompt: Option<Option<String>>, pub system_prompt_name: Option<String> }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustDecision { Yes, No, Undecided }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectTrustEventResult { pub trusted: TrustDecision, pub remember: Option<bool> }
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionBeforeEventResult { pub cancel: Option<bool>, pub reason: Option<String>, pub skip_conversation_restore: Option<bool>, pub compaction: Option<CompactionResult>, pub rejection_cause: Option<CompactionRejectionCause>, pub summary: Option<JsonValue>, pub custom_instructions: Option<String>, pub replace_instructions: Option<bool>, pub label: Option<String> }
pub type SessionBeforeSwitchResult = SessionBeforeEventResult;
pub type SessionBeforeForkResult = SessionBeforeEventResult;
pub type SessionBeforeReloadResult = SessionBeforeEventResult;
pub type SessionBeforeCompactResult = SessionBeforeEventResult;
pub type SessionBeforeTreeResult = SessionBeforeEventResult;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionReason { Manual, Threshold, Overflow, PrePrompt, Branch, Extension }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionRejectionCause { CancelledByExtension, ExternalOwner, WouldOverflow, CircuitBreaker, PerTurnCap, StaleRevision }
#[derive(Clone, Debug, PartialEq)]
pub struct CompactionResult { pub summary: String, pub first_kept_entry_id: String, pub tokens_before: u64, pub details: Option<JsonValue> }
#[derive(Clone, Debug, PartialEq)]
pub struct CompactionPreparation { pub settings: CompactionSettings, pub messages_to_summarize: Vec<AgentMessage>, pub turn_prefix_messages: Vec<AgentMessage>, pub tokens_before: u64, pub first_kept_entry_id: String, pub previous_summary: Option<String> }
/// Complete preparation data for session_before_compact, exposed additively so
/// existing event and preparation constructors remain source-compatible.
#[derive(Clone, Debug, PartialEq)]
pub struct CompactionPreparationDetails {
    pub preparation: CompactionPreparation,
    pub source_messages: Option<Vec<AgentMessage>>,
    pub turn_prefix_source_messages: Option<Vec<AgentMessage>>,
    pub is_split_turn: bool,
    pub file_ops: CompactionFileOperations,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompactionFileOperations { pub read: Vec<String>, pub written: Vec<String>, pub edited: Vec<String> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactionSettings { pub enabled: bool, pub reserve_tokens: u64, pub keep_recent_tokens: u64 }
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedCompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: u64,
    pub keep_recent_tokens: u64,
    pub speculative_enabled: bool,
    pub speculative_fraction: f64,
    pub speculative_cooldown_ms: f64,
    pub restoration_enabled: bool,
    pub restoration_max_items: f64,
    pub restoration_max_tokens_per_item: f64,
    pub restoration_max_total_tokens: f64,
    pub restoration_context_ratio: f64,
    pub idle_compaction_enabled: bool,
    pub grace_band_enabled: bool,
    pub tool_admission_enabled: bool,
    pub reminder_enabled: bool,
    pub reserve_scaling_enabled: bool,
    pub speculative_lead_tokens: Option<f64>,
    pub summarization_max_duration_ms: Option<f64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionReason { Startup, Reload, New, Resume, Fork, Quit }
#[derive(Clone, Debug)]
pub struct SessionStartEvent { pub reason: SessionReason, pub initial_model_provenance: Option<String>, pub previous_session_file: Option<String> }
#[derive(Clone, Debug)]
pub struct SessionShutdownEvent { pub reason: SessionReason, pub target_session_file: Option<String>, pub signal: Option<AbortSignal> }
#[derive(Clone, Debug)]
pub struct ResourcesDiscoverEvent { pub cwd: PathBuf, pub reason: SessionReason, pub scoped_entries: bool }
#[derive(Clone, Debug)]
pub struct SessionBeforeCompactEvent { pub reason: CompactionReason, pub will_retry: bool, pub request_id: String, pub preparation: CompactionPreparation, pub branch_entries: Vec<SessionEntry>, pub custom_instructions: Option<String>, pub signal: AbortSignal }
#[derive(Clone, Debug)]
pub enum SessionCompactEvent {
    Accepted { reason: CompactionReason, request_id: String, compaction_entry: SessionEntry, from_extension: bool, will_retry: bool },
    Rejected { reason: CompactionReason, request_id: String, rejection_cause: CompactionRejectionCause },
}

/// Every event accepted by types.ts has a native discriminant. JSON is retained only for
/// payloads whose upstream contracts are open records or owned by a subsequent session port.
#[derive(Clone, Debug)]
pub enum ExtensionEvent {
    ProjectTrust { cwd: PathBuf }, ResourcesDiscover(ResourcesDiscoverEvent), SessionStart(SessionStartEvent),
    SessionInfoChanged { name: Option<String> }, SessionParked, SessionResumed,
    SessionBeforeSwitch { reason: SessionReason, target_session_file: Option<String> },
    SessionBeforeFork { entry_id: String, position: ForkPosition }, SessionBeforeReload,
    SessionBeforeCompact(SessionBeforeCompactEvent), SessionCompact(SessionCompactEvent),
    SessionCompactFailed { reason: CompactionReason, error_message: Option<String>, aborted: bool, will_retry: bool, from_extension: bool },
    SessionShutdown(SessionShutdownEvent), SessionAbort,
    SessionExtensionsRemoved { reason: SessionReason, removed: Vec<ExtensionIdentity> },
    SessionBeforeTree { preparation: TreePreparation, signal: AbortSignal },
    SessionTree { new_leaf_id: Option<String>, old_leaf_id: Option<String>, summary_entry: Option<SessionEntry>, from_extension: Option<bool> },
    Context { messages: Vec<AgentMessage> }, BeforeProviderRequest { payload: JsonValue, model: Option<Model>, headers: Option<BTreeMap<String, Option<String>>> },
    BeforeProviderHeaders { headers: BTreeMap<String, Option<String>> }, AfterProviderResponse { status: u16, headers: BTreeMap<String, String> },
    BeforeAgentStart(BeforeAgentStartEvent), AgentStart,
    AgentEnd { messages: Vec<AgentMessage>, aborted: Option<bool>, will_retry: Option<bool>, abort_source: Option<AbortSource> }, AgentSettled,
    UiPromptStart { kind: UiPromptKind, title: Option<String> }, UiPromptEnd { kind: UiPromptKind, title: Option<String> },
    TurnStart { turn_index: u64, timestamp: u64 }, TurnEnd { turn_index: u64, message: AgentMessage, tool_results: Vec<maho_ai::types::ToolResultMessage> },
    MessageStart { message: AgentMessage }, MessageUpdate { message: AgentMessage, assistant_message_event: JsonValue }, MessageEnd { message: AgentMessage },
    ToolExecutionStart { tool_call_id: String, tool_name: String, args: JsonValue },
    ToolExecutionUpdate { tool_call_id: String, tool_name: String, args: JsonValue, partial_result: JsonValue },
    ToolExecutionEnd { tool_call_id: String, tool_name: String, result: JsonValue, is_error: bool },
    ModelSelect(ModelSelectEvent), SystemPromptChange { system_prompt: String, previous_system_prompt: String, system_prompt_name: Option<String>, model: Model, previous_model: Option<Model> },
    ThinkingLevelSelect { level: ThinkingLevel, previous_level: ThinkingLevel }, ToolCall(ToolCallEvent), ToolResult(ToolResultEvent),
    UserBash { command: String, exclude_from_context: bool, cwd: PathBuf }, Input(InputEvent), InputDisposition { input_id: String, disposition: InputDisposition },
}
impl ExtensionEvent {
    pub const fn kind(&self) -> EventKind {
        match self {
            Self::ProjectTrust { .. } => EventKind::ProjectTrust, Self::ResourcesDiscover(_) => EventKind::ResourcesDiscover,
            Self::SessionStart(_) => EventKind::SessionStart, Self::SessionInfoChanged { .. } => EventKind::SessionInfoChanged,
            Self::SessionParked => EventKind::SessionParked, Self::SessionResumed => EventKind::SessionResumed,
            Self::SessionBeforeSwitch { .. } => EventKind::SessionBeforeSwitch, Self::SessionBeforeFork { .. } => EventKind::SessionBeforeFork,
            Self::SessionBeforeReload => EventKind::SessionBeforeReload, Self::SessionBeforeCompact(_) => EventKind::SessionBeforeCompact,
            Self::SessionCompact(_) => EventKind::SessionCompact, Self::SessionCompactFailed { .. } => EventKind::SessionCompactFailed,
            Self::SessionShutdown(_) => EventKind::SessionShutdown, Self::SessionAbort => EventKind::SessionAbort,
            Self::SessionExtensionsRemoved { .. } => EventKind::SessionExtensionsRemoved, Self::SessionBeforeTree { .. } => EventKind::SessionBeforeTree,
            Self::SessionTree { .. } => EventKind::SessionTree, Self::Context { .. } => EventKind::Context,
            Self::BeforeProviderRequest { .. } => EventKind::BeforeProviderRequest, Self::BeforeProviderHeaders { .. } => EventKind::BeforeProviderHeaders,
            Self::AfterProviderResponse { .. } => EventKind::AfterProviderResponse, Self::BeforeAgentStart(_) => EventKind::BeforeAgentStart,
            Self::AgentStart => EventKind::AgentStart, Self::AgentEnd { .. } => EventKind::AgentEnd, Self::AgentSettled => EventKind::AgentSettled,
            Self::UiPromptStart { .. } => EventKind::UiPromptStart, Self::UiPromptEnd { .. } => EventKind::UiPromptEnd,
            Self::TurnStart { .. } => EventKind::TurnStart, Self::TurnEnd { .. } => EventKind::TurnEnd,
            Self::MessageStart { .. } => EventKind::MessageStart, Self::MessageUpdate { .. } => EventKind::MessageUpdate, Self::MessageEnd { .. } => EventKind::MessageEnd,
            Self::ToolExecutionStart { .. } => EventKind::ToolExecutionStart, Self::ToolExecutionUpdate { .. } => EventKind::ToolExecutionUpdate,
            Self::ToolExecutionEnd { .. } => EventKind::ToolExecutionEnd, Self::ModelSelect(_) => EventKind::ModelSelect,
            Self::SystemPromptChange { .. } => EventKind::SystemPromptChange, Self::ThinkingLevelSelect { .. } => EventKind::ThinkingLevelSelect,
            Self::ToolCall(_) => EventKind::ToolCall, Self::ToolResult(_) => EventKind::ToolResult, Self::UserBash { .. } => EventKind::UserBash,
            Self::Input(_) => EventKind::Input, Self::InputDisposition { .. } => EventKind::InputDisposition,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkPosition { Before, At }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbortSource { User, System, Provider }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiPromptKind { Select, Confirm, Input, Editor, Custom, Question }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiPromptReason { UiPrompt }
impl UiPromptReason { pub const fn as_str(self) -> &'static str { "ui_prompt" } }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemPromptChangeSource { ModelSelect }
impl SystemPromptChangeSource { pub const fn as_str(self) -> &'static str { "model_select" } }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiPromptStartEvent { pub reason: UiPromptReason, pub kind: UiPromptKind, pub title: Option<String> }
pub type UiPromptEndEvent = UiPromptStartEvent;
#[derive(Clone, Debug, PartialEq)]
pub struct SystemPromptChangeEvent {
    pub system_prompt: String, pub previous_system_prompt: String, pub system_prompt_name: Option<String>,
    pub model: Model, pub previous_model: Option<Model>, pub source: SystemPromptChangeSource,
}
impl ExtensionEvent {
    pub fn ui_prompt_start_event(&self) -> Option<UiPromptStartEvent> {
        match self { Self::UiPromptStart { kind, title } => Some(UiPromptStartEvent { reason: UiPromptReason::UiPrompt, kind: *kind, title: title.clone() }), _ => None }
    }
    pub fn ui_prompt_end_event(&self) -> Option<UiPromptEndEvent> {
        match self { Self::UiPromptEnd { kind, title } => Some(UiPromptEndEvent { reason: UiPromptReason::UiPrompt, kind: *kind, title: title.clone() }), _ => None }
    }
    pub fn system_prompt_change_event(&self) -> Option<SystemPromptChangeEvent> {
        match self {
            Self::SystemPromptChange { system_prompt, previous_system_prompt, system_prompt_name, model, previous_model } => Some(SystemPromptChangeEvent {
                system_prompt: system_prompt.clone(), previous_system_prompt: previous_system_prompt.clone(), system_prompt_name: system_prompt_name.clone(),
                model: model.clone(), previous_model: previous_model.clone(), source: SystemPromptChangeSource::ModelSelect,
            }), _ => None,
        }
    }
    pub const fn ui_prompt_reason(&self) -> Option<UiPromptReason> {
        match self { Self::UiPromptStart { .. } | Self::UiPromptEnd { .. } => Some(UiPromptReason::UiPrompt), _ => None }
    }
    pub const fn system_prompt_change_source(&self) -> Option<SystemPromptChangeSource> {
        match self { Self::SystemPromptChange { .. } => Some(SystemPromptChangeSource::ModelSelect), _ => None }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputDisposition { Handled, Queued, Started, Rejected }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionIdentity { pub path: String, pub resolved_path: String }
#[derive(Clone, Debug)]
pub struct TreePreparation { pub target_id: String, pub old_leaf_id: Option<String>, pub common_ancestor_id: Option<String>, pub entries_to_summarize: Vec<SessionEntry>, pub user_wants_summary: bool, pub custom_instructions: Option<String>, pub replace_instructions: Option<bool>, pub label: Option<String> }
pub enum EventResult {
    None, ProjectTrust(ProjectTrustEventResult), ResourcesDiscover(ResourcesDiscoverResult), SessionBefore(SessionBeforeEventResult),
    Context { messages: Option<Vec<AgentMessage>> }, ProviderPayload(JsonValue), BeforeAgentStart(BeforeAgentStartEventResult),
    ModelSelect(ModelSelectEventResult), MessageEnd { message: Option<AgentMessage> }, ToolCall(ToolCallEventResult),
    ToolResult(ToolResultEventResult), UserBash { operations: Option<Arc<dyn maho_tools::bash::BashOperations>>, result: Option<maho_tools::bash_executor::BashResult> }, Input(InputEventResult),
}
pub type ExtensionHandler = Arc<dyn for<'a> Fn(&'a mut ExtensionEvent, &'a ExtensionContext) -> ExtensionFuture<'a, EventResult> + Send + Sync>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpServerDeclaration {
    pub transport: Option<McpTransport>, pub url: Option<String>, pub command: Option<String>, pub args: Option<Vec<String>>,
    pub env: Option<BTreeMap<String, String>>, pub cwd: Option<String>, pub headers: Option<BTreeMap<String, String>>,
    pub auth: Option<McpAuth>, pub bearer_token_env: Option<String>, pub oauth: Option<McpOAuth>, pub enabled: Option<bool>,
    pub lifecycle: Option<McpLifecycle>, pub idle_timeout_min: Option<f64>, pub request_timeout_ms: Option<f64>,
    pub connect_timeout_ms: Option<f64>, pub startup_timeout_ms: Option<f64>, pub include_tools: Option<Vec<String>>,
    pub exclude_tools: Option<Vec<String>>, pub direct_tools: Option<McpDirectTools>, pub exposure: Option<McpExposure>, pub log_level: Option<McpLogLevel>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpTransport { Stdio, Http }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpAuth { Bearer, OAuth, Disabled }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpLifecycle { Lazy, Eager, KeepAlive }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpExposure { Auto, Direct, Search, Proxy }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpLogLevel { Debug, Info, Notice, Warning, Error, Critical, Alert, Emergency }
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpDirectTools { Enabled(bool), Names(Vec<String>) }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthFlow { Code, ClientCredentials }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpOAuth { pub client_id: Option<String>, pub callback_port: Option<u16>, pub scopes: Option<Vec<String>>, pub client_metadata_url: Option<String>, pub flow: Option<OAuthFlow> }
#[derive(Clone, Debug, PartialEq)]
pub struct RegisteredMcpServerDeclaration { pub name: String, pub config: McpServerDeclaration, pub extension_path: String, pub registration_cwd: PathBuf }
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlagValue { Boolean(bool), String(String) }
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlagType { Boolean { default: Option<bool> }, String { default: Option<String> } }
#[derive(Clone, Debug)]
pub struct ExtensionFlag { pub name: String, pub description: Option<String>, pub kind: FlagType, pub extension_path: String }
pub type CommandHandler = Arc<dyn for<'a> Fn(&'a str, &'a ExtensionContext) -> ExtensionFuture<'a, ()> + Send + Sync>;
pub type CommandArgumentCompletions = Arc<dyn for<'a> Fn(&'a str) -> ExtensionFuture<'a, Option<Vec<maho_tui::autocomplete::AutocompleteItem>>> + Send + Sync>;
pub type CommandContextHandler = Arc<dyn for<'a> Fn(&'a str, &'a ExtensionCommandContext) -> ExtensionFuture<'a, ()> + Send + Sync>;
#[derive(Clone, Debug, Default)]
pub struct ExtensionTreeNavigationOptions { pub summarize: Option<bool>, pub custom_instructions: Option<String>, pub replace_instructions: Option<bool>, pub label: Option<String>, pub expected_leaf_id: Option<String> }
#[derive(Clone, Debug, Default)]
pub struct EditMessageOptions { pub summarize: Option<bool>, pub custom_instructions: Option<String>, pub expected_leaf_id: Option<String> }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionNavigationResult { pub cancelled: bool }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditMessageResult { pub cancelled: bool, pub unchanged: Option<bool>, pub entry_id: Option<String> }
pub type WithSession = Arc<dyn for<'a> Fn(&'a ReplacedSessionContext) -> ExtensionFuture<'a, ()> + Send + Sync>;
pub type SessionSetup = Arc<dyn for<'a> Fn(&'a dyn SessionManager) -> ExtensionFuture<'a, ()> + Send + Sync>;
#[derive(Clone, Default)]
pub struct NewSessionOptions { pub parent_session: Option<String>, pub setup: Option<SessionSetup>, pub with_session: Option<WithSession> }
#[derive(Clone, Default)]
pub struct ForkOptions { pub position: Option<ForkPosition>, pub with_session: Option<WithSession> }
#[derive(Clone, Default)]
pub struct SwitchSessionOptions { pub with_session: Option<WithSession> }
pub trait ExtensionCommandContextActions: Send + Sync {
    fn wait_for_idle(&self) -> ExtensionFuture<'_, ()>;
    fn new_session(&self, options: NewSessionOptions) -> ExtensionFuture<'_, SessionNavigationResult>;
    fn fork<'a>(&'a self, entry_id: &'a str, options: ForkOptions) -> ExtensionFuture<'a, SessionNavigationResult>;
    fn navigate_tree<'a>(&'a self, target_id: &'a str, options: ExtensionTreeNavigationOptions) -> ExtensionFuture<'a, SessionNavigationResult>;
    fn edit_assistant_message<'a>(&'a self, entry_id: &'a str, text: &'a str, options: EditMessageOptions) -> ExtensionFuture<'a, EditMessageResult>;
    fn edit_user_message<'a>(&'a self, entry_id: &'a str, text: &'a str, options: EditMessageOptions) -> ExtensionFuture<'a, EditMessageResult>;
    fn switch_session<'a>(&'a self, path: &'a str, options: SwitchSessionOptions) -> ExtensionFuture<'a, SessionNavigationResult>;
    fn reload(&self) -> ExtensionFuture<'_, ()>;
}
#[derive(Clone)]
pub struct ExtensionCommandContext { pub context: ExtensionContext, pub actions: Arc<dyn ExtensionCommandContextActions>, pub runtime: ExtensionRuntime }
impl std::ops::Deref for ExtensionCommandContext { type Target = ExtensionContext; fn deref(&self) -> &Self::Target { &self.context } }
impl ExtensionCommandContext {
    pub async fn wait_for_idle(&self) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        self.actions.wait_for_idle().await?;
        self.runtime.assert_active()
    }
    pub async fn new_session(&self, options: NewSessionOptions) -> Result<SessionNavigationResult, ExtensionFailure> { self.runtime.assert_active()?; self.actions.new_session(options).await }
    pub async fn fork(&self, entry_id: &str, options: ForkOptions) -> Result<SessionNavigationResult, ExtensionFailure> { self.runtime.assert_active()?; self.actions.fork(entry_id, options).await }
    pub async fn navigate_tree(&self, target_id: &str, options: ExtensionTreeNavigationOptions) -> Result<SessionNavigationResult, ExtensionFailure> {
        self.runtime.assert_active()?;
        let result = self.actions.navigate_tree(target_id, options).await?;
        self.runtime.assert_active()?;
        Ok(result)
    }
    pub async fn edit_assistant_message(&self, entry_id: &str, text: &str, options: EditMessageOptions) -> Result<EditMessageResult, ExtensionFailure> {
        self.runtime.assert_active()?;
        let result = self.actions.edit_assistant_message(entry_id, text, options).await?;
        self.runtime.assert_active()?;
        Ok(result)
    }
    pub async fn edit_user_message(&self, entry_id: &str, text: &str, options: EditMessageOptions) -> Result<EditMessageResult, ExtensionFailure> {
        self.runtime.assert_active()?;
        let result = self.actions.edit_user_message(entry_id, text, options).await?;
        self.runtime.assert_active()?;
        Ok(result)
    }
    pub async fn switch_session(&self, path: &str, options: SwitchSessionOptions) -> Result<SessionNavigationResult, ExtensionFailure> { self.runtime.assert_active()?; self.actions.switch_session(path, options).await }
    pub async fn reload(&self) -> Result<(), ExtensionFailure> { self.runtime.assert_active()?; self.actions.reload().await }
}
#[derive(Clone)]
pub struct ReplacedSessionContext { pub context: ExtensionCommandContext, pub message_actions: Arc<dyn ExtensionActions> }
impl std::ops::Deref for ReplacedSessionContext { type Target = ExtensionCommandContext; fn deref(&self) -> &Self::Target { &self.context } }
impl ReplacedSessionContext {
    pub async fn send_message(&self, message: CustomMessage, options: SendMessageOptions) -> Result<(), ExtensionFailure> { self.context.runtime.assert_active()?; self.message_actions.send_message(message, options) }
    pub async fn send_user_message(&self, content: UserMessageContent, options: SendUserMessageOptions) -> Result<(), ExtensionFailure> { self.context.runtime.assert_active()?; self.message_actions.send_user_message(content, options) }
}
#[derive(Clone)]
pub struct RegisteredCommand { pub name: String, pub source_info: SourceInfo, pub description: Option<String>, pub argument_hint: Option<String>, pub handler: CommandHandler }
#[derive(Clone)]
pub struct ResolvedCommand { pub command: RegisteredCommand, pub invocation_name: String }
#[derive(Clone)]
pub struct RegisteredTool { pub definition: ToolDefinition, pub source_info: SourceInfo }
#[derive(Clone, Debug)]
pub struct ToolInfo { pub name: String, pub label: String, pub description: String, pub parameters: JsonValue, pub prompt_guidelines: Option<Vec<String>>, pub source_info: SourceInfo, pub exposure: ToolExposure, pub search_text: Option<String>, pub search_keywords: Vec<String>, pub search_group: Option<String>, pub allow_lazy_activation: bool }
pub fn normalize_tool_exposure(tool: &ToolDefinition, source_info: SourceInfo) -> ToolInfo {
    let exposure = tool.exposure.unwrap_or(ToolExposure::Direct);
    ToolInfo { name: tool.name.clone(), label: tool.label.clone(), description: tool.description.clone(), parameters: tool.parameters.clone(),
        prompt_guidelines: tool.prompt_guidelines.clone(), source_info, exposure,
        search_text: if exposure == ToolExposure::Search { tool.search_text.clone() } else { None },
        search_keywords: tool.search_keywords.clone().unwrap_or_default(), search_group: tool.search_group.clone(), allow_lazy_activation: tool.allow_lazy_activation != Some(false) }
}
#[derive(Clone, Debug, Default)]
pub struct MessageRenderOptions { pub expanded: bool, pub output_pad: usize }
#[derive(Clone, Debug, Default)]
pub struct EntryRenderOptions { pub expanded: bool }
pub type MessageRenderer = Arc<dyn Fn(&CustomMessage, &MessageRenderOptions, &Theme) -> Option<Box<dyn Component>> + Send + Sync>;
pub type EntryRenderer = Arc<dyn Fn(&SessionEntry, &EntryRenderOptions, &Theme) -> Option<Box<dyn Component>> + Send + Sync>;
pub type EntryReplaces = Arc<dyn Fn(&SessionEntry, &SessionEntry) -> bool + Send + Sync>;
#[derive(Clone, Default)]
pub struct EntryRendererOptions { pub replaces: Option<EntryReplaces> }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SessionKind { #[default] Interactive, Worker }
pub type SessionContext = BTreeMap<String, String>;
pub const EMPTY_SESSION_CONTEXT: SessionContext = BTreeMap::new();
pub const DEFAULT_EXTENSION_SESSION_PROFILE: ExtensionSessionProfile = ExtensionSessionProfile {
    shared_host_enabled: false, session_kind: SessionKind::Interactive, session_context: EMPTY_SESSION_CONTEXT,
};
#[derive(Clone, Debug, Default)]
pub struct ExtensionSessionProfile { pub shared_host_enabled: bool, pub session_kind: SessionKind, pub session_context: BTreeMap<String, String> }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliverAs { Steer, FollowUp, NextTurn }
#[derive(Clone, Debug, Default)]
pub struct SendMessageOptions { pub trigger_turn: bool, pub deliver_as: Option<DeliverAs> }
#[derive(Clone, Debug, Default)]
pub struct SendUserMessageOptions { pub deliver_as: Option<StreamingBehavior>, pub expand_prompt_templates: bool }
#[derive(Clone, Debug)]
pub enum UserMessageContent { Text(String), Blocks(Vec<ToolContent>) }
pub trait ExtensionActions: Send + Sync {
    fn send_message(&self, message: CustomMessage, options: SendMessageOptions) -> Result<(), ExtensionFailure>;
    fn send_user_message(&self, content: UserMessageContent, options: SendUserMessageOptions) -> Result<(), ExtensionFailure>;
    fn append_entry(&self, custom_type: &str, data: Option<JsonValue>) -> Result<(), ExtensionFailure>;
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure>;
}

#[derive(Clone, Default)]
pub struct ExecuteToolOptions {
    pub signal: Option<maho_ai::utils::abort::AbortSignal>,
    pub on_update: Option<maho_agent::types::AgentToolUpdateCallback>,
    pub activate_inactive_tool: Option<bool>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecuteToolErrorCode { UnknownTool, InactiveTool, InvalidParams, Blocked }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecuteToolError { pub code: ExecuteToolErrorCode, pub tool_name: String, pub message: String, pub active_tools: Vec<String> }
impl fmt::Display for ExecuteToolError { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.message) } }
impl std::error::Error for ExecuteToolError {}
pub type ExecuteToolFuture<'a> = Pin<Box<dyn Future<Output = Result<maho_agent::types::AgentToolResult, ExecuteToolError>> + Send + 'a>>;
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SlashCommandInfo { pub name: String, pub description: Option<String>, pub argument_hint: Option<String>, pub source_info: Option<SourceInfo> }
#[derive(Clone, Debug, Default)]
pub struct ExecOptions { pub signal: Option<AbortSignal>, pub timeout_ms: Option<u64>, pub cwd: Option<PathBuf> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecResult { pub stdout: String, pub stderr: String, pub code: i32, pub killed: bool }

/// Additive session actions keep the original ExtensionActions implementors source-compatible.
/// Hosts bind these separately; an unbound capability fails explicitly, never silently succeeds.
pub trait ExtensionSessionActions: Send + Sync {
    fn set_session_name(&self, name: &str) -> Result<(), ExtensionFailure>;
    fn get_session_name(&self) -> Result<Option<String>, ExtensionFailure>;
    fn set_label(&self, entry_id: &str, label: Option<&str>) -> Result<(), ExtensionFailure>;
    fn execute_tool<'a>(&'a self, name: &'a str, params: JsonValue, options: ExecuteToolOptions) -> ExecuteToolFuture<'a>;
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure>;
    fn set_active_tools(&self, names: Vec<String>) -> Result<(), ExtensionFailure>;
    fn refresh_tools(&self) -> Result<(), ExtensionFailure>;
    fn install_registered_tool(&self, _tool: RegisteredTool) -> Result<(), ExtensionFailure> { self.refresh_tools() }
    fn register_removed_tool_hint(&self, name: &str, hint: &str) -> Result<(), ExtensionFailure>;
    fn register_lazy_tool_activator(&self, activator: LazyToolActivator) -> Result<(), ExtensionFailure>;
    fn get_commands(&self) -> Result<Vec<SlashCommandInfo>, ExtensionFailure>;
    fn set_model(&self, model: Model) -> ExtensionFuture<'_, bool>;
    fn get_thinking_level(&self) -> Result<ThinkingLevel, ExtensionFailure>;
    fn set_thinking_level(&self, level: ThinkingLevel) -> Result<(), ExtensionFailure>;
    fn set_session_model(&self, model: Model) -> ExtensionFuture<'_, bool>;
    fn set_session_thinking_level(&self, level: ThinkingLevel) -> Result<(), ExtensionFailure>;
    fn set_session_fast_mode(&self, enabled: bool) -> Result<(), ExtensionFailure>;
    fn exec<'a>(&'a self, command: &'a str, args: &'a [String], cwd: &'a Path, options: ExecOptions) -> ExtensionFuture<'a, ExecResult>;
}

pub type BusHandler = Arc<dyn Fn(&JsonValue) + Send + Sync>;
type NativeBusHandler = Arc<dyn Fn(&(dyn std::any::Any + Send + Sync)) + Send + Sync>;
#[derive(Clone, Default)]
struct BusState { next_id: u64, handlers: BTreeMap<String, Vec<(u64, BusHandler)>>, native_handlers: BTreeMap<String, Vec<(u64, NativeBusHandler)>> }
#[derive(Clone, Default)]
pub struct EventBus { state: Arc<Mutex<BusState>>, registration_stale: Arc<std::sync::atomic::AtomicBool>, runtime: Option<ExtensionRuntime>, registration_subscriptions: Arc<Mutex<Vec<u64>>> }
pub struct BusSubscription { state: Arc<Mutex<BusState>>, channel: String, id: u64 }
impl Drop for BusSubscription {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(handlers) = state.handlers.get_mut(&self.channel) { handlers.retain(|(id, _)| *id != self.id); }
        if let Some(handlers) = state.native_handlers.get_mut(&self.channel) { handlers.retain(|(id, _)| *id != self.id); }
    }
}
impl EventBus {
    pub fn registration_scope(&self) -> Self {
        Self { state: self.state.clone(), registration_stale: Arc::new(std::sync::atomic::AtomicBool::new(false)), runtime: self.runtime.clone(), registration_subscriptions: Arc::default() }
    }
    pub fn bind_runtime(&mut self, runtime: ExtensionRuntime) { self.runtime = Some(runtime); }
    fn assert_active_or_panic(&self) {
        if self.registration_stale.load(std::sync::atomic::Ordering::Acquire) { std::panic::panic_any(ExtensionFailure::new("Extension factory failed to load")); }
        if let Some(runtime) = &self.runtime { runtime.assert_active_or_panic(); }
    }
    pub fn invalidate_registration(&self) {
        self.registration_stale.store(true, std::sync::atomic::Ordering::Release);
        let owned = std::mem::take(&mut *self.registration_subscriptions.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for handlers in state.handlers.values_mut() { handlers.retain(|(id, _)| !owned.contains(id)); }
        for handlers in state.native_handlers.values_mut() { handlers.retain(|(id, _)| !owned.contains(id)); }
    }
    pub fn registration_checkpoint(&self) -> EventBusCheckpoint {
        EventBusCheckpoint(self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone())
    }
    pub fn rollback_registration(&self, checkpoint: EventBusCheckpoint) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let next_id = state.next_id;
        *state = checkpoint.0;
        state.next_id = next_id;
    }
    pub fn on(&self, channel: &str, handler: BusHandler) -> BusSubscription {
        self.assert_active_or_panic();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = state.next_id; state.next_id = state.next_id.wrapping_add(1);
        state.handlers.entry(channel.into()).or_default().push((id, handler));
        self.registration_subscriptions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(id);
        BusSubscription { state: Arc::clone(&self.state), channel: channel.into(), id }
    }
    pub fn emit(&self, channel: &str, data: &JsonValue) {
        self.assert_active_or_panic();
        let handlers = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).handlers.get(channel).cloned().unwrap_or_default();
        for (_, handler) in handlers {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(data))).is_err() { eprintln!("Event handler error ({channel}): native handler panicked"); }
        }
    }
    /// Subscribe to native payloads without serializing contexts or callback objects.
    pub fn on_native<T: Send + Sync + 'static>(&self, channel: &str, handler: Arc<dyn Fn(&T) + Send + Sync>) -> BusSubscription {
        self.assert_active_or_panic();
        let erased: NativeBusHandler = Arc::new(move |data| { if let Some(data) = data.downcast_ref::<T>() { handler(data); } });
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = state.next_id; state.next_id = state.next_id.wrapping_add(1);
        state.native_handlers.entry(channel.into()).or_default().push((id, erased));
        self.registration_subscriptions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(id);
        BusSubscription { state: Arc::clone(&self.state), channel: channel.into(), id }
    }
    /// Publish the same borrowed native object to every matching subscriber.
    pub fn emit_native<T: Send + Sync + 'static>(&self, channel: &str, data: &T) {
        self.assert_active_or_panic();
        let handlers = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).native_handlers.get(channel).cloned().unwrap_or_default();
        for (_, handler) in handlers {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(data))).is_err() { eprintln!("Event handler error ({channel}): native handler panicked"); }
        }
    }
    pub fn clear(&self) {
        if self.registration_stale.load(std::sync::atomic::Ordering::Acquire) { return; }
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.handlers.clear(); state.native_handlers.clear();
    }
}
pub struct EventBusCheckpoint(BusState);

/// Native factories are passed by the CLI as an explicit Vec<Box<dyn Extension>>.
pub trait Extension: Send + Sync { fn register(&self, api: &mut ExtensionApi); }
#[derive(Clone)]
pub struct LoadedExtension {
    pub identity: ExtensionIdentity, pub source_info: SourceInfo, pub registration_cwd: PathBuf,
    pub handlers: BTreeMap<EventKind, Vec<ExtensionHandler>>, pub tools: Vec<RegisteredTool>,
    pub commands: Vec<RegisteredCommand>, pub flags: Vec<ExtensionFlag>, pub message_renderers: BTreeMap<String, MessageRenderer>,
    pub entry_renderers: BTreeMap<String, EntryRenderer>, pub entry_renderer_options: BTreeMap<String, EntryRendererOptions>,
    pub mcp_servers: Vec<RegisteredMcpServerDeclaration>, pub removed_tool_hints: BTreeMap<String, String>, pub filesystem_policies: Vec<FilesystemPolicy>,
    pub shortcuts: BTreeMap<String, ExtensionShortcut>, pub lazy_tool_activators: Vec<LazyToolActivator>,
    pub markdown_transformer: Option<MarkdownTransformer>, pub rpc_handlers: BTreeMap<String, ExtensionRpcRequestHandler>,
    pub command_context_handlers: BTreeMap<String, CommandContextHandler>,
    pub command_argument_completions: BTreeMap<String, CommandArgumentCompletions>,
    pub tool_renderers: BTreeMap<String, Arc<dyn std::any::Any + Send + Sync>>,
}
impl LoadedExtension {
    pub fn new(path: &str, cwd: PathBuf, source_info: SourceInfo) -> Self {
        Self { identity: ExtensionIdentity { path: path.into(), resolved_path: path.into() }, source_info, registration_cwd: cwd,
            handlers: BTreeMap::new(), tools: Vec::new(), commands: Vec::new(), flags: Vec::new(), message_renderers: BTreeMap::new(),
            entry_renderers: BTreeMap::new(), entry_renderer_options: BTreeMap::new(), mcp_servers: Vec::new(), removed_tool_hints: BTreeMap::new(), filesystem_policies: Vec::new(),
            shortcuts: BTreeMap::new(), lazy_tool_activators: Vec::new(), markdown_transformer: None, rpc_handlers: BTreeMap::new(), command_context_handlers: BTreeMap::new(), command_argument_completions: BTreeMap::new(), tool_renderers: BTreeMap::new() }
    }
}
#[derive(Clone, Default)]
struct RuntimeState {
    flags: BTreeMap<String, FlagValue>, actions: Option<Arc<dyn ExtensionActions>>, stale: Option<String>,
    provider_actions: Option<Arc<dyn ExtensionProviderActions>>, pending_providers: Vec<(ProviderRegistration, String)>,
    read_classifiers: Vec<(u64, ReadClassifier)>, next_classifier_id: u64,
    session_actions: Option<Arc<dyn ExtensionSessionActions>>,
    provider_errors: Vec<ExtensionError>,
    registered_providers: BTreeMap<String, String>,
    live_handlers: BTreeMap<(String, EventKind), Vec<ExtensionHandler>>,
    live_commands: BTreeMap<String, LiveCommandRegistrations>,
    live_command_argument_completions: BTreeMap<String, BTreeMap<String, CommandArgumentCompletions>>,
    live_shortcuts: BTreeMap<String, BTreeMap<String, ExtensionShortcut>>,
    live_markdown_transformers: BTreeMap<String, MarkdownTransformer>,
    live_rpc_handlers: BTreeMap<String, BTreeMap<String, ExtensionRpcRequestHandler>>,
    live_flags: BTreeMap<String, Vec<ExtensionFlag>>,
    live_tools: BTreeMap<String, Vec<RegisteredTool>>,
    live_mcp_servers: BTreeMap<String, Vec<RegisteredMcpServerDeclaration>>,
    live_message_renderers: BTreeMap<String, BTreeMap<String, MessageRenderer>>,
    live_entry_renderers: BTreeMap<String, LiveEntryRenderers>,
    live_filesystem_policies: BTreeMap<String, Vec<FilesystemPolicy>>,
    live_tool_renderers: BTreeMap<String, BTreeMap<String, Arc<dyn std::any::Any + Send + Sync>>>,
    extension_tool_executors: BTreeMap<(String, String), ExtensionToolExecutor>,
}
pub type LiveCommandRegistrations = (Vec<RegisteredCommand>, BTreeMap<String, CommandContextHandler>);
pub type LiveEntryRenderers = BTreeMap<String, (EntryRenderer, Option<EntryRendererOptions>)>;
#[derive(Clone, Default)]
pub struct ExtensionRuntime { state: Arc<Mutex<RuntimeState>>, registration_stale: Arc<Mutex<Option<String>>>, registration_pending: Arc<Mutex<Option<RegistrationPending>>>, registration_classifiers: Arc<Mutex<Vec<u64>>> }
#[derive(Default)]
struct RegistrationPending { flags: BTreeMap<String, FlagValue>, providers: Vec<ProviderRegistrationChange>, tool_executors: BTreeMap<(String, String), ExtensionToolExecutor> }
enum ProviderRegistrationChange { Register(ProviderRegistration, String), Unregister(String, String) }
pub struct RuntimeRegistrationCheckpoint(RuntimeState);
impl ExtensionRuntime {
    pub fn registration_scope(&self) -> Self {
        Self { state: self.state.clone(), registration_stale: Arc::new(Mutex::new(None)), registration_pending: Arc::new(Mutex::new(Some(RegistrationPending::default()))), registration_classifiers: Arc::default() }
    }
    pub fn commit_registration(&self) -> Result<(), ExtensionFailure> {
        self.assert_active()?;
        let pending = self.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some(pending) = pending {
            {
                let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                for (name, value) in pending.flags { state.flags.entry(name).or_insert(value); }
                state.extension_tool_executors.extend(pending.tool_executors);
            }
            for change in pending.providers { match change {
                ProviderRegistrationChange::Register(registration, path) => self.register_provider(registration, &path)?,
                ProviderRegistrationChange::Unregister(name, path) => self.unregister_provider(&name, &path)?,
            } }
        }
        Ok(())
    }
    fn register_flag_default(&self, name: &str, value: FlagValue) {
        let mut pending = self.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pending) = pending.as_mut() { pending.flags.entry(name.into()).or_insert(value); }
        else { self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).flags.entry(name.into()).or_insert(value); }
    }
    pub fn invalidate_registration(&self, message: &str) {
        self.registration_stale.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_or_insert_with(|| message.into());
        self.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        let owned = std::mem::take(&mut *self.registration_classifiers.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).read_classifiers.retain(|(id, _)| !owned.contains(id));
    }
    pub fn registration_checkpoint(&self) -> RuntimeRegistrationCheckpoint {
        RuntimeRegistrationCheckpoint(self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone())
    }
    pub fn rollback_registration(&self, checkpoint: RuntimeRegistrationCheckpoint) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let next_id = state.next_classifier_id;
        *state = checkpoint.0;
        state.next_classifier_id = next_id;
    }
    pub fn bind_session_actions(&self, actions: Arc<dyn ExtensionSessionActions>) { self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).session_actions = Some(actions); }
    pub fn session_actions(&self) -> Result<Arc<dyn ExtensionSessionActions>, ExtensionFailure> {
        self.assert_active()?;
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).session_actions.clone().ok_or_else(|| ExtensionFailure::new("Extension session actions are unavailable during registration"))
    }
    fn assert_active_or_panic(&self) {
        if let Err(error) = self.assert_active() { std::panic::panic_any(error); }
    }
    pub fn assert_active(&self) -> Result<(), ExtensionFailure> {
        if let Some(message) = &*self.registration_stale.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
            return Err(ExtensionFailure::new(message.clone()));
        }
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(message) = &state.stale { return Err(ExtensionFailure::new(message.clone())); } Ok(())
    }
    pub fn invalidate(&self, message: &str) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.stale.get_or_insert_with(|| message.into()); state.read_classifiers.clear(); state.pending_providers.clear();
        state.live_handlers.clear();
        state.live_commands.clear();
        state.live_command_argument_completions.clear();
        state.live_shortcuts.clear();
        state.live_markdown_transformers.clear(); state.live_rpc_handlers.clear();
        state.live_flags.clear();
        state.live_tools.clear(); state.live_tool_renderers.clear(); state.extension_tool_executors.clear();
        state.live_mcp_servers.clear();
        state.live_message_renderers.clear(); state.live_entry_renderers.clear();
        state.live_filesystem_policies.clear();
    }
    pub fn bind_providers(&self, actions: Arc<dyn ExtensionProviderActions>) -> Result<(), ExtensionFailure> {
        self.assert_active()?;
        let pending = {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.provider_actions = Some(Arc::clone(&actions)); std::mem::take(&mut state.pending_providers)
        };
        for (registration, path) in pending {
            let name = registration.name().to_owned();
            if let Err(error) = actions.register_provider(registration, &path) {
                self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).provider_errors.push(ExtensionError { extension_path: path, event: "register_provider".into(), error: error.message, stack: error.stack });
            } else {
                self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).registered_providers.insert(name, path);
            }
        }
        Ok(())
    }
    pub fn dispose_providers(&self) -> Result<(), ExtensionFailure> {
        let (actions, providers) = {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            (state.provider_actions.clone(), std::mem::take(&mut state.registered_providers))
        };
        if let Some(actions) = actions {
            for (name, path) in providers { actions.unregister_provider(&name, &path)?; }
        }
        Ok(())
    }
    pub fn take_provider_errors(&self) -> Vec<ExtensionError> { std::mem::take(&mut self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).provider_errors) }
    pub fn live_handlers(&self, path: &str, kind: EventKind) -> Option<Vec<ExtensionHandler>> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_handlers.get(&(path.into(), kind)).cloned()
    }
    pub fn live_commands(&self, path: &str) -> Option<LiveCommandRegistrations> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_commands.get(path).cloned()
    }
    pub fn live_shortcuts(&self, path: &str) -> Option<BTreeMap<String, ExtensionShortcut>> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_shortcuts.get(path).cloned()
    }
    pub fn live_markdown_transformer(&self, path: &str) -> Option<MarkdownTransformer> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_markdown_transformers.get(path).cloned()
    }
    pub fn live_rpc_handlers(&self, path: &str) -> Option<BTreeMap<String, ExtensionRpcRequestHandler>> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_rpc_handlers.get(path).cloned()
    }
    pub fn live_flags(&self, path: &str) -> Option<Vec<ExtensionFlag>> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_flags.get(path).cloned()
    }
    pub fn live_tools(&self, path: &str) -> Option<Vec<RegisteredTool>> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_tools.get(path).cloned()
    }
    pub fn live_mcp_servers(&self, path: &str) -> Option<Vec<RegisteredMcpServerDeclaration>> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_mcp_servers.get(path).cloned()
    }
    pub fn live_filesystem_policies(&self, path: &str) -> Option<Vec<FilesystemPolicy>> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_filesystem_policies.get(path).cloned()
    }
    pub fn live_command_argument_completions(&self, path: &str) -> Option<BTreeMap<String, CommandArgumentCompletions>> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_command_argument_completions.get(path).cloned()
    }
    pub fn live_message_renderers(&self, path: &str) -> Option<BTreeMap<String, MessageRenderer>> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_message_renderers.get(path).cloned()
    }
    pub fn live_entry_renderers(&self, path: &str) -> Option<LiveEntryRenderers> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_entry_renderers.get(path).cloned()
    }
    pub fn live_tool_renderer(&self, path: &str, name: &str) -> Option<Option<Arc<dyn std::any::Any + Send + Sync>>> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_tool_renderers.get(path).map(|renderers| renderers.get(name).cloned())
    }
    pub fn extension_tool_executor(&self, path: &str, name: &str) -> Option<ExtensionToolExecutor> {
        let execute = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extension_tool_executors.get(&(path.into(), name.into())).cloned()?;
        let runtime = self.clone();
        Some(Arc::new(move |id, params, signal, update, context| {
            let execute = execute.clone(); let runtime = runtime.clone();
            Box::pin(async move {
                runtime.assert_active()?;
                let result = execute(id, params, signal, update, context).await?;
                runtime.assert_active()?;
                Ok(result)
            })
        }))
    }
    pub fn register_provider(&self, registration: ProviderRegistration, path: &str) -> Result<(), ExtensionFailure> {
        self.assert_active()?;
        let config = match &registration {
            ProviderRegistration::Config { name, config } => Some((name, config.as_ref())),
            ProviderRegistration::ConfigOptions { name, options } => Some((name, &options.config)),
            ProviderRegistration::Native(_) => None,
        };
        if let Some((name, config)) = config {
            if config.stream_simple.is_some() && config.api.is_none() {
                return Err(ExtensionFailure::new(format!("Provider {name}: \"api\" is required when registering streamSimple.")));
            }
            if config.extra_body.as_ref().is_some_and(|body| !body.is_object()) {
                return Err(ExtensionFailure::new(format!("Provider {name}: extraBody must be an object")));
            }
            for model in config.models.iter().flatten() {
                if model.extra_body.as_ref().is_some_and(|body| !body.is_object()) {
                    return Err(ExtensionFailure::new(format!("Provider {name}, model {}: extraBody must be an object", model.id)));
                }
            }
        }
        {
            let mut pending = self.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(pending) = pending.as_mut() { pending.providers.push(ProviderRegistrationChange::Register(registration, path.into())); return Ok(()); }
        }
        let actions = {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match &state.provider_actions { Some(actions) => Arc::clone(actions), None => { state.pending_providers.push((registration, path.into())); return Ok(()); } }
        };
        let name = registration.name().to_owned();
        actions.register_provider(registration, path)?;
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).registered_providers.insert(name, path.into());
        Ok(())
    }
    pub fn unregister_provider(&self, name: &str, path: &str) -> Result<(), ExtensionFailure> {
        self.assert_active()?;
        {
            let mut pending = self.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(pending) = pending.as_mut() { pending.providers.push(ProviderRegistrationChange::Unregister(name.into(), path.into())); return Ok(()); }
        }
        let actions = {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.pending_providers.retain(|(registration, _)| registration.name() != name);
            state.provider_actions.clone()
        };
        if let Some(actions) = actions { actions.unregister_provider(name, path)?; }
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).registered_providers.remove(name);
        Ok(())
    }
    pub fn classify_read(&self, path: &Path, cwd: &Path) -> Option<CompactReadClassification> {
        let classifiers = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).read_classifiers.clone();
        for (_, classifier) in classifiers {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| classifier(path, cwd))) {
                Ok(Some(classification)) => return Some(classification), Ok(None) => {}, Err(_) => eprintln!("Read classifier failed: native classifier panicked"),
            }
        }
        None
    }
    pub fn bind(&self, actions: Arc<dyn ExtensionActions>) { self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).actions = Some(actions); }
    fn actions(&self) -> Result<Arc<dyn ExtensionActions>, ExtensionFailure> {
        self.assert_active()?;
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).actions.clone().ok_or_else(|| ExtensionFailure::new("Extension actions are unavailable during registration"))
    }
    pub fn get_flag(&self, name: &str) -> Option<FlagValue> {
        self.assert_active_or_panic();
        let existing = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).flags.get(name).cloned();
        existing.or_else(|| self.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().and_then(|pending| pending.flags.get(name).cloned()))
    }
    pub fn set_flag(&self, name: &str, value: FlagValue) { self.assert_active_or_panic(); self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).flags.insert(name.into(), value); }
}
pub struct ExtensionApi {
    pub cwd: PathBuf, pub profile: ExtensionSessionProfile, pub events: EventBus, pub runtime: ExtensionRuntime, pub registered: LoadedExtension,
}
impl ExtensionApi {
    pub fn new(registered: LoadedExtension, profile: ExtensionSessionProfile, mut events: EventBus, runtime: ExtensionRuntime) -> Self {
        events.bind_runtime(runtime.clone());
        Self { cwd: registered.registration_cwd.clone(), profile, events, runtime, registered }
    }
    pub fn on(&mut self, event: EventKind, handler: ExtensionHandler) {
        self.runtime.assert_active_or_panic();
        let handlers = self.registered.handlers.entry(event).or_default();
        handlers.push(handler);
        if self.runtime.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_none() {
            self.runtime.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_handlers.insert((self.registered.identity.path.clone(), event), handlers.clone());
        }
    }
    pub fn register_provider(&self, name: &str, config: ProviderConfig) -> Result<(), ExtensionFailure> {
        self.runtime.register_provider(ProviderRegistration::Config { name: name.into(), config: Box::new(config) }, &self.registered.identity.path)
    }
    pub fn register_provider_with_options(&self, name: &str, options: ProviderConfigOptions) -> Result<(), ExtensionFailure> {
        self.runtime.register_provider(ProviderRegistration::ConfigOptions { name: name.into(), options: Box::new(options) }, &self.registered.identity.path)
    }
    pub fn register_provider_object(&self, name: &str, mut config: ProviderObjectConfig) -> Result<(), ExtensionFailure> {
        if let Some(body) = config.extra_body { config.config.extra_body = Some(JsonValue::Object(body.into_iter().collect())); }
        if let Some(models) = &mut config.config.models {
            for model in models {
                if let Some(body) = config.model_extra_bodies.remove(&model.id) { model.extra_body = Some(JsonValue::Object(body.into_iter().collect())); }
            }
        }
        self.register_provider(name, config.config)
    }
    pub fn register_native_provider(&self, provider: Arc<dyn maho_ai::models::Provider>) -> Result<(), ExtensionFailure> {
        self.runtime.register_provider(ProviderRegistration::Native(provider), &self.registered.identity.path)
    }
    pub fn unregister_provider(&self, name: &str) -> Result<(), ExtensionFailure> { self.runtime.unregister_provider(name, &self.registered.identity.path) }
    pub fn register_shortcut(&mut self, shortcut: &str, description: Option<String>, handler: ShortcutHandler) {
        self.runtime.assert_active_or_panic();
        self.registered.shortcuts.insert(shortcut.into(), ExtensionShortcut { shortcut: shortcut.into(), description, handler, extension_path: self.registered.identity.path.clone() });
        if self.runtime.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_none() {
            self.runtime.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_shortcuts.insert(self.registered.identity.path.clone(), self.registered.shortcuts.clone());
        }
    }
    pub fn register_lazy_tool_activator(&mut self, activator: LazyToolActivator) {
        self.runtime.assert_active_or_panic();
        let actions = self.runtime.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).session_actions.clone();
        if let Some(actions) = actions
            && let Err(error) = actions.register_lazy_tool_activator(Arc::clone(&activator)) { std::panic::panic_any(error); }
        self.registered.lazy_tool_activators.push(activator);
    }
    pub fn register_markdown_transformer(&mut self, transformer: MarkdownTransformer) {
        self.runtime.assert_active_or_panic(); self.registered.markdown_transformer = Some(transformer.clone());
        if self.runtime.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_none() {
            self.runtime.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_markdown_transformers.insert(self.registered.identity.path.clone(), transformer);
        }
    }
    pub fn register_read_classifier(&self, classifier: ReadClassifier) -> Result<ReadClassifierSubscription, ExtensionFailure> {
        self.runtime.assert_active()?;
        let mut state = self.runtime.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = state.next_classifier_id; state.next_classifier_id = id.wrapping_add(1); state.read_classifiers.push((id, classifier));
        self.runtime.registration_classifiers.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(id);
        Ok(ReadClassifierSubscription { state: Arc::clone(&self.runtime.state), id })
    }
    pub fn rpc_handle(&mut self, name: &str, handler: ExtensionRpcRequestHandler) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        let name = name.trim();
        if name.is_empty() { return Err(ExtensionFailure::new("RPC extension request name must not be empty")); }
        if self.registered.rpc_handlers.contains_key(name) { return Err(ExtensionFailure::new(format!("RPC extension request handler already registered: {name}"))); }
        self.registered.rpc_handlers.insert(name.into(), handler);
        if self.runtime.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_none() {
            self.runtime.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_rpc_handlers.insert(self.registered.identity.path.clone(), self.registered.rpc_handlers.clone());
        }
        Ok(())
    }
    pub fn rpc_emit(&self, name: &str, data: &JsonValue) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        let name = name.trim();
        if name.is_empty() { return Err(ExtensionFailure::new("RPC extension event name must not be empty")); }
        let event = JsonValue::Object([(String::from("name"), JsonValue::String(name.into())), (String::from("data"), data.clone())].into_iter().collect());
        self.events.emit("senpi:extension-rpc-event", &event); Ok(())
    }
    pub fn register_tool(&mut self, definition: ToolDefinition) {
        if let Err(error) = self.try_register_tool(definition) { std::panic::panic_any(error); }
    }
    pub fn register_tool_with_extension_context(&mut self, definition: ToolDefinition, execute: ExtensionToolExecutor) -> Result<(), ExtensionFailure> {
        let name = definition.name.clone();
        self.try_register_tool(definition)?;
        let path = if self.registered.source_info.path.is_empty() { self.registered.identity.path.clone() } else { self.registered.source_info.path.clone() };
        let key = (path, name);
        let mut pending = self.runtime.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pending) = pending.as_mut() { pending.tool_executors.insert(key, execute); }
        else { self.runtime.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extension_tool_executors.insert(key, execute); }
        Ok(())
    }
    pub fn register_typed_tool<TArgs: serde::de::DeserializeOwned + Send + 'static, TDetails: serde::Serialize + Send + 'static>(&mut self, definition: ToolDefinition, execute: TypedExtensionToolExecutor<TArgs, TDetails>) -> Result<(), ExtensionFailure> {
        self.register_tool_with_extension_context(definition, Arc::new(move |id, params, signal, update, context| {
            let execute = execute.clone();
            Box::pin(async move {
                let params = serde_json::from_value(params).map_err(|error| ExtensionFailure::new(error.to_string()))?;
                let update = update.map(|update| Arc::new(move |result: TypedAgentToolResult<TDetails>| { update(result.into_agent_result()?); Ok(()) }) as TypedToolUpdateCallback<TDetails>);
                execute(id, params, signal, update, context).await?.into_agent_result()
            })
        }))
    }
    pub fn register_tool_with_renderers<TState: 'static, TArgs: Clone + 'static>(&mut self, definition: ToolDefinition, renderers: ToolRenderers<TState, TArgs>) -> Result<(), ExtensionFailure> {
        let name = definition.name.clone();
        self.try_register_tool(definition)?;
        self.registered.tool_renderers.insert(name, Arc::new(renderers));
        self.publish_tools();
        Ok(())
    }
    pub fn try_register_tool(&mut self, definition: ToolDefinition) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        if definition.name == "tool_search" && self.registered.source_info.source != "builtin" {
            return Err(ExtensionFailure::new("Tool name \"tool_search\" is reserved for the builtin tool-search extension."));
        }
        if !definition.parameters.is_object() {
            return Err(ExtensionFailure::new(format!("Tool \"{}\" registered by extension \"{}\" must define an object parameter schema.", definition.name, self.registered.identity.path)));
        }
        let mut source_info = self.registered.source_info.clone();
        if source_info.path.is_empty() { source_info.path = self.registered.identity.path.clone(); }
        let key = (source_info.path.clone(), definition.name.clone());
        let mut pending = self.runtime.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pending) = pending.as_mut() { pending.tool_executors.remove(&key); }
        else { self.runtime.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extension_tool_executors.remove(&key); }
        drop(pending);
        self.registered.tool_renderers.remove(&definition.name);
        let tool = RegisteredTool { definition, source_info };
        if let Some(existing) = self.registered.tools.iter_mut().find(|t| t.definition.name == tool.definition.name) { *existing = tool.clone(); } else { self.registered.tools.push(tool.clone()); }
        let actions = self.runtime.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).session_actions.clone();
        if let Some(actions) = actions { actions.install_registered_tool(tool)?; }
        self.publish_tools();
        Ok(())
    }
    fn publish_tools(&self) {
        if self.runtime.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_none() {
            let mut state = self.runtime.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.live_tools.insert(self.registered.identity.path.clone(), self.registered.tools.clone());
            state.live_tool_renderers.insert(self.registered.identity.path.clone(), self.registered.tool_renderers.clone());
        }
    }
    pub fn register_command(&mut self, name: &str, description: Option<String>, argument_hint: Option<String>, handler: CommandHandler) {
        self.runtime.assert_active_or_panic();
        self.registered.command_context_handlers.remove(name);
        self.registered.command_argument_completions.remove(name);
        let command = RegisteredCommand { name: name.into(), source_info: self.registered.source_info.clone(), description, argument_hint, handler };
        if let Some(existing) = self.registered.commands.iter_mut().find(|c| c.name == name) { *existing = command; } else { self.registered.commands.push(command); }
        self.publish_commands();
    }
    fn publish_commands(&self) {
        if self.runtime.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_none() {
            let mut state = self.runtime.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.live_commands.insert(self.registered.identity.path.clone(), (self.registered.commands.clone(), self.registered.command_context_handlers.clone()));
            state.live_command_argument_completions.insert(self.registered.identity.path.clone(), self.registered.command_argument_completions.clone());
        }
    }
    pub fn register_command_with_completions(&mut self, name: &str, description: Option<String>, argument_hint: Option<String>, handler: CommandHandler, completions: CommandArgumentCompletions) {
        self.register_command(name, description, argument_hint, handler);
        self.registered.command_argument_completions.insert(name.into(), completions);
        self.publish_commands();
    }
    pub fn register_command_with_context(&mut self, name: &str, description: Option<String>, argument_hint: Option<String>, handler: CommandContextHandler) {
        self.register_command(name, description, argument_hint, Arc::new(|_, _| Box::pin(async { Err(ExtensionFailure::new("Command requires a command-capable context")) })));
        self.registered.command_context_handlers.insert(name.into(), handler);
        self.publish_commands();
    }
    pub fn register_flag(&mut self, name: &str, kind: FlagType, description: Option<String>) {
        self.runtime.assert_active_or_panic();
        let default = match &kind { FlagType::Boolean { default } => default.map(FlagValue::Boolean), FlagType::String { default } => default.clone().map(FlagValue::String) };
        if let Some(value) = default { self.runtime.register_flag_default(name, value); }
        let flag = ExtensionFlag { name: name.into(), description, kind, extension_path: self.registered.identity.path.clone() };
        if let Some(existing) = self.registered.flags.iter_mut().find(|f| f.name == name) { *existing = flag; } else { self.registered.flags.push(flag); }
        if self.runtime.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_none() {
            self.runtime.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_flags.insert(self.registered.identity.path.clone(), self.registered.flags.clone());
        }
    }
    pub fn get_flag(&self, name: &str) -> Option<FlagValue> { self.runtime.assert_active_or_panic(); if self.registered.flags.iter().any(|flag| flag.name == name) { self.runtime.get_flag(name) } else { None } }
    pub fn set_flag(&self, name: &str, value: FlagValue) { self.runtime.set_flag(name, value); }
    pub fn register_message_renderer(&mut self, custom_type: &str, renderer: MessageRenderer) {
        self.runtime.assert_active_or_panic(); self.registered.message_renderers.insert(custom_type.into(), renderer);
        if self.runtime.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_none() {
            self.runtime.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_message_renderers.insert(self.registered.identity.path.clone(), self.registered.message_renderers.clone());
        }
    }
    pub fn register_entry_renderer(&mut self, custom_type: &str, renderer: EntryRenderer, options: EntryRendererOptions) {
        self.register_entry_renderer_optional(custom_type, renderer, Some(options));
    }
    pub fn register_entry_renderer_optional(&mut self, custom_type: &str, renderer: EntryRenderer, options: Option<EntryRendererOptions>) {
        self.runtime.assert_active_or_panic(); self.registered.entry_renderers.insert(custom_type.into(), renderer);
        if let Some(options) = options { self.registered.entry_renderer_options.insert(custom_type.into(), options); }
        else { self.registered.entry_renderer_options.remove(custom_type); }
        if self.runtime.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_none() {
            let renderers = self.registered.entry_renderers.iter().map(|(name, renderer)| (name.clone(), (renderer.clone(), self.registered.entry_renderer_options.get(name).cloned()))).collect();
            self.runtime.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_entry_renderers.insert(self.registered.identity.path.clone(), renderers);
        }
    }
    pub fn register_mcp_server(&mut self, name: &str, config: McpServerDeclaration) {
        if let Err(error) = self.try_register_mcp_server(name, config) { std::panic::panic_any(error); }
    }
    pub fn try_register_mcp_server(&mut self, name: &str, config: McpServerDeclaration) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        for (field, value) in [
            ("idleTimeoutMin", config.idle_timeout_min), ("requestTimeoutMs", config.request_timeout_ms),
            ("connectTimeoutMs", config.connect_timeout_ms), ("startupTimeoutMs", config.startup_timeout_ms),
        ] {
            if value.is_some_and(|value| !value.is_finite()) {
                return Err(ExtensionFailure::new(format!("Invalid MCP server declaration \"{name}\": {field}: must be number")));
            }
        }
        if config.enabled != Some(false) {
            let transport = config.transport.unwrap_or_else(|| if config.url.as_ref().is_some_and(|url| !url.is_empty()) { McpTransport::Http } else { McpTransport::Stdio });
            let (field, endpoint, kind) = match transport {
                McpTransport::Stdio => ("command", config.command.as_deref(), "stdio"),
                McpTransport::Http => ("url", config.url.as_deref(), "http"),
            };
            if endpoint.is_none_or(|endpoint| endpoint.trim().is_empty()) {
                return Err(ExtensionFailure::new(format!("Invalid MCP server declaration \"{name}\": mcpServers.{name}.{field}: Required for enabled {kind} server")));
            }
        }
        let declaration = RegisteredMcpServerDeclaration { name: name.into(), config, extension_path: self.registered.identity.path.clone(), registration_cwd: self.cwd.clone() };
        if let Some(existing) = self.registered.mcp_servers.iter_mut().find(|s| s.name == name) { *existing = declaration; } else { self.registered.mcp_servers.push(declaration); }
        if self.runtime.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_none() {
            self.runtime.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_mcp_servers.insert(self.registered.identity.path.clone(), self.registered.mcp_servers.clone());
        }
        Ok(())
    }
    pub fn register_removed_tool_hint(&mut self, name: &str, hint: &str) {
        self.runtime.assert_active_or_panic();
        let actions = self.runtime.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).session_actions.clone();
        if let Some(actions) = actions
            && let Err(error) = actions.register_removed_tool_hint(name, hint) { std::panic::panic_any(error); }
        self.registered.removed_tool_hints.insert(name.into(), hint.into());
    }
    pub fn register_filesystem_policy(&mut self, policy: FilesystemPolicy) {
        self.runtime.assert_active_or_panic(); self.registered.filesystem_policies.push(policy);
        if self.runtime.registration_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_none() {
            self.runtime.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).live_filesystem_policies.insert(self.registered.identity.path.clone(), self.registered.filesystem_policies.clone());
        }
    }
    pub fn send_message(&self, message: CustomMessage, options: SendMessageOptions) -> Result<(), ExtensionFailure> { self.runtime.actions()?.send_message(message, options) }
    pub fn send_user_message(&self, content: UserMessageContent, options: SendUserMessageOptions) -> Result<(), ExtensionFailure> { self.runtime.actions()?.send_user_message(content, options) }
    pub fn append_entry(&self, custom_type: &str, data: Option<JsonValue>) -> Result<(), ExtensionFailure> { self.runtime.actions()?.append_entry(custom_type, data) }
    pub fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> { self.runtime.actions()?.get_all_tools() }
    pub fn set_session_name(&self, name: &str) -> Result<(), ExtensionFailure> { self.runtime.session_actions()?.set_session_name(name) }
    pub fn get_session_name(&self) -> Result<Option<String>, ExtensionFailure> { self.runtime.session_actions()?.get_session_name() }
    pub fn set_label(&self, entry_id: &str, label: Option<&str>) -> Result<(), ExtensionFailure> { self.runtime.session_actions()?.set_label(entry_id, label) }
    pub fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> { self.runtime.session_actions()?.get_active_tools() }
    pub fn set_active_tools(&self, names: Vec<String>) -> Result<(), ExtensionFailure> { self.runtime.session_actions()?.set_active_tools(names) }
    pub fn refresh_tools(&self) -> Result<(), ExtensionFailure> { self.runtime.session_actions()?.refresh_tools() }
    pub fn get_commands(&self) -> Result<Vec<SlashCommandInfo>, ExtensionFailure> { self.runtime.session_actions()?.get_commands() }
    pub async fn set_model(&self, model: Model) -> Result<bool, ExtensionFailure> {
        let result = self.runtime.session_actions()?.set_model(model).await?;
        self.runtime.assert_active()?;
        Ok(result)
    }
    pub fn get_thinking_level(&self) -> Result<ThinkingLevel, ExtensionFailure> { self.runtime.session_actions()?.get_thinking_level() }
    pub fn set_thinking_level(&self, level: ThinkingLevel) -> Result<(), ExtensionFailure> { self.runtime.session_actions()?.set_thinking_level(level) }
    pub async fn set_session_model(&self, model: Model) -> Result<bool, ExtensionFailure> {
        let result = self.runtime.session_actions()?.set_session_model(model).await?;
        self.runtime.assert_active()?;
        Ok(result)
    }
    pub fn set_session_thinking_level(&self, level: ThinkingLevel) -> Result<(), ExtensionFailure> { self.runtime.session_actions()?.set_session_thinking_level(level) }
    pub fn set_session_fast_mode(&self, enabled: bool) -> Result<(), ExtensionFailure> { self.runtime.session_actions()?.set_session_fast_mode(enabled) }
    pub async fn execute_tool(&self, name: &str, params: JsonValue, options: ExecuteToolOptions) -> Result<maho_agent::types::AgentToolResult, ExecuteToolError> {
        let actions = self.runtime.session_actions().map_err(|error| ExecuteToolError { code: ExecuteToolErrorCode::Blocked, tool_name: name.into(), message: error.message, active_tools: Vec::new() })?;
        let result = actions.execute_tool(name, params, options).await?;
        self.runtime.assert_active().map_err(|error| ExecuteToolError { code: ExecuteToolErrorCode::Blocked, tool_name: name.into(), message: error.message, active_tools: Vec::new() })?;
        Ok(result)
    }
    pub async fn exec(&self, command: &str, args: &[String], options: ExecOptions) -> Result<ExecResult, ExtensionFailure> {
        let result = self.runtime.session_actions()?.exec(command, args, &self.cwd, options).await?;
        self.runtime.assert_active()?;
        Ok(result)
    }
}

pub const RUNTIME_EXTENSION_PATH: &str = "<runtime>";
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionError { pub extension_path: String, pub event: String, pub error: String, pub stack: Option<String> }

/// Additional session-owned events from agent-session.ts:312-494. Agent lifecycle events
/// retain the real agent union; open records stay data, with no dependency on maho-core.
#[derive(Clone, Debug)]
pub enum AgentSessionEvent {
    Agent(AgentEvent), AgentEnd { messages: Vec<AgentMessage>, aborted: bool, will_retry: bool, abort_source: Option<AbortSource> },
    MessageUpdate { message: AgentMessage, assistant_message_event: JsonValue }, AgentSettled, AgentIdle, SessionAbort,
    ResumeCompactionRequired { projection: JsonValue, notice: String }, ResumeContextReduced { tokens_before: u64, tokens_after: u64, dropped_entries: usize, notice: String },
    ContinuationError { error_message: String }, SkillInvocation { skills: Vec<SkillInvocation> }, CommandInvocation { command: JsonValue },
    QueueUpdate { steering: Vec<String>, follow_up: Vec<String>, ordered: Vec<QueuedInput> },
    CompactionStart { reason: CompactionReason, request_id: Option<String> }, CompactionProgress { reason: CompactionReason, delta: Option<String>, text: Option<String> },
    EntryAppended { entry: SessionEntry }, SessionInfoChanged { name: Option<String> }, ToolHookStatus(ToolHookLifecycleEvent),
    SystemPromptChange { system_prompt: String, previous_system_prompt: String, system_prompt_name: Option<String>, model: Model, previous_model: Option<Model> },
    ThinkingLevelChanged { level: ThinkingLevel }, HighReasoningWarning { model_id: String, provider: String, thinking_level: ThinkingLevel },
    SettingsSourceSelected { selection: JsonValue }, ModelChanged { model: Model, thinking_level: ThinkingLevel, source: ModelSelectSource },
    ModelChangeRejected { model: Model, reason: String, detail: String, budget: Option<ModelBudget> },
    ModelChangeSkipped { model: Model, budget: ModelBudget, direction: String }, ModelChangePending { model: Model, budget: ModelBudget, notice: String },
    ServiceTierChanged { tier: Option<ServiceTier>, fast_mode: bool }, SessionSettingsChanged { steering_mode: String, follow_up_mode: String, auto_compaction_enabled: bool },
    CompactionEnd { reason: CompactionReason, result: Option<CompactionResult>, aborted: bool, will_retry: bool, request_id: Option<String>, accepted: Option<bool>, rejection_cause: Option<CompactionRejectionCause>, error_message: Option<String> },
    AutoRetryStart { attempt: u32, max_attempts: u32, delay_ms: u64, error_message: String }, AutoRetryEnd { success: bool, attempt: u32, final_error: Option<String> },
    RetryFallbackApplied { from: String, to: String, chain_key: String, reason: String }, RetryFallbackSucceeded { model: String, chain_key: String },
    RetryFallbackReverted { from: String, to: String }, RetryFallbackExhausted { chain_key: String, last_error: String }, ServerFallbackAborted { from: String, to: String, chain_configured: bool },
    AuthLoginUrl { provider: String, url: String }, AuthLoginEnd { provider: String, success: bool, error: Option<String> },
    SummarizationRetryScheduled { attempt: u32, max_attempts: u32, delay_ms: u64, error_message: String }, SummarizationRetryAttemptStart { source: String, reason: Option<CompactionReason> }, SummarizationRetryFinished,
    RetryProbeScheduled { selector: String, at_ms: u64, probe_index: u8 }, RetryProbeResult { selector: String, ok: bool, error_message: Option<String> }, BashExecutionUpdate { id: Option<String>, delta: String },
}
#[derive(Clone, Debug)]
pub struct SkillInvocation { pub name: String, pub path: String, pub syntax: String }
#[derive(Clone, Debug)]
pub struct QueuedInput { pub text: String, pub mode: StreamingBehavior, pub enqueue_order: u64 }
#[derive(Clone, Debug)]
pub struct ModelBudget { pub context_window: u64, pub live_context_tokens: u64, pub required_tokens: u64, pub shortfall_tokens: u64, pub safety_margin_profile: Option<String> }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolHookName { PreToolUse, PostToolUse }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolHookStatus { Completed, Blocked, Failed }
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolHookPhase { Start, Update, End { completed_at: u64, status: ToolHookStatus, error_message: Option<String> } }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolHookLifecycleEvent { pub hook_run_id: String, pub hook_name: ToolHookName, pub tool_name: String, pub tool_call_id: String, pub extension_path: String, pub status_message: String, pub started_at: u64, pub phase: ToolHookPhase }
