//! Rust mirror of senpi core/extensions/types.ts at fe8c564b.
//! Session, resource, registry and theme ports live here to keep the dependency graph acyclic.
use crate::offload::{
    AccountLimits, ChildModuleRegistration, InputOrigin, OffloadOptions, RegisteredChildModule,
    SubscriptionLimitsListener, UploadRedaction, validate_child_module_registration,
};
pub use maho_agent::types::{AgentEvent, AgentMessage};
pub use maho_agent::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback};
pub use maho_ai::types::{AssistantMessage, ContentBlock, Message, UserContent, UserMessage};
pub use maho_ai::{
    model::Model,
    types::{ImageContent, JsonValue, ModelThinkingLevel, ThinkingLevel, Usage},
};
pub use maho_tools::definition::{
    AbortSignal, ToolCall, ToolContent, ToolError, ToolExecutionMode, ToolExposure,
    ToolPermissionParser, ToolPermissionRequest, ToolResult, ToolSessionManager,
};
pub use maho_tools::filesystem_policy::FilesystemOperation;
pub use maho_tools::tool_definition_wrapper::wrap_tool_definition;
pub use maho_tools::{
    FilesystemPolicy, FilesystemPolicyChecker, FilesystemPolicyDecision, FilesystemPolicyRequest,
    KernelPrelude, ToolContext, ToolDefinition,
};
pub use maho_tools::{
    bash::BashToolInput, edit::EditToolInput, find::FindToolInput, grep::index::GrepToolInput,
    ls::LsToolInput, read::ReadToolInput, write::WriteToolInput,
};
pub use maho_tui::autocomplete::AutocompleteItem;
pub use maho_tui::tui::Component;
use std::{
    collections::BTreeMap,
    fmt,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
};

pub fn define_tool(tool: ToolDefinition) -> ToolDefinition {
    tool
}
pub fn is_tool_call_event_type(name: &str, event: &ToolCallEvent) -> bool {
    event.tool_name == name
}
pub fn is_bash_tool_result(event: &ToolResultEvent) -> bool {
    event.tool_name == "bash"
}
pub fn is_power_shell_tool_result(event: &ToolResultEvent) -> bool {
    event.tool_name == "powershell"
}
pub fn is_read_tool_result(event: &ToolResultEvent) -> bool {
    event.tool_name == "read"
}
pub fn is_edit_tool_result(event: &ToolResultEvent) -> bool {
    event.tool_name == "edit"
}
pub fn is_write_tool_result(event: &ToolResultEvent) -> bool {
    event.tool_name == "write"
}
pub fn is_grep_tool_result(event: &ToolResultEvent) -> bool {
    event.tool_name == "grep"
}
pub fn is_find_tool_result(event: &ToolResultEvent) -> bool {
    event.tool_name == "find"
}
pub fn is_ls_tool_result(event: &ToolResultEvent) -> bool {
    event.tool_name == "ls"
}

pub type ExtensionFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ExtensionFailure>> + Send + 'a>>;
pub type UiFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvalHandleKind {
    Agent,
    Completion,
    Workpool,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EvalHandleRef {
    pub kind: EvalHandleKind,
    pub id: String,
    pub run_epoch: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvalHandlePhase {
    Pending,
    Succeeded,
    Failed,
    Cancelled,
    Lost,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalHandleSnapshot {
    pub reference: EvalHandleRef,
    pub phase: EvalHandlePhase,
    pub host_status: String,
    pub revision: i64,
}
#[derive(Clone, Debug, PartialEq)]
pub enum EvalHandleOutcome {
    Fulfilled {
        reference: EvalHandleRef,
        value: JsonValue,
    },
    Rejected {
        reference: EvalHandleRef,
        error: EvalHandleError,
    },
}
#[derive(Clone, Debug, PartialEq)]
pub struct EvalHandleError {
    pub code: String,
    pub message: String,
    pub details: Option<JsonValue>,
}
#[derive(Clone, Debug)]
pub struct EvalHandleCallContext {
    pub owner_session_id: String,
    pub signal: Option<maho_ai::utils::abort::AbortSignal>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EvalHandleOutputRequest {
    pub tail: bool,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalHandleOutputSnapshot {
    pub reference: EvalHandleRef,
    pub text: String,
    pub offset: usize,
    pub total: usize,
    pub truncated: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalHandleCancelReceipt {
    pub reference: EvalHandleRef,
    pub cancelled: bool,
    pub phase: EvalHandlePhase,
}
pub trait EvalHandleUpdateStream: Send {
    fn poll_next(
        self: Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<EvalHandleSnapshot>>;
}
pub struct EvalHandleWatch {
    pub initial: Vec<EvalHandleSnapshot>,
    pub updates: Pin<Box<dyn EvalHandleUpdateStream>>,
    pub close: Arc<dyn Fn() + Send + Sync>,
}
pub trait EvalHandleHost: Send + Sync {
    fn watch<'a>(
        &'a self,
        references: Vec<EvalHandleRef>,
        context: EvalHandleCallContext,
    ) -> ExtensionFuture<'a, EvalHandleWatch>;
    fn result<'a>(
        &'a self,
        reference: EvalHandleRef,
        context: EvalHandleCallContext,
    ) -> ExtensionFuture<'a, EvalHandleOutcome>;
    fn send<'a>(
        &'a self,
        reference: EvalHandleRef,
        message: String,
        context: EvalHandleCallContext,
    ) -> ExtensionFuture<'a, EvalHandleSnapshot>;
    fn cancel<'a>(
        &'a self,
        reference: EvalHandleRef,
        context: EvalHandleCallContext,
    ) -> ExtensionFuture<'a, EvalHandleCancelReceipt>;
    fn output<'a>(
        &'a self,
        reference: EvalHandleRef,
        request: EvalHandleOutputRequest,
        context: EvalHandleCallContext,
    ) -> ExtensionFuture<'a, EvalHandleOutputSnapshot>;
}
pub type SharedEvalHandleHost = Arc<dyn EvalHandleHost>;
pub type ToolHookStatusUpdater = Arc<dyn Fn(&str) + Send + Sync>;
pub type ExtensionToolExecutor = Arc<
    dyn for<'a> Fn(
            &'a str,
            JsonValue,
            Option<maho_ai::utils::abort::AbortSignal>,
            Option<maho_agent::types::AgentToolUpdateCallback>,
            &'a ExtensionContext,
        ) -> ExtensionFuture<'a, AgentToolResult>
        + Send
        + Sync,
>;
#[derive(Clone, Debug, PartialEq)]
pub struct TypedAgentToolResult<TDetails> {
    pub content: Vec<ContentBlock>,
    pub details: TDetails,
    pub usage: Option<Usage>,
    pub nested_calls: Option<maho_ai::types::NestedToolCalls>,
    pub added_tool_names: Option<Vec<String>>,
    pub terminate: Option<bool>,
    pub is_error: Option<bool>,
}
impl<TDetails: serde::Serialize> TypedAgentToolResult<TDetails> {
    pub fn into_agent_result(self) -> Result<AgentToolResult, ExtensionFailure> {
        Ok(AgentToolResult {
            content: self.content,
            details: serde_json::to_value(self.details)
                .map_err(|error| ExtensionFailure::new(error.to_string()))?,
            structured_content: None,
            usage: self.usage,
            nested_calls: self.nested_calls,
            added_tool_names: self.added_tool_names,
            terminate: self.terminate,
            is_error: self.is_error,
        })
    }
}
impl<TDetails: serde::de::DeserializeOwned> TypedAgentToolResult<TDetails> {
    pub fn from_agent_result(result: &AgentToolResult) -> Result<Self, ExtensionFailure> {
        Ok(Self {
            content: result.content.clone(),
            details: serde_json::from_value(result.details.clone())
                .map_err(|error| ExtensionFailure::new(error.to_string()))?,
            usage: result.usage,
            nested_calls: result.nested_calls.clone(),
            added_tool_names: result.added_tool_names.clone(),
            terminate: result.terminate,
            is_error: result.is_error,
        })
    }
}
pub type TypedToolUpdateCallback<TDetails> =
    Arc<dyn Fn(TypedAgentToolResult<TDetails>) -> Result<(), ExtensionFailure> + Send + Sync>;
pub type TypedExtensionToolExecutor<TArgs, TDetails> = Arc<
    dyn for<'a> Fn(
            &'a str,
            TArgs,
            Option<maho_ai::utils::abort::AbortSignal>,
            Option<TypedToolUpdateCallback<TDetails>>,
            &'a ExtensionContext,
        ) -> ExtensionFuture<'a, TypedAgentToolResult<TDetails>>
        + Send
        + Sync,
>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToolRenderResultOptions {
    pub expanded: bool,
    pub is_partial: bool,
}
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
pub type ToolCallRenderer<TState, TArgs> = Arc<
    dyn Fn(&TArgs, &Theme, &mut ToolRenderContext<TState, TArgs>) -> Box<dyn Component>
        + Send
        + Sync,
>;
pub type ToolResultRenderer<TState, TArgs> = Arc<
    dyn Fn(
            &AgentToolResult,
            ToolRenderResultOptions,
            &Theme,
            &mut ToolRenderContext<TState, TArgs>,
        ) -> Box<dyn Component>
        + Send
        + Sync,
>;
pub type TypedToolResultRenderer<TState, TArgs, TDetails> = Arc<
    dyn Fn(
            &TypedAgentToolResult<TDetails>,
            ToolRenderResultOptions,
            &Theme,
            &mut ToolRenderContext<TState, TArgs>,
        ) -> Box<dyn Component>
        + Send
        + Sync,
>;
pub fn typed_tool_result_renderer<
    TState: 'static,
    TArgs: 'static,
    TDetails: serde::de::DeserializeOwned + 'static,
>(
    renderer: TypedToolResultRenderer<TState, TArgs, TDetails>,
) -> ToolResultRenderer<TState, TArgs> {
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
        ToolRendererSlots {
            session: self,
            call_component,
            result_component: None,
        }
    }
}
impl<TState, TArgs: Clone> ToolRendererSlots<TState, TArgs> {
    pub fn render_call(&mut self, theme: &Theme, width: usize) -> Option<Vec<String>> {
        self.session.context.last_component = self.call_component.take();
        let lines = self.session.render_call(theme, width);
        self.call_component = self.session.context.last_component.take();
        lines
    }
    pub fn render_result(
        &mut self,
        result: &AgentToolResult,
        theme: &Theme,
        width: usize,
    ) -> Option<Vec<String>> {
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
    pub fn render_result(
        &mut self,
        result: &AgentToolResult,
        theme: &Theme,
        width: usize,
    ) -> Option<Vec<String>> {
        let renderer = self.renderers.render_result.as_ref()?;
        let options = ToolRenderResultOptions {
            expanded: self.context.expanded,
            is_partial: self.context.is_partial,
        };
        let mut component = renderer(result, options, theme, &mut self.context);
        let lines = component.render(width);
        self.context.last_component = Some(component);
        Some(lines)
    }
}

/// Object-safe, `Send+Sync` view over a registered typed renderer set. The generic parameters are
/// erased at registration time so a host can render any card by tool name with JSON args and no
/// type parameters.
pub trait ErasedToolRenderers: Send + Sync {
    /// senpi `renderCall`/`renderResult` are optional; a host draws its fallback for a missing half.
    fn has_call_renderer(&self) -> bool {
        true
    }
    fn has_result_renderer(&self) -> bool {
        true
    }
    fn render_call(&self, args: &JsonValue, theme: &Theme, width: usize) -> Option<Vec<String>>;
    fn render_result(
        &self,
        args: &JsonValue,
        result: &AgentToolResult,
        theme: &Theme,
        width: usize,
    ) -> Option<Vec<String>>;
    /// senpi export `renderResult` with explicit `{ expanded, isPartial }` options and the
    /// context's `isError`; implementors without context state render as `render_result`.
    fn render_result_with(
        &self,
        args: &JsonValue,
        result: &AgentToolResult,
        options: ToolRenderResultOptions,
        is_error: bool,
        theme: &Theme,
        width: usize,
    ) -> Option<Vec<String>> {
        let _ = (options, is_error);
        self.render_result(args, result, theme, width)
    }
}
struct TypedErasedToolRenderers<TState, TArgs> {
    renderers: Arc<ToolRenderers<TState, TArgs>>,
    state: Mutex<Option<TState>>,
    cwd: PathBuf,
}
fn erased_render_context<TState, TArgs>(
    args: TArgs,
    state: TState,
    cwd: PathBuf,
) -> ToolRenderContext<TState, TArgs> {
    ToolRenderContext {
        args,
        tool_call_id: String::new(),
        invalidate: std::rc::Rc::new(|| {}),
        last_component: None,
        state,
        cwd,
        execution_started: false,
        args_complete: true,
        is_partial: false,
        expanded: false,
        show_images: false,
        image_protocol: None,
        is_error: false,
        has_result: None,
        spinner_frame: None,
    }
}
impl<TState, TArgs> ErasedToolRenderers for TypedErasedToolRenderers<TState, TArgs>
where
    TState: Default + Send + 'static,
    TArgs: Clone + serde::de::DeserializeOwned + Send + 'static,
{
    fn has_call_renderer(&self) -> bool {
        self.renderers.render_call.is_some()
    }
    fn has_result_renderer(&self) -> bool {
        self.renderers.render_result.is_some()
    }
    fn render_call(&self, args: &JsonValue, theme: &Theme, width: usize) -> Option<Vec<String>> {
        let renderer = self.renderers.render_call.as_ref()?;
        let typed: TArgs = serde_json::from_value(args.clone()).ok()?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut context = erased_render_context(
            typed.clone(),
            state.take().unwrap_or_default(),
            self.cwd.clone(),
        );
        let mut component = renderer(&typed, theme, &mut context);
        let lines = component.render(width);
        *state = Some(context.state);
        Some(lines)
    }
    fn render_result(
        &self,
        args: &JsonValue,
        result: &AgentToolResult,
        theme: &Theme,
        width: usize,
    ) -> Option<Vec<String>> {
        self.render_result_with(
            args,
            result,
            ToolRenderResultOptions::default(),
            false,
            theme,
            width,
        )
    }
    fn render_result_with(
        &self,
        args: &JsonValue,
        result: &AgentToolResult,
        options: ToolRenderResultOptions,
        is_error: bool,
        theme: &Theme,
        width: usize,
    ) -> Option<Vec<String>> {
        let renderer = self.renderers.render_result.as_ref()?;
        let typed: TArgs = serde_json::from_value(args.clone()).ok()?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut context = erased_render_context(
            typed.clone(),
            state.take().unwrap_or_default(),
            self.cwd.clone(),
        );
        context.expanded = options.expanded;
        context.is_partial = options.is_partial;
        context.is_error = is_error;
        let mut component = renderer(result, options, theme, &mut context);
        let lines = component.render(width);
        *state = Some(context.state);
        Some(lines)
    }
}
/// Erases the generic parameters of a typed renderer set into a `Send+Sync` handle.
pub fn erase_tool_renderers<TState, TArgs>(
    renderers: Arc<ToolRenderers<TState, TArgs>>,
    cwd: PathBuf,
) -> Arc<dyn ErasedToolRenderers>
where
    TState: Default + Send + 'static,
    TArgs: Clone + serde::de::DeserializeOwned + Send + 'static,
{
    Arc::new(TypedErasedToolRenderers {
        renderers,
        state: Mutex::new(None),
        cwd,
    })
}

pub type LazyToolActivator = Arc<dyn Fn(&str) -> bool + Send + Sync>;
/// Consumes the tool-search service's pending native-injection failure (`takeNativeInjectionFailure`).
pub type NativeToolSearchInjectionFailureSource = Arc<dyn Fn() -> Option<String> + Send + Sync>;
pub type ShortcutHandler =
    Arc<dyn for<'a> Fn(&'a ExtensionContext) -> ExtensionFuture<'a, ()> + Send + Sync>;
#[derive(Clone)]
pub struct ExtensionShortcut {
    pub shortcut: String,
    pub description: Option<String>,
    pub handler: ShortcutHandler,
    pub extension_path: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkdownMessageType {
    User,
    Assistant,
    AssistantThinking,
}
#[derive(Clone, Debug)]
pub struct MarkdownTransformContext {
    pub message_type: MarkdownMessageType,
    pub is_streaming: bool,
    pub available_width: usize,
}
pub type MarkdownTransformer = Arc<dyn Fn(&str, &MarkdownTransformContext) -> String + Send + Sync>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactReadKind {
    Docs,
    Resource,
    Skill,
    Memory,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactReadClassification {
    pub kind: CompactReadKind,
    pub label: String,
    pub headline: Option<String>,
}
pub type ReadClassifier =
    Arc<dyn Fn(&Path, &Path) -> Option<CompactReadClassification> + Send + Sync>;
pub type ExtensionRpcRequestHandler =
    Arc<dyn Fn(JsonValue) -> ExtensionFuture<'static, JsonValue> + Send + Sync>;
pub type ProviderStream = Arc<
    dyn Fn(
            &Model,
            &maho_ai::types::Context,
            Option<maho_ai::types::SimpleStreamOptions>,
        ) -> maho_ai::types::AssistantMessageEventStream
        + Send
        + Sync,
>;
pub type ProviderRefresh = Arc<
    dyn Fn(
            maho_ai::models::RefreshModelsContext,
        ) -> ExtensionFuture<'static, Vec<ProviderModelConfig>>
        + Send
        + Sync,
>;
#[derive(Clone, Debug)]
pub struct ProviderModelConfig {
    pub model_type: Option<maho_ai::types::ModelType>,
    pub output: Option<Vec<maho_ai::types::ImagesOutputModality>>,
    pub image_cost: Option<maho_ai::types::ImagesModelCost>,
    pub input_limits: Option<maho_ai::types::ModelInputLimits>,
    pub prompt_cache: Option<maho_ai::types::ModelPromptCache>,
    pub supports_assistant_prefill: Option<bool>,
    pub default_thinking_level: Option<maho_ai::types::ModelThinkingLevel>,
    pub id: String,
    pub name: String,
    pub upstream_model_id: Option<String>,
    pub api: Option<String>,
    pub base_url: Option<String>,
    pub reasoning: bool,
    pub recover_text_tool_calls: Option<bool>,
    pub thinking_level_map: Option<maho_ai::types::ThinkingLevelMap>,
    pub input: Vec<maho_ai::types::InputModality>,
    pub cost: maho_ai::types::ModelCost,
    pub context_window: u64,
    pub max_tokens: u64,
    pub headers: Option<BTreeMap<String, String>>,
    pub extra_body: Option<JsonValue>,
    pub compat: Option<maho_ai::model::ModelCompat>,
}
#[derive(Clone, Default)]
pub struct ProviderConfig {
    pub owns_account_selection: bool,
    pub ambient_auth: Option<Arc<dyn maho_ai::auth::types::ApiKeyAuth>>,
    pub images: BTreeMap<String, Arc<dyn maho_ai::types::ProviderImages>>,
    pub classifiers: BTreeMap<String, Arc<dyn maho_ai::types::ProviderClassifier>>,
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api: Option<String>,
    pub stream_simple: Option<ProviderStream>,
    pub headers: Option<BTreeMap<String, String>>,
    pub extra_body: Option<JsonValue>,
    pub auth_header: Option<bool>,
    pub models: Option<Vec<ProviderModelConfig>>,
    pub refresh_models: Option<ProviderRefresh>,
    pub oauth: Option<Arc<dyn maho_ai::auth::types::OAuthAuth>>,
    pub fallback_eligible: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
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
    fn is_subscription(&self) -> bool {
        false
    }
    fn uses_callback_server(&self) -> Option<bool> {
        None
    }
    fn login<'a>(
        &'a self,
        callbacks: &'a dyn maho_ai::oauth::OAuthLoginCallbacks,
    ) -> ExtensionFuture<'a, maho_ai::oauth::OAuthCredentials>;
    fn refresh_token<'a>(
        &'a self,
        credentials: &'a maho_ai::oauth::OAuthCredentials,
        signal: &'a maho_ai::utils::abort::AbortSignal,
    ) -> ExtensionFuture<'a, maho_ai::oauth::OAuthCredentials>;
    fn get_api_key(&self, credentials: &maho_ai::oauth::OAuthCredentials) -> String;
    fn modify_models(
        &self,
        models: Vec<Model>,
        _: &maho_ai::oauth::OAuthCredentials,
    ) -> Vec<Model> {
        models
    }
    /// Whether `resolve_ambient` is implemented (senpi optional `resolveAmbient`).
    fn has_resolve_ambient(&self) -> bool {
        false
    }
    /// senpi `resolveAmbient`: request auth for credentials that live outside auth.json (an env
    /// token, a CLI login); never consulted once a credential is stored.
    fn resolve_ambient<'a>(
        &'a self,
        _ctx: &'a dyn maho_ai::auth::types::AuthContext,
        _signal: &'a maho_ai::utils::abort::AbortSignal,
    ) -> ExtensionFuture<'a, Option<maho_ai::auth::types::AuthResult>> {
        Box::pin(async { Ok(None) })
    }
}
#[derive(Clone)]
pub enum ProviderRegistration {
    Config {
        name: String,
        config: Box<ProviderConfig>,
    },
    ConfigOptions {
        name: String,
        options: Box<ProviderConfigOptions>,
    },
    Native(Arc<dyn maho_ai::models::Provider>),
}
/// Object-only request fields for callers that want schema constraints at construction.
#[derive(Clone, Default)]
pub struct ProviderObjectConfig {
    pub config: ProviderConfig,
    pub extra_body: Option<BTreeMap<String, JsonValue>>,
    pub model_extra_bodies: BTreeMap<String, BTreeMap<String, JsonValue>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionPresetName {
    FullAccess,
    Workspace,
    ReadOnly,
    Ask,
}
pub type MemoryReporter = Arc<dyn Fn() -> BTreeMap<String, usize> + Send + Sync>;
/// A lazily read extension contribution to an on-demand memory report.
#[derive(Clone)]
pub struct NamedMemoryReporter {
    pub name: String,
    pub reporter: MemoryReporter,
}
/// Sections owned by the report, never by an extension.
/// Includes `tui`, which the upstream builder also publishes as a core section.
pub const RESERVED_MEMORY_REPORT_KEYS: &[&str] = &[
    "sessionId",
    "takenAt",
    "pid",
    "main",
    "kernels",
    "residentStore",
    "tuiRenderCache",
    "tui",
    "heapSnapshot",
    "reporterErrors",
];
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolNamespace {
    pub name: String,
    pub description: Option<String>,
}
pub trait ToolLoadout: Send + Sync {
    fn declared(&self) -> Vec<AgentTool>;
    fn callable(&self) -> Vec<AgentTool>;
    fn registered(&self) -> Vec<AgentTool>;
    fn get_exposure(&self, name: &str) -> Option<ToolExposure>;
    fn get_namespace(&self, name: &str) -> Option<ToolNamespace>;
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolLoadoutChanges {
    pub descriptions: BTreeMap<String, String>,
    pub hidden_declarations: Vec<String>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolResultEventChanges {
    pub content: Option<Vec<ContentBlock>>,
    pub details: Option<JsonValue>,
    pub structured_content: Option<JsonValue>,
    pub is_error: Option<bool>,
    pub usage: Option<Usage>,
}

impl ProviderRegistration {
    pub fn name(&self) -> &str {
        match self {
            Self::Config { name, .. } | Self::ConfigOptions { name, .. } => name,
            Self::Native(provider) => provider.id(),
        }
    }
}
pub trait ExtensionProviderActions: Send + Sync {
    fn register_provider(
        &self,
        registration: ProviderRegistration,
        extension_path: &str,
    ) -> Result<(), ExtensionFailure>;
    fn unregister_provider(&self, name: &str, extension_path: &str)
    -> Result<(), ExtensionFailure>;
}
pub struct ReadClassifierSubscription {
    state: Arc<Mutex<RuntimeState>>,
    id: u64,
}
impl Drop for ReadClassifierSubscription {
    fn drop(&mut self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .read_classifiers
            .retain(|(id, _)| *id != self.id);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionFailure {
    pub message: String,
    pub stack: Option<String>,
    pub class: Option<String>,
    pub code: Option<String>,
}
impl ExtensionFailure {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            stack: None,
            class: None,
            code: None,
        }
    }
    /// senpi `errorKind` = `${error.name}/${error.code}`: keep the machine classification (for
    /// example `ModelsError`/`oauth`) so a caller never has to fingerprint the message for parity.
    pub fn classified(
        message: impl Into<String>,
        class: Option<String>,
        code: Option<String>,
    ) -> Self {
        Self {
            message: message.into(),
            stack: None,
            class,
            code,
        }
    }
}
impl fmt::Display for ExtensionFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ExtensionFailure {}
impl From<String> for ExtensionFailure {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}
impl From<&str> for ExtensionFailure {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EventKind {
    ProjectTrust,
    ResourcesDiscover,
    SessionStart,
    SessionInfoChanged,
    SessionParked,
    SessionResumed,
    SessionBeforeSwitch,
    SessionBeforeFork,
    SessionBeforeReload,
    SessionBeforeCompact,
    SessionCompact,
    SessionCompactFailed,
    SessionShutdown,
    SessionAbort,
    SessionMoveExport,
    SessionMoveImport,
    SessionHoldChanged,
    SessionExtensionsRemoved,
    SessionBeforeTree,
    SessionTree,
    Context,
    BeforeProviderRequest,
    BeforeProviderHeaders,
    AfterProviderResponse,
    BeforeAgentStart,
    AgentStart,
    AgentEnd,
    AgentSettled,
    UiPromptStart,
    UiPromptEnd,
    TurnStart,
    TurnEnd,
    MessageStart,
    MessageUpdate,
    MessageEnd,
    ToolExecutionStart,
    ToolExecutionUpdate,
    ToolExecutionEnd,
    ModelSelect,
    SystemPromptChange,
    ThinkingLevelSelect,
    ToolCall,
    ToolResult,
    UserBash,
    Input,
    InputDisposition,
    ProviderStreamEvent,
    AgentBeforeSettle,
    SessionControlWake,
    ToolActivated,
    HeldWakesReleased,
}
impl EventKind {
    pub const ALL: [Self; 51] = [
        Self::ProjectTrust,
        Self::ResourcesDiscover,
        Self::SessionStart,
        Self::SessionInfoChanged,
        Self::SessionParked,
        Self::SessionResumed,
        Self::SessionBeforeSwitch,
        Self::SessionBeforeFork,
        Self::SessionBeforeReload,
        Self::SessionBeforeCompact,
        Self::SessionCompact,
        Self::SessionCompactFailed,
        Self::SessionShutdown,
        Self::SessionAbort,
        Self::SessionMoveExport,
        Self::SessionMoveImport,
        Self::SessionHoldChanged,
        Self::SessionExtensionsRemoved,
        Self::SessionBeforeTree,
        Self::SessionTree,
        Self::Context,
        Self::BeforeProviderRequest,
        Self::BeforeProviderHeaders,
        Self::AfterProviderResponse,
        Self::BeforeAgentStart,
        Self::AgentStart,
        Self::AgentEnd,
        Self::AgentSettled,
        Self::UiPromptStart,
        Self::UiPromptEnd,
        Self::TurnStart,
        Self::TurnEnd,
        Self::MessageStart,
        Self::MessageUpdate,
        Self::MessageEnd,
        Self::ToolExecutionStart,
        Self::ToolExecutionUpdate,
        Self::ToolExecutionEnd,
        Self::ModelSelect,
        Self::SystemPromptChange,
        Self::ThinkingLevelSelect,
        Self::ToolCall,
        Self::ToolResult,
        Self::UserBash,
        Self::Input,
        Self::InputDisposition,
        Self::ProviderStreamEvent,
        Self::AgentBeforeSettle,
        Self::SessionControlWake,
        Self::ToolActivated,
        Self::HeldWakesReleased,
    ];
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProjectTrust => "project_trust",
            Self::ResourcesDiscover => "resources_discover",
            Self::SessionStart => "session_start",
            Self::SessionInfoChanged => "session_info_changed",
            Self::SessionParked => "session_parked",
            Self::SessionResumed => "session_resumed",
            Self::SessionBeforeSwitch => "session_before_switch",
            Self::SessionBeforeFork => "session_before_fork",
            Self::SessionBeforeReload => "session_before_reload",
            Self::SessionBeforeCompact => "session_before_compact",
            Self::SessionCompact => "session_compact",
            Self::SessionCompactFailed => "session_compact_failed",
            Self::SessionShutdown => "session_shutdown",
            Self::SessionAbort => "session_abort",
            Self::SessionMoveExport => "session_move_export",
            Self::SessionMoveImport => "session_move_import",
            Self::SessionHoldChanged => "session_hold_changed",
            Self::SessionExtensionsRemoved => "session_extensions_removed",
            Self::SessionBeforeTree => "session_before_tree",
            Self::SessionTree => "session_tree",
            Self::Context => "context",
            Self::BeforeProviderRequest => "before_provider_request",
            Self::BeforeProviderHeaders => "before_provider_headers",
            Self::AfterProviderResponse => "after_provider_response",
            Self::BeforeAgentStart => "before_agent_start",
            Self::AgentStart => "agent_start",
            Self::AgentEnd => "agent_end",
            Self::AgentSettled => "agent_settled",
            Self::UiPromptStart => "ui_prompt_start",
            Self::UiPromptEnd => "ui_prompt_end",
            Self::TurnStart => "turn_start",
            Self::TurnEnd => "turn_end",
            Self::MessageStart => "message_start",
            Self::MessageUpdate => "message_update",
            Self::MessageEnd => "message_end",
            Self::ToolExecutionStart => "tool_execution_start",
            Self::ToolExecutionUpdate => "tool_execution_update",
            Self::ToolExecutionEnd => "tool_execution_end",
            Self::ModelSelect => "model_select",
            Self::SystemPromptChange => "system_prompt_change",
            Self::ThinkingLevelSelect => "thinking_level_select",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::UserBash => "user_bash",
            Self::Input => "input",
            Self::InputDisposition => "input_disposition",
            Self::ProviderStreamEvent => "provider_stream_event",
            Self::AgentBeforeSettle => "agent_before_settle",
            Self::SessionControlWake => "session_control_wake",
            Self::ToolActivated => "tool_activated",
            Self::HeldWakesReleased => "held_wakes_released",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == name)
    }
    pub const fn is_session_before(self) -> bool {
        matches!(
            self,
            Self::SessionBeforeSwitch
                | Self::SessionBeforeFork
                | Self::SessionBeforeReload
                | Self::SessionBeforeCompact
                | Self::SessionBeforeTree
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ExtensionMode {
    Tui,
    Rpc,
    Json,
    #[default]
    Print,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceScope {
    User,
    Project,
    #[default]
    Temporary,
    System,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceOrigin {
    Package,
    #[default]
    TopLevel,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub path: String,
    pub source: String,
    pub scope: SourceScope,
    pub origin: SourceOrigin,
    pub base_dir: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillProvenance {
    pub kind: String,
    pub selected_root: String,
    pub project_root: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillAllowedSet {
    pub roots: Vec<String>,
    pub files: Vec<String>,
    pub digest: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub provenance: Option<SkillProvenance>,
    pub allowed_set: Option<SkillAllowedSet>,
    pub name: String,
    pub description: String,
    pub file_path: String,
    pub base_dir: String,
    pub source_info: SourceInfo,
    pub disable_model_invocation: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceDiscoverEntry {
    pub path: String,
    pub scope: Option<SourceScope>,
}
impl From<String> for ResourceDiscoverEntry {
    fn from(path: String) -> Self {
        Self { path, scope: None }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredResourceEntry {
    pub path: String,
    pub scope: Option<SourceScope>,
    pub extension_path: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourcesDiscoverResult {
    pub skill_paths: Vec<ResourceDiscoverEntry>,
    pub prompt_paths: Vec<ResourceDiscoverEntry>,
    pub theme_paths: Vec<ResourceDiscoverEntry>,
    pub hook_paths: Vec<ResourceDiscoverEntry>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiscoveredResources {
    pub skill_paths: Vec<DiscoveredResourceEntry>,
    pub prompt_paths: Vec<DiscoveredResourceEntry>,
    pub theme_paths: Vec<DiscoveredResourceEntry>,
    pub hook_paths: Vec<DiscoveredResourceEntry>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Theme {
    pub name: Option<String>,
    pub colors: BTreeMap<String, String>,
    pub backgrounds: BTreeMap<String, String>,
    pub vars: BTreeMap<String, String>,
}
impl Theme {
    /// senpi's guest `Theme.fg(color, text)`: prefix the text with the named color's exported ANSI
    /// prefix and reset the foreground after it.
    pub fn fg(&self, color: &str, text: &str) -> String {
        let prefix = self
            .colors
            .get(color)
            .map(String::as_str)
            .unwrap_or("\u{1b}[39m");
        format!("{prefix}{text}\u{1b}[39m")
    }
    /// senpi's guest `Theme.bg(color, text)`: prefix the text with the named background's exported
    /// ANSI prefix and reset the background after it.
    pub fn bg(&self, color: &str, text: &str) -> String {
        let prefix = self
            .backgrounds
            .get(color)
            .map(String::as_str)
            .unwrap_or("\u{1b}[49m");
        format!("{prefix}{text}\u{1b}[49m")
    }
    pub fn bold(&self, text: &str) -> String {
        format!("\u{1b}[1m{text}\u{1b}[22m")
    }
}
#[derive(Clone, Debug)]
pub struct ScopedModel {
    pub model: Model,
    pub thinking_level: Option<ThinkingLevel>,
    pub service_tier: Option<ServiceTier>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceTier {
    Auto,
    Flex,
    Priority,
    Ultrafast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialAccountSource {
    Login,
    Import,
    Env,
}
impl CredentialAccountSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::Import => "import",
            Self::Env => "env",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialAccountSummary {
    pub name: String,
    pub display_name: Option<String>,
    pub source: CredentialAccountSource,
    pub blocked: bool,
    pub pinned: bool,
    /// A slot of the other kind than the stored credential: kept, never used, cannot be pinned (senpi#3486).
    pub unused: bool,
}

/// Host implementations adapt their owning registry, without an ext-api -> core edge.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedRequestAuth {
    pub ambient: bool,
    pub auth: maho_ai::models::ProviderAuthResult,
    pub extra_body: Option<serde_json::Map<String, JsonValue>>,
    pub upstream_model_id: Option<String>,
    pub service_tier: Option<maho_ai::types::ServiceTierPreference>,
    pub env: Option<maho_ai::types::ProviderEnv>,
}

/// senpi `registry.getProviderAuthStatus(provider)`: the shared, ext-api-owned status type the
/// component reads (`.source` only). It mirrors `maho_core::provider_composer::AuthStatus`; ext-api
/// owns it so a component never needs an ext-api -> core edge.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderAuthStatus {
    pub ambient: bool,
    pub configured: bool,
    pub source: Option<String>,
    pub label: Option<String>,
}

pub trait ModelRegistry: Send + Sync {
    fn get_all(&self) -> Vec<Model>;
    fn get_available(&self) -> Vec<Model>;
    fn find(&self, provider: &str, id: &str) -> Option<Model>;
    fn upstream_model_id(&self, model: &Model) -> String {
        model
            .upstream_model_id
            .clone()
            .unwrap_or_else(|| model.id.clone())
    }
    fn catalog_service_tier(&self, model: &Model) -> Option<ServiceTier> {
        model.service_tier.map(|tier| match tier {
            maho_ai::types::ServiceTierPreference::Auto => ServiceTier::Auto,
            maho_ai::types::ServiceTierPreference::Flex => ServiceTier::Flex,
            maho_ai::types::ServiceTierPreference::Priority => ServiceTier::Priority,
            maho_ai::types::ServiceTierPreference::Ultrafast => ServiceTier::Ultrafast,
        })
    }
    /// senpi `model.promptPreset`: the per-model preset from models.json `modelOverrides` or a model definition.
    fn prompt_preset(&self, _model: &Model) -> Option<String> {
        None
    }
    fn has_configured_auth(&self, model: &Model) -> bool;
    /// senpi `registry.getProviderAuthStatus(provider)`: the configured/source status. The default
    /// body reports an absent accessor; the production registry overrides it with the runtime source.
    fn get_provider_auth_status(&self, _provider: &str) -> ProviderAuthStatus {
        ProviderAuthStatus::default()
    }
    fn get_api_key_for_provider<'a>(
        &'a self,
        provider: &'a str,
    ) -> ExtensionFuture<'a, Option<String>>;
    fn get_provider_auth<'a>(
        &'a self,
        _provider: &'a str,
    ) -> ExtensionFuture<'a, Option<maho_ai::models::AuthResolution>> {
        Box::pin(async {
            Err(ExtensionFailure::new(
                "Provider auth is not supported by this model registry",
            ))
        })
    }
    /// senpi `runtime.getAuth(provider, { slotName })`: `None` is senpi's `{}` (the flat/default
    /// credential). The default delegates to the flat probe; the production registry overrides it to
    /// thread the slot, so a default body can never silently reintroduce the hidden flat fallback.
    fn get_provider_auth_for_slot<'a>(
        &'a self,
        provider: &'a str,
        _slot_name: Option<&'a str>,
    ) -> ExtensionFuture<'a, Option<maho_ai::models::AuthResolution>> {
        self.get_provider_auth(provider)
    }
    fn get_stored_credential_type(
        &self,
        _provider: &str,
    ) -> Result<Option<maho_ai::auth::types::CredentialType>, ExtensionFailure> {
        Err(ExtensionFailure::new(
            "Stored credential metadata is not supported by this model registry",
        ))
    }
    fn stream_simple(
        &self,
        _model: &Model,
        _context: &maho_ai::types::Context,
        _options: Option<maho_ai::types::SimpleStreamOptions>,
    ) -> Result<maho_ai::utils::event_stream::AssistantMessageEventStream, ExtensionFailure> {
        Err(ExtensionFailure::new(
            "Configured streaming is not supported by this model registry",
        ))
    }
    /// senpi `modelRegistry.modelRuntime.stream`: dispatch through the runtime so providers
    /// registered by extensions resolve. The default is the builtin api-registry stream, matching
    /// senpi's behavior for a context constructed without a registry runtime.
    fn stream(
        &self,
        model: &Model,
        context: &maho_ai::types::Context,
        options: Option<maho_ai::types::StreamOptions>,
    ) -> Result<maho_ai::utils::event_stream::AssistantMessageEventStream, ExtensionFailure> {
        Ok(maho_ai::compat::stream(model, context, options))
    }
    fn get_api_key_and_headers<'a>(
        &'a self,
        _model: &'a Model,
    ) -> ExtensionFuture<'a, ResolvedRequestAuth> {
        Box::pin(async {
            Err(ExtensionFailure::new(
                "Model request auth is not supported by this model registry",
            ))
        })
    }
    /// senpi `runtime.getAuth(model, { slotName })`: the MODEL overload with the SAME slot the
    /// provider scope resolved on, so the model's own headers resolve against that account. The
    /// default delegates to the flat model probe; the production registry overrides it.
    fn get_api_key_and_headers_for_slot<'a>(
        &'a self,
        model: &'a Model,
        _slot_name: Option<&'a str>,
    ) -> ExtensionFuture<'a, ResolvedRequestAuth> {
        self.get_api_key_and_headers(model)
    }
    fn get_credential_accounts<'a>(
        &'a self,
        _provider: &'a str,
    ) -> ExtensionFuture<'a, Vec<CredentialAccountSummary>> {
        Box::pin(async {
            Err(ExtensionFailure::new(
                "Credential account listing is not supported by this model registry",
            ))
        })
    }
    fn pin_credential_account<'a>(
        &'a self,
        _provider: &'a str,
        _name: Option<&'a str>,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async {
            Err(ExtensionFailure::new(
                "Credential account pinning is not supported by this model registry",
            ))
        })
    }
    fn reset_credential_account<'a>(
        &'a self,
        _provider: &'a str,
        _name: &'a str,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async {
            Err(ExtensionFailure::new(
                "Credential account reset is not supported by this model registry",
            ))
        })
    }
    fn remove_credential_account<'a>(
        &'a self,
        _provider: &'a str,
        _name: &'a str,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async {
            Err(ExtensionFailure::new(
                "Credential account removal is not supported by this model registry",
            ))
        })
    }
    fn rename_credential_account<'a>(
        &'a self,
        _provider: &'a str,
        _name: &'a str,
        _display_name: Option<&'a str>,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async {
            Err(ExtensionFailure::new(
                "Credential account renaming is not supported by this model registry",
            ))
        })
    }
    /// Pinned `ModelRegistry.refresh` (`core/model-registry.ts`): reload the provider catalog and
    /// refresh the selected providers' models, returning the pinned `ModelsRefreshResult` (aborted
    /// flag plus per-provider errors) rather than rejecting. The llama.cpp `/llama` flow refreshes
    /// its provider catalog after every server interaction.
    fn refresh<'a>(
        &'a self,
        _options: maho_ai::models::ModelsRefreshOptions,
    ) -> ExtensionFuture<'a, maho_ai::models::ModelsRefreshResult> {
        Box::pin(async {
            Err(ExtensionFailure::new(
                "Model catalog refresh is not supported by this model registry",
            ))
        })
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct SessionEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub kind: String,
    pub data: JsonValue,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkProvenance {
    pub parent_session: String,
    pub inherited_through_entry_id: Option<String>,
}
pub trait SessionManager: ToolSessionManager {
    fn get_session_dir(&self) -> Option<PathBuf> {
        None
    }
    fn get_entries(&self) -> Vec<SessionEntry>;
    fn get_branch(&self) -> Vec<SessionEntry>;
    /// Folds `reduce` over the resident entries, oldest first, starting from `initial`.
    fn reduce_history(
        &self,
        initial: JsonValue,
        reduce: &mut dyn FnMut(JsonValue, &SessionEntry) -> JsonValue,
    ) -> JsonValue {
        self.get_entries().iter().fold(initial, &mut *reduce)
    }
    fn get_usage_totals(&self) -> Option<JsonValue> {
        None
    }
    /// The newest suffix of `get_branch()` plus the TRUE branch length. The suffix holds at least
    /// `min_entries` entries (or the whole branch) and extends further back until `satisfied`,
    /// called on each entry's `data` newest-first, has returned `true` once. Implementations may
    /// walk from the leaf instead of materializing the whole branch.
    fn get_branch_tail(
        &self,
        min_entries: usize,
        satisfied: &mut dyn FnMut(&JsonValue) -> bool,
    ) -> (usize, Vec<SessionEntry>) {
        let mut branch = self.get_branch();
        let len = branch.len();
        let mut start = len;
        let mut done = false;
        while start > 0 && (len - start < min_entries || !done) {
            start -= 1;
            if !done {
                done = satisfied(&branch[start].data);
            }
        }
        (len, branch.split_off(start))
    }
    fn get_leaf_id(&self) -> Option<String>;
    fn get_session_name(&self) -> Option<String>;
    fn is_child_session(&self) -> bool {
        self.fork_provenance().is_some()
    }
    fn fork_provenance(&self) -> Option<ForkProvenance> {
        None
    }
    fn extension_context_actions(&self) -> Option<&dyn ExtensionContextActions> {
        None
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextUsage {
    pub tokens: Option<u64>,
    pub context_window: u64,
    pub percent: Option<f64>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReloadVetoDecision {
    pub cancelled: bool,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct PromptCacheKeepAliveSettings {
    pub enabled: bool,
    pub max_requests_per_session: u64,
    pub max_cost_usd_per_session: f64,
    pub margin_seconds: f64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LookAtSettings {
    pub enabled: bool,
    pub models: Option<Vec<String>>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct AskUserSettings {
    pub enabled: bool,
    pub timeout_minutes: f64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageSettings {
    pub auto_resize: bool,
    pub block_images: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedHookSources {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub global_hooks_path: PathBuf,
    pub project_hooks_path: PathBuf,
    pub global_settings_hooks: Option<JsonValue>,
    pub project_settings_hooks: Option<JsonValue>,
    pub global_hook_source_paths: Vec<PathBuf>,
    pub project_hook_source_paths: Vec<PathBuf>,
    pub pre_session_hook_source_paths: Vec<PathBuf>,
    pub runtime_hook_source_paths: Vec<PathBuf>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackRevertPolicy {
    CooldownExpiry,
    Never,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryFallbackSettings {
    pub model_fallback: bool,
    pub chains: BTreeMap<String, Vec<String>>,
    pub revert_policy: FallbackRevertPolicy,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryFallbackStatus {
    pub active: bool,
    pub current_model: Option<String>,
    pub original_selector: Option<String>,
    pub pinned: bool,
}
pub trait ExtensionSessionSettings: Send + Sync {
    fn get_retry_fallback_settings(&self) -> RetryFallbackSettings;
    fn set_fallback_chain<'a>(
        &'a self,
        key: &'a str,
        entries: &'a [String],
    ) -> ExtensionFuture<'a, ()>;
    fn remove_fallback_chain<'a>(&'a self, key: &'a str) -> ExtensionFuture<'a, ()>;
    fn set_model_fallback_enabled(&self, enabled: bool) -> ExtensionFuture<'_, ()>;
    fn set_fallback_revert_policy(&self, policy: FallbackRevertPolicy) -> ExtensionFuture<'_, ()>;
    fn reload(&self) -> ExtensionFuture<'_, ()>;
    fn get_fallback_status(&self) -> Option<RetryFallbackStatus>;
}
#[derive(Clone, Default)]
pub struct CompactOptions {
    pub custom_instructions: Option<String>,
    pub on_complete: Option<Arc<dyn Fn(CompactionResult) + Send + Sync>>,
    pub on_error: Option<Arc<dyn Fn(ExtensionFailure) + Send + Sync>>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WarmAnchorSnapshot {
    pub first_kept_entry_id: String,
    pub prefix_entry_ids: Vec<String>,
    pub latest_compaction_entry_id: Option<String>,
}
#[derive(Clone, Debug)]
pub struct ApplyCompactionOptions {
    pub reason: CompactionReason,
    pub expected_revision: Option<u64>,
    pub expected_warm_anchor: Option<WarmAnchorSnapshot>,
    pub signal: Option<AbortSignal>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyCompactionResult {
    Applied,
    Stale,
    Rejected,
}
#[derive(Clone, Debug, PartialEq)]
pub struct FastModeResult {
    pub enabled: bool,
    pub applied: bool,
    pub recorded_tier: ServiceTier,
    pub message: String,
    pub memory_model: Option<Model>,
}
pub type FastModeAction =
    Arc<dyn Fn(bool, ExtensionContext) -> ExtensionFuture<'static, FastModeResult> + Send + Sync>;
#[derive(Clone, Debug)]
pub struct BeginCompactionOptions {
    pub reason: CompactionReason,
}
#[derive(Clone, Debug)]
pub struct UpdateCompactionOptions {
    pub reason: CompactionReason,
    pub signal: Option<AbortSignal>,
    pub delta: Option<String>,
    pub text: Option<String>,
}
#[derive(Clone, Debug)]
pub struct EndCompactionOptions {
    pub reason: CompactionReason,
    pub signal: Option<AbortSignal>,
    pub aborted: Option<bool>,
    pub error_message: Option<String>,
}
pub type ProviderHeaderTransform = Arc<
    dyn Fn(
            BTreeMap<String, Option<String>>,
        ) -> ExtensionFuture<'static, BTreeMap<String, Option<String>>>
        + Send
        + Sync,
>;
pub struct ProviderRequestPreparation {
    pub messages: Vec<AgentMessage>,
    pub transform_payload:
        Arc<dyn Fn(JsonValue) -> ExtensionFuture<'static, JsonValue> + Send + Sync>,
    pub transform_headers: ProviderHeaderTransform,
}
#[derive(Clone, Debug, Default)]
pub struct KernelToolInvokeScope {
    pub allow: Option<Vec<String>>,
    pub deny: Option<Vec<String>>,
}
#[derive(Clone, Debug, Default)]
pub struct KernelToolInvokeOptions {
    pub signal: Option<AbortSignal>,
    pub scope: Option<KernelToolInvokeScope>,
}
#[derive(Clone, Debug)]
pub struct KernelToolInvokeRequest {
    pub name: String,
    pub kernel_generation: u64,
    pub definition_revision: u64,
    pub args: JsonValue,
    pub call_id: String,
}
pub trait ExtensionKernelTools: Send + Sync {
    fn invoke_scope(&self) -> bool;
    fn describe<'a>(&'a self, names: &'a [String]) -> ExtensionFuture<'a, JsonValue>;
    fn invoke(
        &self,
        request: KernelToolInvokeRequest,
        options: KernelToolInvokeOptions,
    ) -> ExtensionFuture<'_, JsonValue>;
}
tokio::task_local! {
    static ACTIVE_KERNEL_TOOLS: Option<Arc<dyn ExtensionKernelTools>>;
}
pub fn current_kernel_tools() -> Option<Arc<dyn ExtensionKernelTools>> {
    ACTIVE_KERNEL_TOOLS.try_with(Clone::clone).ok().flatten()
}
pub async fn with_kernel_tools<F: std::future::Future>(
    capability: Option<Arc<dyn ExtensionKernelTools>>,
    future: F,
) -> F::Output {
    ACTIVE_KERNEL_TOOLS.scope(capability, future).await
}
pub fn with_kernel_tools_sync<T>(
    capability: Option<Arc<dyn ExtensionKernelTools>>,
    operation: impl FnOnce() -> T,
) -> T {
    ACTIVE_KERNEL_TOOLS.sync_scope(capability, operation)
}
pub trait ExtensionContextActions: Send + Sync {
    fn assert_active(&self) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn set_approved_monitor_parent(
        &self,
        _tool_call_id: &str,
        _input: &JsonValue,
        _parent: &Path,
    ) -> Result<(), ExtensionFailure> {
        Err(ExtensionFailure::new(
            "Monitor admission attachment is not supported by this extension context",
        ))
    }
    fn take_approved_monitor_parent(
        &self,
        _tool_call_id: &str,
        _input: &JsonValue,
    ) -> Result<Option<PathBuf>, ExtensionFailure> {
        Err(ExtensionFailure::new(
            "Monitor admission identity is not supported by this extension context",
        ))
    }
    /// senpi `getBrowserEngine`: the session's browser engine (`connected` / `builtin` / `none`).
    fn get_browser_engine(&self) -> Option<&'static str> {
        None
    }
    fn get_model(&self) -> Option<Model>;
    fn get_service_tier(&self) -> Option<ServiceTier>;
    fn get_effective_service_tier(&self) -> Option<ServiceTier> {
        self.get_service_tier()
    }
    /// The shared `applyFastMode` behind `/fast` and RPC `set_fast_mode`.
    fn apply_fast_mode(
        &self,
        _enabled: bool,
        _context: ExtensionContext,
    ) -> ExtensionFuture<'static, FastModeResult> {
        Box::pin(async {
            Err(ExtensionFailure::new(
                "Fast mode is not available in this session",
            ))
        })
    }
    fn get_scoped_models(&self) -> Vec<ScopedModel>;
    fn get_agent_dir(&self) -> PathBuf;
    fn is_idle(&self) -> bool;
    /// True while engine turns are held (unattended, or `session_start` handlers still running).
    fn engine_turns_held(&self) -> bool {
        false
    }
    /// Records a wake that was held rather than delivered.
    fn note_held_wake(&self, _source: &str) {}
    fn get_pause_state(&self) -> JsonValue {
        serde_json::json!({"pauseSeq": 0, "holds": []})
    }
    fn fence_run(&self, _run_ref: JsonValue) -> Result<(), ExtensionFailure> {
        Err(ExtensionFailure::new("Run fencing is unavailable"))
    }
    fn supports_release_run_fence(&self) -> bool {
        false
    }
    fn release_run_fence(&self, _run_ref: JsonValue) -> Result<(), ExtensionFailure> {
        Err(ExtensionFailure::new("Run fence release is unavailable"))
    }
    /// Runs a registered child module; `None` when no session backs this context.
    fn offload(
        &self,
        _id: &str,
        _args: JsonValue,
        _options: OffloadOptions,
    ) -> Option<ExtensionFuture<'_, JsonValue>> {
        None
    }
    /// Present only when the host reports question answers delivered as person input.
    fn question_input(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        None
    }
    fn is_project_trusted(&self) -> bool;
    fn get_signal(&self) -> Option<AbortSignal>;
    fn get_steering_signal(&self) -> Option<AbortSignal> {
        None
    }
    fn get_thinking_level(&self) -> Option<ThinkingLevel> {
        None
    }
    fn abort(&self, source: Option<AbortSource>);
    fn has_pending_messages(&self) -> bool;
    /// Person input releases holds; extension-originated messages do not.
    fn has_queued_person_input(&self) -> bool {
        false
    }
    fn todo_turn_end_backstop_enabled(&self) -> bool {
        true
    }
    fn request_reload(&self) -> ExtensionFuture<'_, ()>;
    fn is_compacting(&self) -> bool;
    fn check_reload_veto(&self) -> ExtensionFuture<'_, ReloadVetoDecision>;
    fn shutdown(&self);
    fn get_context_usage(&self) -> Option<ContextUsage>;
    fn get_compaction_settings(&self) -> CompactionSettings;
    /// senpi `ctx.getCompactionModel()`: the summarizer model (`compaction.model` override when usable).
    fn get_compaction_model(&self) -> Option<Model> {
        None
    }
    fn get_resolved_compaction_settings(&self) -> Option<ResolvedCompactionSettings> {
        None
    }
    fn get_compaction_preparation(&self) -> Option<CompactionPreparationDetails> {
        None
    }
    fn get_prompt_cache_safe_wait_seconds(&self) -> Option<f64>;
    fn get_prompt_cache_goal_backstop_max_seconds(&self) -> f64;
    fn get_prompt_cache_keep_alive_settings(&self) -> PromptCacheKeepAliveSettings;
    fn get_look_at_settings(&self) -> LookAtSettings;
    fn get_ask_user_settings(&self) -> AskUserSettings;
    fn get_image_settings(&self) -> ImageSettings;
    fn session_settings(&self) -> &dyn ExtensionSessionSettings;
    fn compact(&self, options: CompactOptions);
    fn prepare_provider_request(
        &self,
        messages: Vec<AgentMessage>,
    ) -> ExtensionFuture<'_, ProviderRequestPreparation>;
    fn begin_compaction(&self, options: BeginCompactionOptions) -> Option<AbortSignal>;
    fn update_compaction(&self, options: UpdateCompactionOptions);
    fn end_compaction(&self, options: EndCompactionOptions);
    fn get_message_revision(&self) -> u64;
    fn apply_compaction(
        &self,
        result: CompactionResult,
        options: ApplyCompactionOptions,
    ) -> ExtensionFuture<'_, ApplyCompactionResult>;
    fn get_system_prompt(&self) -> String;
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions;
    fn get_loaded_hook_sources(&self) -> LoadedHookSources;
    fn get_registered_mcp_servers(&self) -> Option<Vec<RegisteredMcpServerDeclaration>> {
        None
    }
    fn kernel_tools(&self) -> Option<&dyn ExtensionKernelTools>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WidgetPlacement {
    #[default]
    AboveEditor,
    BelowEditor,
}
impl WidgetPlacement {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AboveEditor => "aboveEditor",
            Self::BelowEditor => "belowEditor",
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct ExtensionUiDialogOptions {
    pub signal: Option<AbortSignal>,
    pub timeout_ms: Option<u64>,
}
/// A clickable action shown beside a string-array widget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionWidgetAction {
    pub label: String,
    pub command: String,
}
#[derive(Clone, Debug, Default)]
pub struct ExtensionWidgetOptions {
    pub placement: WidgetPlacement,
    /// Clickable command buttons shown beside a string-array widget.
    pub actions: Option<Vec<ExtensionWidgetAction>>,
}
#[derive(Clone, Debug, Default)]
pub struct WorkingIndicatorOptions {
    pub frames: Option<Vec<String>>,
    pub interval_ms: Option<u64>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalInputResult {
    pub consume: Option<bool>,
    pub data: Option<String>,
}
pub type TerminalInputHandler = Arc<dyn Fn(&str) -> Option<TerminalInputResult> + Send + Sync>;
pub type UiUnsubscribe = Box<dyn FnOnce() + Send>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionRequest {
    pub request_id: String,
    pub questions: Vec<Question>,
    pub wait_for_answer: bool,
    pub timeout_ms: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestionStatus {
    Answered,
    CommentSubmitted,
    TimedOut,
    Cancelled,
    OrphanedAfterRestart,
    Unavailable,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuestionAnswer {
    pub selected: Vec<String>,
    pub text: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionCancelOrigin {
    User,
    UiFailure,
    TurnAbort,
    SessionClosed,
    AskUserDisabled,
}

impl QuestionCancelOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::UiFailure => "ui_failure",
            Self::TurnAbort => "turn_abort",
            Self::SessionClosed => "session_closed",
            Self::AskUserDisabled => "ask_user_disabled",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionResolvedBy {
    LocalUi,
    RpcConnection,
    ControlEndpoint,
}

impl QuestionResolvedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalUi => "local_ui",
            Self::RpcConnection => "rpc_connection",
            Self::ControlEndpoint => "control_endpoint",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionResponse {
    pub status: QuestionStatus,
    pub answers: BTreeMap<String, QuestionAnswer>,
    pub comment: Option<String>,
    pub unanswered: Vec<String>,
    pub auto_resolved_after_ms: Option<u64>,
    pub cancel_origin: Option<QuestionCancelOrigin>,
    pub resolved_by: Option<QuestionResolvedBy>,
}

pub const CANCELLED_BY_USER: &str = "The user cancelled the question after answering part of it.";
pub const CANCELLED_AFTER_ANSWERS: &str =
    "The question was cancelled after part of it was answered.";

pub fn is_cancelled_with_answers_line(line: &str) -> bool {
    line == CANCELLED_BY_USER || line == CANCELLED_AFTER_ANSWERS
}

pub fn shows_partial_answers(response: &QuestionResponse) -> bool {
    if response.status != QuestionStatus::Cancelled {
        return false;
    }
    if !matches!(
        response.cancel_origin,
        Some(QuestionCancelOrigin::User | QuestionCancelOrigin::UiFailure)
    ) {
        return false;
    }
    response.answers.values().any(|answer| {
        !answer.selected.is_empty()
            || answer
                .text
                .as_ref()
                .is_some_and(|text| !text.trim().is_empty())
    })
}

#[derive(Clone, Debug, Default)]
pub struct QuestionDraft {
    pub answers: Option<BTreeMap<String, QuestionAnswer>>,
    pub comment: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum QuestionDelivery {
    #[default]
    ToolResult,
    UserMessage,
}
#[derive(Clone, Default)]
pub struct QuestionOptions {
    pub dialog: ExtensionUiDialogOptions,
    pub on_progress: Option<Arc<dyn Fn(QuestionDraft) + Send + Sync>>,
    pub deliver: QuestionDelivery,
    /// Fixed wall-clock hard cap, captured when the pending timer is created.
    pub hard_deadline_at_ms: Option<u64>,
    /// Authoritative live wall-clock idle deadline; UI countdowns are display-only.
    pub get_deadline_at_ms: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
    pub initial_draft: Option<QuestionDraft>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeInfo {
    pub name: String,
    pub path: Option<PathBuf>,
}
#[derive(Clone, Debug)]
pub enum ThemeSelection {
    Name(String),
    Theme(Theme),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetThemeResult {
    pub success: bool,
    pub error: Option<String>,
}
pub type AutocompleteProviderFactory = Arc<
    dyn Fn(
            Box<dyn maho_tui::autocomplete::AutocompleteProvider>,
        ) -> Box<dyn maho_tui::autocomplete::AutocompleteProvider>
        + Send
        + Sync,
>;
pub type EditorFactory = Arc<
    dyn Fn(
            std::rc::Rc<dyn maho_tui::components::editor::EditorTuiHost>,
            maho_tui::components::editor::EditorTheme,
            &maho_tui::keybindings::KeybindingsManager,
        ) -> Box<dyn maho_tui::editor_component::EditorComponent>
        + Send
        + Sync,
>;
pub trait ExtensionUiActions: Send + Sync {
    fn question(
        &self,
        request: QuestionRequest,
        options: QuestionOptions,
    ) -> ExtensionFuture<'_, QuestionResponse>;
    fn on_terminal_input(&self, handler: TerminalInputHandler) -> UiUnsubscribe;
    fn set_working_message(&self, message: Option<&str>);
    fn set_working_visible(&self, visible: bool);
    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>);
    fn set_hidden_thinking_label(&self, label: Option<&str>);
    fn editor<'a>(
        &'a self,
        title: &'a str,
        prefill: Option<&'a str>,
    ) -> ExtensionFuture<'a, Option<String>>;
    fn add_autocomplete_provider(&self, factory: AutocompleteProviderFactory);
    fn set_editor_component(&self, factory: Option<EditorFactory>);
    fn get_editor_component(&self) -> Option<EditorFactory>;
    fn get_all_themes(&self) -> Vec<ThemeInfo>;
    fn get_theme(&self, name: &str) -> Option<Theme>;
    fn set_theme(&self, theme: ThemeSelection) -> SetThemeResult;
    fn get_tools_expanded(&self) -> bool;
    fn set_tools_expanded(&self, expanded: bool);
    fn set_subagent_cost(&self, _cost_usd: Option<f64>) {}
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationType {
    Info,
    Warning,
    Error,
}
/// Components retain senpi's render/invalidate/input contract; creation occurs on the UI thread.
pub type ComponentFactory = Arc<dyn Fn(&Theme) -> Box<dyn Component> + Send + Sync>;
/// A worker-owned column. Only its factory crosses the UI queue; native components stay on
/// the terminal thread, alongside the transcript's existing scroll/layout tree.
#[derive(Clone)]
pub struct ExtensionSideColumn {
    pub factory: ComponentFactory,
    pub width: Arc<dyn Fn(usize) -> usize + Send + Sync>,
    pub min_columns: usize,
    pub min_transcript_columns: usize,
}
pub trait ExtensionTuiHost {
    fn request_render(&self);
    fn dimensions(&self) -> (u16, u16);
    fn dimensions_reader(&self) -> std::rc::Rc<dyn Fn() -> (u16, u16)> {
        let dimensions = self.dimensions();
        std::rc::Rc::new(move || dimensions)
    }
    fn full_redraws(&self) -> u64 {
        0
    }
}
pub trait ReadonlyFooterDataProvider {
    fn get_git_branch(&self) -> Option<String>;
    fn get_extension_statuses(&self) -> BTreeMap<String, String>;
    fn get_available_provider_count(&self) -> usize;
    fn get_subagent_cost(&self) -> Option<f64> {
        None
    }
    fn on_branch_change(&self, callback: Arc<dyn Fn() + Send + Sync>) -> UiUnsubscribe;
}
pub type TuiComponentFactory =
    Arc<dyn Fn(&dyn ExtensionTuiHost, &Theme) -> Box<dyn Component> + Send + Sync>;
pub type FooterComponentFactory = Arc<
    dyn Fn(&dyn ExtensionTuiHost, &Theme, &dyn ReadonlyFooterDataProvider) -> Box<dyn Component>
        + Send
        + Sync,
>;
pub type CustomUiDone = std::rc::Rc<dyn Fn(JsonValue)>;
pub type CustomComponentFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn Component>, ExtensionFailure>> + 'a>>;
pub type CustomComponentFactory = Arc<
    dyn for<'a> Fn(
            &'a dyn ExtensionTuiHost,
            &'a Theme,
            &'a maho_tui::keybindings::KeybindingsManager,
            CustomUiDone,
        ) -> CustomComponentFuture<'a>
        + Send
        + Sync,
>;
pub type OverlayOptionsFactory = Arc<dyn Fn() -> maho_tui::tui::OverlayOptions + Send + Sync>;
#[derive(Clone)]
pub enum ExtensionOverlayOptions {
    Static(OverlayOptionsFactory),
    Dynamic(OverlayOptionsFactory),
}
#[derive(Clone, Default)]
pub struct CustomUiFactoryOptions {
    pub overlay: bool,
    pub overlay_options: Option<ExtensionOverlayOptions>,
    pub on_handle: Option<Arc<dyn Fn(maho_tui::tui::OverlayHandle) + Send + Sync>>,
}
pub trait ExtensionUiFactories: Send + Sync {
    fn set_widget_factory(
        &self,
        key: &str,
        factory: Option<TuiComponentFactory>,
        options: ExtensionWidgetOptions,
    );
    fn set_header_factory(&self, factory: Option<TuiComponentFactory>);
    fn set_footer_factory(&self, factory: Option<FooterComponentFactory>);
    fn custom_factory(
        &self,
        factory: CustomComponentFactory,
        options: CustomUiFactoryOptions,
    ) -> ExtensionFuture<'_, JsonValue>;
}
#[derive(Clone)]
pub enum WidgetContent {
    Lines(Vec<String>),
    Component(ComponentFactory),
}
#[derive(Clone, Debug, Default)]
pub struct CustomUiOptions {
    pub overlay: bool,
    pub overlay_options: Option<JsonValue>,
}
pub trait ExtensionUi: Send + Sync {
    /// Optional footer capability; unsupported engines must not trigger a tree scan.
    fn supports_subagent_cost(&self) -> bool {
        false
    }
    fn set_subagent_cost(&self, _cost: Option<f64>) {}
    fn actions(&self) -> Option<&dyn ExtensionUiActions> {
        None
    }
    fn factories(&self) -> Option<&dyn ExtensionUiFactories> {
        None
    }
    fn set_widget_factory(
        &self,
        key: &str,
        factory: Option<TuiComponentFactory>,
        options: ExtensionWidgetOptions,
    ) -> Result<(), ExtensionFailure> {
        self.factories()
            .ok_or_else(|| ExtensionFailure::new("Widget factories are not available"))?
            .set_widget_factory(key, factory, options);
        Ok(())
    }
    fn set_header_factory(
        &self,
        factory: Option<TuiComponentFactory>,
    ) -> Result<(), ExtensionFailure> {
        self.factories()
            .ok_or_else(|| ExtensionFailure::new("Header factories are not available"))?
            .set_header_factory(factory);
        Ok(())
    }
    fn set_footer_factory(
        &self,
        factory: Option<FooterComponentFactory>,
    ) -> Result<(), ExtensionFailure> {
        self.factories()
            .ok_or_else(|| ExtensionFailure::new("Footer factories are not available"))?
            .set_footer_factory(factory);
        Ok(())
    }
    fn custom_factory(
        &self,
        factory: CustomComponentFactory,
        options: CustomUiFactoryOptions,
    ) -> ExtensionFuture<'_, JsonValue> {
        match self.factories() {
            Some(factories) => factories.custom_factory(factory, options),
            None => Box::pin(async {
                Err(ExtensionFailure::new(
                    "Custom UI factories are not available",
                ))
            }),
        }
    }
    fn question(
        &self,
        request: QuestionRequest,
        options: QuestionOptions,
    ) -> ExtensionFuture<'_, QuestionResponse> {
        match self.actions() {
            Some(actions) => actions.question(request, options),
            None => Box::pin(async move {
                Ok(QuestionResponse {
                    status: QuestionStatus::Unavailable,
                    answers: BTreeMap::new(),
                    comment: None,
                    unanswered: request
                        .questions
                        .into_iter()
                        .map(|question| question.id)
                        .collect(),
                    auto_resolved_after_ms: None,
                    cancel_origin: None,
                    resolved_by: None,
                })
            }),
        }
    }
    fn on_terminal_input(
        &self,
        handler: TerminalInputHandler,
    ) -> Result<UiUnsubscribe, ExtensionFailure> {
        Ok(self
            .actions()
            .ok_or_else(|| ExtensionFailure::new("Terminal input is not available"))?
            .on_terminal_input(handler))
    }
    fn set_working_message(&self, message: Option<&str>) -> Result<(), ExtensionFailure> {
        self.actions()
            .ok_or_else(|| ExtensionFailure::new("Working indicator is not available"))?
            .set_working_message(message);
        Ok(())
    }
    fn set_working_visible(&self, visible: bool) -> Result<(), ExtensionFailure> {
        self.actions()
            .ok_or_else(|| ExtensionFailure::new("Working indicator is not available"))?
            .set_working_visible(visible);
        Ok(())
    }
    fn set_working_indicator(
        &self,
        options: Option<WorkingIndicatorOptions>,
    ) -> Result<(), ExtensionFailure> {
        self.actions()
            .ok_or_else(|| ExtensionFailure::new("Working indicator is not available"))?
            .set_working_indicator(options);
        Ok(())
    }
    fn set_hidden_thinking_label(&self, label: Option<&str>) -> Result<(), ExtensionFailure> {
        self.actions()
            .ok_or_else(|| ExtensionFailure::new("Thinking label is not available"))?
            .set_hidden_thinking_label(label);
        Ok(())
    }
    fn editor<'a>(
        &'a self,
        title: &'a str,
        prefill: Option<&'a str>,
    ) -> ExtensionFuture<'a, Option<String>> {
        match self.actions() {
            Some(actions) => actions.editor(title, prefill),
            None => Box::pin(async { Err(ExtensionFailure::new("Editor is not available")) }),
        }
    }
    fn add_autocomplete_provider(
        &self,
        factory: AutocompleteProviderFactory,
    ) -> Result<(), ExtensionFailure> {
        self.actions()
            .ok_or_else(|| ExtensionFailure::new("Autocomplete is not available"))?
            .add_autocomplete_provider(factory);
        Ok(())
    }
    fn set_editor_component(&self, factory: Option<EditorFactory>) -> Result<(), ExtensionFailure> {
        self.actions()
            .ok_or_else(|| ExtensionFailure::new("Custom editor is not available"))?
            .set_editor_component(factory);
        Ok(())
    }
    fn get_editor_component(&self) -> Result<Option<EditorFactory>, ExtensionFailure> {
        Ok(self
            .actions()
            .ok_or_else(|| ExtensionFailure::new("Custom editor is not available"))?
            .get_editor_component())
    }
    fn get_all_themes(&self) -> Result<Vec<ThemeInfo>, ExtensionFailure> {
        Ok(self
            .actions()
            .ok_or_else(|| ExtensionFailure::new("Theme catalog is not available"))?
            .get_all_themes())
    }
    fn get_theme(&self, name: &str) -> Result<Option<Theme>, ExtensionFailure> {
        Ok(self
            .actions()
            .ok_or_else(|| ExtensionFailure::new("Theme catalog is not available"))?
            .get_theme(name))
    }
    fn set_theme(&self, theme: ThemeSelection) -> Result<SetThemeResult, ExtensionFailure> {
        Ok(self
            .actions()
            .ok_or_else(|| ExtensionFailure::new("Theme selection is not available"))?
            .set_theme(theme))
    }
    fn get_tools_expanded(&self) -> Result<bool, ExtensionFailure> {
        Ok(self
            .actions()
            .ok_or_else(|| ExtensionFailure::new("Tool expansion is not available"))?
            .get_tools_expanded())
    }
    fn set_tools_expanded(&self, expanded: bool) -> Result<(), ExtensionFailure> {
        self.actions()
            .ok_or_else(|| ExtensionFailure::new("Tool expansion is not available"))?
            .set_tools_expanded(expanded);
        Ok(())
    }
    /// Send-safe repaint request. Extension workers (async HTTP backends on non-UI threads) hold an
    /// `Arc<dyn ExtensionUi>` and cannot capture the non-`Send` `ExtensionTuiHost`; this is the
    /// capability-gated seam for them. A context with no repaint path returns an error rather than
    /// silently pretending the frame was scheduled.
    fn request_render(&self) -> Result<(), ExtensionFailure> {
        Err(ExtensionFailure::new("Repaint requests are not available"))
    }
    fn set_side_column(
        &self,
        _column: Option<ExtensionSideColumn>,
    ) -> Result<(), ExtensionFailure> {
        Err(ExtensionFailure::new(
            "Native side columns are not available",
        ))
    }
    fn select<'a>(
        &'a self,
        title: &'a str,
        options: &'a [String],
        opts: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>>;
    fn confirm<'a>(
        &'a self,
        title: &'a str,
        message: &'a str,
        opts: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool>;
    fn input<'a>(
        &'a self,
        title: &'a str,
        placeholder: Option<&'a str>,
        opts: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>>;
    fn notify(&self, message: &str, kind: NotificationType);
    fn set_status(&self, key: &str, text: Option<&str>);
    fn set_widget(
        &self,
        key: &str,
        content: Option<WidgetContent>,
        options: ExtensionWidgetOptions,
    );
    fn set_header(&self, factory: Option<ComponentFactory>);
    fn set_footer(&self, factory: Option<ComponentFactory>);
    fn set_title(&self, title: &str);
    fn paste_to_editor(&self, text: &str);
    fn set_editor_text(&self, text: &str);
    fn get_editor_text(&self) -> String;
    fn custom(
        &self,
        factory: ComponentFactory,
        options: CustomUiOptions,
    ) -> ExtensionFuture<'_, JsonValue>;
    fn theme(&self) -> Theme;
}

/// One queued idle-edge injection (upstream `IdleInjection`, idle-injection-coordinator.ts).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Upstream source set. The PINNED `idle-injection-coordinator.ts` (77f3067f1) carries the first six;
/// `WorkpoolAggregate` and `Kibitzer` are LATEST additions (audit checkout 20261006), selected for the
/// recall production integration and recorded as latest, NOT pinned parity.
pub enum IdleInjectionSource {
    TaskCompletion,
    /// Latest (`workpool-aggregate`); ranked with `TaskCompletion`.
    WorkpoolAggregate,
    TeamMessage,
    TeamLiveness,
    BoulderContinuation,
    UlwContinuation,
    DagRun,
    /// Latest (`kibitzer`); ranked last, always queued `passive`.
    Kibitzer,
}
/// `onFlushed` callback: runs once per injection after a successful delivery.
pub type IdleInjectionCallback = Arc<dyn Fn() + Send + Sync>;
/// `onDeliveryFailed` callback: receives the delivery error message.
pub type IdleInjectionFailureCallback = Arc<dyn Fn(&str) + Send + Sync>;
#[derive(Clone)]
pub struct IdleInjection {
    pub key: String,
    pub source: IdleInjectionSource,
    pub custom_type: Option<String>,
    pub content: String,
    pub display: Option<bool>,
    pub details: Option<JsonValue>,
    /// LATEST addition (absent from the pinned TS): `passive?: boolean` rides a flush carrying at
    /// least one non-passive entry and never causes a flush by itself. `None` = non-passive.
    pub passive: Option<bool>,
    pub on_flushed: Option<IdleInjectionCallback>,
    pub on_delivery_failed: Option<IdleInjectionFailureCallback>,
}
/// The single idle-edge injection queue. The concrete implementation lives in the composition
/// (`crates/omo/maho-omo`, todo 47); ext-api defines only the contract so `ExtensionContext` can
/// carry it without depending on that crate. `None` means "no coordinator" and every consumer
/// falls back to a direct send, matching upstream's optional `ComponentContext.idleCoordinator`.
pub trait IdleInjectionCoordinator: Send + Sync {
    /// Accept an injection into the batch window. Returns `false` when the coordinator is retired:
    /// the queue is gone, the injection is NOT queued and gets no receipt, so the caller still owns
    /// the notification and must fail it durably (upstream `enqueue(...): boolean`).
    fn enqueue(&self, injection: IdleInjection) -> bool;
    /// Retire the coordinator on session shutdown (upstream `retire()`): refuse further enqueues and
    /// fail every still-queued injection through `on_delivery_failed`. A coordinator that is never
    /// retired keeps this default no-op and always accepts.
    fn retire(&self) {}
    fn schedule_flush(&self);
    fn flush_soon(&self);
    fn flush_on_idle(&self) -> usize;
    fn pending_count(&self) -> usize;
    fn remove(&self, key: &str) -> bool;
}
/// Structured component logger (upstream `ComponentLogger`, omo-senpi extension/types.ts:40).
pub trait ComponentLogger: Send + Sync {
    fn info(&self, message: &str, details: Option<&JsonValue>);
    fn warn(&self, message: &str, details: Option<&JsonValue>);
    fn error(&self, message: &str, details: Option<&JsonValue>);
}
/// Host-provided deferred scheduler (upstream `setTimeout(0)`): the host decides how the deferred
/// task is scheduled and ordered. A `tokio::spawn`/`yield_now` is not by itself asserted equivalent
/// to a JS macrotask; the concrete host (todo 47) must establish the ordering.
pub type DeferredMacrotask = Arc<dyn Fn(Box<dyn FnOnce() + Send + 'static>) + Send + Sync>;

/// Context actions are live host callbacks rather than frozen snapshots.
#[derive(Clone)]
pub struct ExtensionContext {
    pub ui: Arc<dyn ExtensionUi>,
    pub mode: ExtensionMode,
    pub has_ui: bool,
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub session_manager: Arc<dyn SessionManager>,
    pub model_registry: Arc<dyn ModelRegistry>,
    pub model: Option<Model>,
    pub thinking_level: Option<ThinkingLevel>,
    pub service_tier: Option<ServiceTier>,
    pub effective_service_tier: Option<ServiceTier>,
    pub scoped_models: Vec<ScopedModel>,
    pub goal_store_file: Option<PathBuf>,
    pub loaded_extension_paths: Vec<String>,
    pub signal: Option<AbortSignal>,
    pub steering_signal: Option<AbortSignal>,
    pub is_idle_fn: Arc<dyn Fn() -> bool + Send + Sync>,
    pub wait_for_idle_fn: Arc<dyn Fn() -> UiFuture<'static, ()> + Send + Sync>,
    pub is_project_trusted_fn: Arc<dyn Fn() -> bool + Send + Sync>,
    pub is_compacting_fn: Arc<dyn Fn() -> bool + Send + Sync>,
    pub get_system_prompt_fn: Arc<dyn Fn() -> String + Send + Sync>,
    pub get_system_prompt_options_fn: Arc<dyn Fn() -> BuildSystemPromptOptions + Send + Sync>,
    pub registered_mcp_servers: Vec<RegisteredMcpServerDeclaration>,
    pub update_tool_hook_status: Option<ToolHookStatusUpdater>,
    /// Idle-edge injection arbiter (todo 47). `None` = no coordinator; consumers send directly.
    pub idle_coordinator: Option<Arc<dyn IdleInjectionCoordinator>>,
    /// Structured component logger (todo 47). `None` = only `ctx.ui.notify`/`eprintln!` fallbacks.
    pub logger: Option<Arc<dyn ComponentLogger>>,
    /// Host deferred-macrotask scheduler (todo 47). `None` = no host macrotask entry point.
    pub defer_macrotask: Option<DeferredMacrotask>,
    /// Per-invocation compaction feedback signal, the port of upstream `createContext`'s
    /// `compactionSignal` closure variable: created fresh for every context so a signal begun in one
    /// context is never inherited by another. `ctx.session_manager` stays a shared adapter.
    pub compaction_signal: Arc<Mutex<Option<AbortSignal>>>,
}
impl ExtensionContext {
    fn assert_active_or_panic(&self) {
        if let Some(actions) = self.session_manager.extension_context_actions()
            && let Err(error) = actions.assert_active()
        {
            std::panic::panic_any(error);
        }
    }
    pub fn actions(&self) -> Result<&dyn ExtensionContextActions, ExtensionFailure> {
        let actions = self
            .session_manager
            .extension_context_actions()
            .ok_or_else(|| ExtensionFailure::new("Extension context actions are not bound"))?;
        actions.assert_active()?;
        Ok(actions)
    }
    pub fn engine_turns_held(&self) -> Result<bool, ExtensionFailure> {
        Ok(match self.session_manager.extension_context_actions() {
            Some(actions) => {
                actions.assert_active()?;
                actions.engine_turns_held()
            }
            None => false,
        })
    }
    pub fn get_pause_state(&self) -> Result<JsonValue, ExtensionFailure> {
        Ok(self.actions()?.get_pause_state())
    }
    pub fn fence_run(&self, run_ref: JsonValue) -> Result<(), ExtensionFailure> {
        self.actions()?.fence_run(run_ref)
    }
    pub fn release_run_fence(&self, run_ref: JsonValue) -> Result<(), ExtensionFailure> {
        self.actions()?.release_run_fence(run_ref)
    }
    pub fn note_held_wake(&self, source: &str) -> Result<(), ExtensionFailure> {
        if let Some(actions) = self.session_manager.extension_context_actions() {
            actions.assert_active()?;
            actions.note_held_wake(source);
        }
        Ok(())
    }
    /// Runs the child module registered under `id`.
    ///
    /// # Errors
    /// Fails with `offload_unavailable` when no session backs this context.
    pub fn offload<'a>(
        &'a self,
        id: &'a str,
        args: JsonValue,
        options: OffloadOptions,
    ) -> ExtensionFuture<'a, JsonValue> {
        Box::pin(async move {
            let actions = self.session_manager.extension_context_actions();
            if let Some(actions) = actions {
                actions.assert_active()?;
            }
            match actions.and_then(|actions| actions.offload(id, args, options)) {
                Some(future) => future.await,
                None => Err(ExtensionFailure::new(format!(
                    "offload_unavailable: no session backs this context (offload {id})"
                ))),
            }
        })
    }
    pub fn current_model(&self) -> Result<Option<Model>, ExtensionFailure> {
        Ok(match self.session_manager.extension_context_actions() {
            Some(actions) => {
                actions.assert_active()?;
                actions.get_model()
            }
            None => self.model.clone(),
        })
    }
    pub fn current_service_tier(&self) -> Result<Option<ServiceTier>, ExtensionFailure> {
        Ok(match self.session_manager.extension_context_actions() {
            Some(actions) => {
                actions.assert_active()?;
                actions.get_service_tier()
            }
            None => self.service_tier,
        })
    }
    pub fn current_effective_service_tier(&self) -> Result<Option<ServiceTier>, ExtensionFailure> {
        Ok(match self.session_manager.extension_context_actions() {
            Some(actions) => {
                actions.assert_active()?;
                actions.get_effective_service_tier()
            }
            None => self.effective_service_tier.or(self.service_tier),
        })
    }
    pub fn current_steering_signal(&self) -> Result<Option<AbortSignal>, ExtensionFailure> {
        Ok(match self.session_manager.extension_context_actions() {
            Some(actions) => {
                actions.assert_active()?;
                actions.get_steering_signal()
            }
            None => self.steering_signal.clone(),
        })
    }
    pub fn current_thinking_level(&self) -> Result<Option<ThinkingLevel>, ExtensionFailure> {
        Ok(match self.session_manager.extension_context_actions() {
            Some(actions) => {
                actions.assert_active()?;
                actions.get_thinking_level()
            }
            None => self.thinking_level,
        })
    }
    pub fn current_model_registry(&self) -> Result<Arc<dyn ModelRegistry>, ExtensionFailure> {
        if let Some(actions) = self.session_manager.extension_context_actions() {
            actions.assert_active()?;
        }
        Ok(Arc::clone(&self.model_registry))
    }
    pub fn current_ui(&self) -> Result<Arc<dyn ExtensionUi>, ExtensionFailure> {
        if let Some(actions) = self.session_manager.extension_context_actions() {
            actions.assert_active()?;
        }
        Ok(Arc::clone(&self.ui))
    }
    pub fn apply_fast_mode(&self, enabled: bool) -> ExtensionFuture<'static, FastModeResult> {
        match self.actions() {
            Ok(actions) => actions.apply_fast_mode(enabled, self.clone()),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
    pub fn abort(&self, source: Option<AbortSource>) -> Result<(), ExtensionFailure> {
        self.actions()?.abort(source);
        Ok(())
    }
    pub fn has_queued_person_input(&self) -> Result<bool, ExtensionFailure> {
        Ok(self.actions()?.has_queued_person_input())
    }
    pub fn todo_turn_end_backstop_enabled(&self) -> Result<bool, ExtensionFailure> {
        Ok(self.actions()?.todo_turn_end_backstop_enabled())
    }
    pub fn has_pending_messages(&self) -> Result<bool, ExtensionFailure> {
        Ok(self.actions()?.has_pending_messages())
    }
    pub async fn request_reload(&self) -> Result<(), ExtensionFailure> {
        self.actions()?.request_reload().await
    }
    pub async fn check_reload_veto(&self) -> Result<ReloadVetoDecision, ExtensionFailure> {
        let result = self.actions()?.check_reload_veto().await?;
        self.actions()?;
        Ok(result)
    }
    pub fn shutdown(&self) -> Result<(), ExtensionFailure> {
        self.actions()?.shutdown();
        Ok(())
    }
    pub fn get_context_usage(&self) -> Result<Option<ContextUsage>, ExtensionFailure> {
        Ok(self.actions()?.get_context_usage())
    }
    pub fn get_compaction_settings(&self) -> Result<CompactionSettings, ExtensionFailure> {
        Ok(self.actions()?.get_compaction_settings())
    }
    pub fn get_compaction_model(&self) -> Result<Option<Model>, ExtensionFailure> {
        Ok(self.actions()?.get_compaction_model())
    }
    pub fn get_resolved_compaction_settings(
        &self,
    ) -> Result<Option<ResolvedCompactionSettings>, ExtensionFailure> {
        Ok(self.actions()?.get_resolved_compaction_settings())
    }
    pub fn get_compaction_preparation(
        &self,
    ) -> Result<Option<CompactionPreparationDetails>, ExtensionFailure> {
        Ok(self.actions()?.get_compaction_preparation())
    }
    pub fn get_prompt_cache_safe_wait_seconds(&self) -> Result<Option<f64>, ExtensionFailure> {
        Ok(self.actions()?.get_prompt_cache_safe_wait_seconds())
    }
    pub fn get_prompt_cache_goal_backstop_max_seconds(&self) -> Result<f64, ExtensionFailure> {
        Ok(self.actions()?.get_prompt_cache_goal_backstop_max_seconds())
    }
    pub fn get_prompt_cache_keep_alive_settings(
        &self,
    ) -> Result<PromptCacheKeepAliveSettings, ExtensionFailure> {
        Ok(self.actions()?.get_prompt_cache_keep_alive_settings())
    }
    pub fn get_look_at_settings(&self) -> Result<LookAtSettings, ExtensionFailure> {
        Ok(self.actions()?.get_look_at_settings())
    }
    pub fn get_ask_user_settings(&self) -> Result<AskUserSettings, ExtensionFailure> {
        Ok(self.actions()?.get_ask_user_settings())
    }
    pub fn get_image_settings(&self) -> Result<ImageSettings, ExtensionFailure> {
        Ok(self.actions()?.get_image_settings())
    }
    pub fn session_settings(&self) -> Result<&dyn ExtensionSessionSettings, ExtensionFailure> {
        Ok(self.actions()?.session_settings())
    }
    pub fn compact(&self, options: CompactOptions) -> Result<(), ExtensionFailure> {
        self.actions()?.compact(options);
        Ok(())
    }
    pub async fn prepare_provider_request(
        &self,
        messages: Vec<AgentMessage>,
    ) -> Result<ProviderRequestPreparation, ExtensionFailure> {
        let result = self.actions()?.prepare_provider_request(messages).await?;
        self.actions()?;
        Ok(result)
    }
    pub fn begin_compaction(
        &self,
        options: BeginCompactionOptions,
    ) -> Result<Option<AbortSignal>, ExtensionFailure> {
        let signal = self.actions()?.begin_compaction(options);
        *self
            .compaction_signal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = signal.clone();
        Ok(signal)
    }
    pub fn update_compaction(
        &self,
        mut options: UpdateCompactionOptions,
    ) -> Result<(), ExtensionFailure> {
        options.signal = options.signal.or_else(|| {
            self.compaction_signal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        });
        self.actions()?.update_compaction(options);
        Ok(())
    }
    pub fn end_compaction(
        &self,
        mut options: EndCompactionOptions,
    ) -> Result<(), ExtensionFailure> {
        options.signal = options.signal.or_else(|| {
            self.compaction_signal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        });
        self.actions()?.end_compaction(options);
        Ok(())
    }
    pub fn get_message_revision(&self) -> Result<u64, ExtensionFailure> {
        Ok(self.actions()?.get_message_revision())
    }
    pub async fn apply_compaction(
        &self,
        result: CompactionResult,
        mut options: ApplyCompactionOptions,
    ) -> Result<ApplyCompactionResult, ExtensionFailure> {
        options.signal = options.signal.or_else(|| {
            self.compaction_signal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        });
        let result = self.actions()?.apply_compaction(result, options).await?;
        self.actions()?;
        Ok(result)
    }
    pub fn get_loaded_hook_sources(&self) -> Result<LoadedHookSources, ExtensionFailure> {
        Ok(self.actions()?.get_loaded_hook_sources())
    }
    pub fn kernel_tools(&self) -> Result<Option<&dyn ExtensionKernelTools>, ExtensionFailure> {
        Ok(self.actions()?.kernel_tools())
    }
    pub fn set_approved_monitor_parent(
        &self,
        tool_call_id: &str,
        input: &JsonValue,
        parent: &Path,
    ) -> Result<(), ExtensionFailure> {
        self.actions()?
            .set_approved_monitor_parent(tool_call_id, input, parent)
    }
    pub fn take_approved_monitor_parent(
        &self,
        tool_call_id: &str,
        input: &JsonValue,
    ) -> Result<Option<PathBuf>, ExtensionFailure> {
        self.actions()?
            .take_approved_monitor_parent(tool_call_id, input)
    }
    pub fn is_idle(&self) -> bool {
        self.assert_active_or_panic();
        self.session_manager
            .extension_context_actions()
            .map_or_else(|| (self.is_idle_fn)(), ExtensionContextActions::is_idle)
    }
    pub async fn wait_for_idle(&self) {
        self.assert_active_or_panic();
        (self.wait_for_idle_fn)().await;
        self.assert_active_or_panic();
    }
    pub fn is_project_trusted(&self) -> bool {
        self.assert_active_or_panic();
        self.session_manager
            .extension_context_actions()
            .map_or_else(
                || (self.is_project_trusted_fn)(),
                ExtensionContextActions::is_project_trusted,
            )
    }
    pub fn is_compacting(&self) -> bool {
        self.assert_active_or_panic();
        self.session_manager
            .extension_context_actions()
            .map_or_else(
                || (self.is_compacting_fn)(),
                ExtensionContextActions::is_compacting,
            )
    }
    pub fn get_system_prompt(&self) -> String {
        self.assert_active_or_panic();
        (self.get_system_prompt_fn)()
    }
    pub fn get_system_prompt_options(&self) -> BuildSystemPromptOptions {
        self.assert_active_or_panic();
        (self.get_system_prompt_options_fn)()
    }
    pub fn get_registered_mcp_servers(&self) -> Vec<RegisteredMcpServerDeclaration> {
        self.assert_active_or_panic();
        self.session_manager
            .extension_context_actions()
            .and_then(ExtensionContextActions::get_registered_mcp_servers)
            .unwrap_or_else(|| self.registered_mcp_servers.clone())
    }
}
impl ToolContext for ExtensionContext {
    fn cwd(&self) -> &Path {
        self.assert_active_or_panic();
        &self.cwd
    }
    fn model(&self) -> Option<&Model> {
        self.assert_active_or_panic();
        self.model.as_ref()
    }
    fn thinking_level(&self) -> Option<ThinkingLevel> {
        self.assert_active_or_panic();
        self.thinking_level
    }
    fn session_manager(&self) -> &dyn ToolSessionManager {
        self.assert_active_or_panic();
        self.session_manager.as_ref()
    }
    fn goal_store_file(&self) -> Option<&Path> {
        self.assert_active_or_panic();
        self.goal_store_file.as_deref()
    }
    fn browser_engine(&self) -> Option<&str> {
        self.session_manager
            .extension_context_actions()
            .and_then(ExtensionContextActions::get_browser_engine)
    }
    fn get_steering_signal(&self) -> Option<AbortSignal> {
        self.current_steering_signal().ok().flatten()
    }
    fn get_prompt_cache_safe_wait_seconds(&self) -> Option<f64> {
        ExtensionContext::get_prompt_cache_safe_wait_seconds(self)
            .ok()
            .flatten()
    }
    fn take_approved_monitor_parent(
        &self,
        tool_call_id: &str,
        input: &JsonValue,
    ) -> Result<Option<PathBuf>, maho_tools::definition::ToolError> {
        ExtensionContext::take_approved_monitor_parent(self, tool_call_id, input)
            .map_err(|error| maho_tools::definition::ToolError::Message(error.message))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BuildSystemPromptOptions {
    pub cwd: PathBuf,
    pub custom_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub tools: Vec<String>,
    pub tool_snippets: BTreeMap<String, String>,
    pub prompt_guidelines: Vec<String>,
    pub skills: Vec<Skill>,
    pub context_files: Vec<ContextFile>,
    pub surface: PromptSurface,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextFile {
    pub path: String,
    pub content: String,
}
/// senpi `PromptSurface`: where the assistant's replies are rendered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PromptSurface {
    #[default]
    Terminal,
    App,
    Chat,
}
#[derive(Clone, Debug, PartialEq)]
pub struct CustomMessage {
    pub custom_type: String,
    pub content: Vec<ToolContent>,
    pub display: bool,
    pub details: Option<JsonValue>,
}
#[derive(Clone, Debug)]
pub struct BeforeAgentStartEvent {
    pub prompt: String,
    /// Turn source: `prompt`, `delivery`, or `extension`, matching senpi.
    pub trigger: String,
    /// True only for prompt composition previews, which must not consume state.
    pub preview: bool,
    pub images: Option<Vec<ImageContent>>,
    pub system_prompt: String,
    pub system_prompt_options: BuildSystemPromptOptions,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DynamicSystemPromptPart {
    pub name: String,
    pub source: Option<String>,
    pub text: String,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BeforeAgentStartEventResult {
    pub system_prompt_parts: Option<Vec<DynamicSystemPromptPart>>,
    pub message: Option<CustomMessage>,
    pub system_prompt: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BeforeAgentStartCombinedResult {
    pub system_prompt_parts: Option<Vec<DynamicSystemPromptPart>>,
    pub messages: Vec<CustomMessage>,
    pub system_prompt: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputSource {
    Interactive,
    Rpc,
    Extension,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamingBehavior {
    Steer,
    FollowUp,
}
#[derive(Clone, Debug)]
pub struct InputEvent {
    pub input_id: String,
    pub text: String,
    pub images: Option<Vec<ImageContent>>,
    pub source: InputSource,
    pub streaming_behavior: Option<StreamingBehavior>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExtensionHandlerOptions {
    pub preview_safe: bool,
    pub mutates_messages: Option<bool>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum InputEventResult {
    Continue,
    Transform {
        text: String,
        images: Option<Vec<ImageContent>>,
    },
    Handled,
}
#[derive(Clone, Debug)]
pub struct ToolCallEvent {
    pub tool_call_id: String,
    pub tool_name: String,
    pub input: JsonValue,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolCallEventResult {
    pub block: Option<bool>,
    pub reason: Option<String>,
    pub terminate: Option<bool>,
}
#[derive(Clone, Debug)]
pub struct ToolResultEvent {
    pub tool_call_id: String,
    pub tool_name: String,
    pub input: JsonValue,
    pub content: Vec<ToolContent>,
    pub details: Option<JsonValue>,
    pub is_error: bool,
    pub usage: Option<Usage>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolResultEventResult {
    pub content: Option<Vec<ToolContent>>,
    pub details: Option<JsonValue>,
    pub is_error: Option<bool>,
    pub usage: Option<Usage>,
}
#[derive(Clone, Debug)]
pub struct ModelSelectEvent {
    pub model: Model,
    pub previous_model: Option<Model>,
    pub source: ModelSelectSource,
    pub system_prompt: String,
    pub system_prompt_options: BuildSystemPromptOptions,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelSelectSource {
    Set,
    Cycle,
    Restore,
    Fallback,
    FallbackRevert,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelSelectEventResult {
    pub system_prompt: Option<Option<String>>,
    pub system_prompt_name: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustDecision {
    Yes,
    No,
    Undecided,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectTrustEventResult {
    pub trusted: TrustDecision,
    pub remember: Option<bool>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionBeforeEventResult {
    pub cancel: Option<bool>,
    pub reason: Option<String>,
    pub skip_conversation_restore: Option<bool>,
    pub compaction: Option<CompactionResult>,
    pub rejection_cause: Option<CompactionRejectionCause>,
    pub summary: Option<JsonValue>,
    pub custom_instructions: Option<String>,
    pub replace_instructions: Option<bool>,
    pub label: Option<String>,
}
pub type SessionBeforeSwitchResult = SessionBeforeEventResult;
pub type SessionBeforeForkResult = SessionBeforeEventResult;
pub type SessionBeforeReloadResult = SessionBeforeEventResult;
pub type SessionBeforeCompactResult = SessionBeforeEventResult;
pub type SessionBeforeTreeResult = SessionBeforeEventResult;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionReason {
    Manual,
    Threshold,
    Overflow,
    PrePrompt,
    Branch,
    Extension,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionRejectionCause {
    CancelledByExtension,
    ExternalOwner,
    WouldOverflow,
    CircuitBreaker,
    PerTurnCap,
    StaleRevision,
}
#[derive(Clone, Debug, PartialEq)]
pub struct CompactionResult {
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: u64,
    pub details: Option<JsonValue>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct CompactionPreparation {
    pub settings: CompactionSettings,
    pub messages_to_summarize: Vec<AgentMessage>,
    pub turn_prefix_messages: Vec<AgentMessage>,
    pub tokens_before: u64,
    pub first_kept_entry_id: String,
    pub previous_summary: Option<String>,
}
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
pub struct CompactionFileOperations {
    pub read: Vec<String>,
    pub written: Vec<String>,
    pub edited: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: u64,
    pub keep_recent_tokens: u64,
}
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
pub enum SessionReason {
    Startup,
    Reload,
    New,
    Resume,
    Fork,
    Quit,
}
#[derive(Clone, Debug)]
pub struct SessionStartEvent {
    pub reason: SessionReason,
    pub initial_model_provenance: Option<String>,
    pub previous_session_file: Option<String>,
    /// Whether a human is attending this session (`None` = unknown).
    pub attended: Option<bool>,
}
#[derive(Clone, Debug)]
pub struct SessionShutdownEvent {
    pub reason: SessionReason,
    pub target_session_file: Option<String>,
    pub signal: Option<AbortSignal>,
}
#[derive(Clone, Debug)]
pub struct ResourcesDiscoverEvent {
    pub cwd: PathBuf,
    pub reason: SessionReason,
    pub scoped_entries: bool,
}
#[derive(Clone, Debug)]
pub struct SessionBeforeCompactEvent {
    pub reason: CompactionReason,
    pub will_retry: bool,
    pub request_id: String,
    pub preparation: CompactionPreparation,
    pub branch_entries: Vec<SessionEntry>,
    pub custom_instructions: Option<String>,
    pub signal: AbortSignal,
}
#[derive(Clone, Debug)]
pub enum SessionCompactEvent {
    Accepted {
        reason: CompactionReason,
        request_id: String,
        compaction_entry: SessionEntry,
        from_extension: bool,
        will_retry: bool,
    },
    Rejected {
        reason: CompactionReason,
        request_id: String,
        rejection_cause: CompactionRejectionCause,
    },
}

/// Every event accepted by types.ts has a native discriminant. JSON is retained only for
/// payloads whose upstream contracts are open records or owned by a subsequent session port.
/// senpi `ContextEvent.source`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContextEventSource {
    #[default]
    Agent,
    Projection,
}
#[derive(Clone, Debug)]
pub enum ExtensionEvent {
    ProjectTrust {
        cwd: PathBuf,
    },
    ResourcesDiscover(ResourcesDiscoverEvent),
    SessionStart(SessionStartEvent),
    SessionInfoChanged {
        name: Option<String>,
    },
    SessionParked,
    SessionResumed,
    SessionBeforeSwitch {
        reason: SessionReason,
        target_session_file: Option<String>,
    },
    SessionBeforeFork {
        entry_id: String,
        position: ForkPosition,
    },
    SessionBeforeReload,
    SessionBeforeCompact(SessionBeforeCompactEvent),
    SessionCompact(SessionCompactEvent),
    SessionCompactFailed {
        reason: CompactionReason,
        error_message: Option<String>,
        aborted: bool,
        will_retry: bool,
        from_extension: bool,
    },
    SessionShutdown(SessionShutdownEvent),
    SessionAbort,
    SessionMoveExport {
        move_id: String,
        dir: PathBuf,
        signal: AbortSignal,
    },
    SessionMoveImport {
        move_id: String,
        dir: PathBuf,
        redelivered: bool,
        signal: AbortSignal,
    },
    SessionHoldChanged {
        change: String,
        pause_seq: u64,
        holds: Vec<JsonValue>,
    },
    SessionExtensionsRemoved {
        reason: SessionReason,
        removed: Vec<ExtensionIdentity>,
    },
    SessionBeforeTree {
        preparation: TreePreparation,
        signal: AbortSignal,
    },
    SessionTree {
        new_leaf_id: Option<String>,
        old_leaf_id: Option<String>,
        summary_entry: Option<SessionEntry>,
        from_extension: Option<bool>,
    },
    /// `entry_ids` is senpi's request-local `SESSION_CONTEXT_ENTRY_ID` stamp as a typed side
    /// channel: index-aligned with `messages`, `Some(id)` for a message projected from that session
    /// entry. It is never written into message JSON.
    Context {
        messages: Vec<AgentMessage>,
        entry_ids: Vec<Option<String>>,
        /// senpi `ContextEvent.source`: a prepared projection may not advance live agent state.
        source: ContextEventSource,
    },
    BeforeProviderRequest {
        payload: JsonValue,
        model: Option<Model>,
        headers: Option<BTreeMap<String, Option<String>>>,
    },
    BeforeProviderHeaders {
        headers: BTreeMap<String, Option<String>>,
    },
    AfterProviderResponse {
        status: u16,
        headers: BTreeMap<String, String>,
    },
    BeforeAgentStart(BeforeAgentStartEvent),
    AgentStart,
    AgentEnd {
        messages: Vec<AgentMessage>,
        aborted: Option<bool>,
        will_retry: Option<bool>,
        abort_source: Option<AbortSource>,
    },
    AgentSettled,
    UiPromptStart {
        kind: UiPromptKind,
        title: Option<String>,
    },
    UiPromptEnd {
        kind: UiPromptKind,
        title: Option<String>,
    },
    TurnStart {
        turn_index: u64,
        timestamp: u64,
    },
    TurnEnd {
        turn_index: u64,
        message: AgentMessage,
        tool_results: Vec<maho_ai::types::ToolResultMessage>,
    },
    MessageStart {
        message: AgentMessage,
    },
    MessageUpdate {
        message: AgentMessage,
        assistant_message_event: JsonValue,
    },
    MessageEnd {
        message: AgentMessage,
    },
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: JsonValue,
        parent_tool_call_id: Option<String>,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: JsonValue,
        partial_result: JsonValue,
        parent_tool_call_id: Option<String>,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: JsonValue,
        is_error: bool,
        parent_tool_call_id: Option<String>,
    },
    ModelSelect(ModelSelectEvent),
    SystemPromptChange {
        system_prompt: String,
        previous_system_prompt: String,
        system_prompt_name: Option<String>,
        model: Model,
        previous_model: Option<Model>,
    },
    ThinkingLevelSelect {
        level: ModelThinkingLevel,
        previous_level: ModelThinkingLevel,
    },
    ToolCall(ToolCallEvent),
    ToolResult(ToolResultEvent),
    UserBash {
        command: String,
        exclude_from_context: bool,
        cwd: PathBuf,
    },
    Input(InputEvent),
    InputDisposition {
        input_id: String,
        disposition: InputDisposition,
    },
    ProviderStreamEvent {
        provider: String,
        api: String,
        model: String,
        data: JsonValue,
    },
    AgentBeforeSettle {
        entries: Vec<JsonValue>,
        should_continue: bool,
        context: JsonValue,
    },
    SessionControlWake {
        source: String,
        reason: String,
    },
    ToolActivated {
        tool_names: Vec<String>,
    },
    /// Sources released by real input or an explicit Resume; handlers revalidate current state.
    HeldWakesReleased {
        sources: BTreeMap<String, u32>,
    },
}
impl ExtensionEvent {
    pub const fn kind(&self) -> EventKind {
        match self {
            Self::ProjectTrust { .. } => EventKind::ProjectTrust,
            Self::ResourcesDiscover(_) => EventKind::ResourcesDiscover,
            Self::SessionStart(_) => EventKind::SessionStart,
            Self::SessionInfoChanged { .. } => EventKind::SessionInfoChanged,
            Self::SessionParked => EventKind::SessionParked,
            Self::SessionResumed => EventKind::SessionResumed,
            Self::SessionBeforeSwitch { .. } => EventKind::SessionBeforeSwitch,
            Self::SessionBeforeFork { .. } => EventKind::SessionBeforeFork,
            Self::SessionBeforeReload => EventKind::SessionBeforeReload,
            Self::SessionBeforeCompact(_) => EventKind::SessionBeforeCompact,
            Self::SessionCompact(_) => EventKind::SessionCompact,
            Self::SessionCompactFailed { .. } => EventKind::SessionCompactFailed,
            Self::SessionShutdown(_) => EventKind::SessionShutdown,
            Self::SessionAbort => EventKind::SessionAbort,
            Self::SessionMoveExport { .. } => EventKind::SessionMoveExport,
            Self::SessionMoveImport { .. } => EventKind::SessionMoveImport,
            Self::SessionHoldChanged { .. } => EventKind::SessionHoldChanged,
            Self::SessionExtensionsRemoved { .. } => EventKind::SessionExtensionsRemoved,
            Self::SessionBeforeTree { .. } => EventKind::SessionBeforeTree,
            Self::SessionTree { .. } => EventKind::SessionTree,
            Self::Context { .. } => EventKind::Context,
            Self::BeforeProviderRequest { .. } => EventKind::BeforeProviderRequest,
            Self::BeforeProviderHeaders { .. } => EventKind::BeforeProviderHeaders,
            Self::AfterProviderResponse { .. } => EventKind::AfterProviderResponse,
            Self::BeforeAgentStart(_) => EventKind::BeforeAgentStart,
            Self::AgentStart => EventKind::AgentStart,
            Self::AgentEnd { .. } => EventKind::AgentEnd,
            Self::AgentSettled => EventKind::AgentSettled,
            Self::UiPromptStart { .. } => EventKind::UiPromptStart,
            Self::UiPromptEnd { .. } => EventKind::UiPromptEnd,
            Self::TurnStart { .. } => EventKind::TurnStart,
            Self::TurnEnd { .. } => EventKind::TurnEnd,
            Self::MessageStart { .. } => EventKind::MessageStart,
            Self::MessageUpdate { .. } => EventKind::MessageUpdate,
            Self::MessageEnd { .. } => EventKind::MessageEnd,
            Self::ToolExecutionStart { .. } => EventKind::ToolExecutionStart,
            Self::ToolExecutionUpdate { .. } => EventKind::ToolExecutionUpdate,
            Self::ToolExecutionEnd { .. } => EventKind::ToolExecutionEnd,
            Self::ModelSelect(_) => EventKind::ModelSelect,
            Self::SystemPromptChange { .. } => EventKind::SystemPromptChange,
            Self::ThinkingLevelSelect { .. } => EventKind::ThinkingLevelSelect,
            Self::ToolCall(_) => EventKind::ToolCall,
            Self::ToolResult(_) => EventKind::ToolResult,
            Self::UserBash { .. } => EventKind::UserBash,
            Self::Input(_) => EventKind::Input,
            Self::InputDisposition { .. } => EventKind::InputDisposition,
            Self::ProviderStreamEvent { .. } => EventKind::ProviderStreamEvent,
            Self::AgentBeforeSettle { .. } => EventKind::AgentBeforeSettle,
            Self::SessionControlWake { .. } => EventKind::SessionControlWake,
            Self::ToolActivated { .. } => EventKind::ToolActivated,
            Self::HeldWakesReleased { .. } => EventKind::HeldWakesReleased,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkPosition {
    Before,
    At,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AbortSource {
    User,
    System,
    Provider,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiPromptKind {
    Select,
    Confirm,
    Input,
    Editor,
    Custom,
    Question,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiPromptReason {
    UiPrompt,
}
impl UiPromptReason {
    pub const fn as_str(self) -> &'static str {
        "ui_prompt"
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemPromptChangeSource {
    ModelSelect,
}
impl SystemPromptChangeSource {
    pub const fn as_str(self) -> &'static str {
        "model_select"
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiPromptStartEvent {
    pub reason: UiPromptReason,
    pub kind: UiPromptKind,
    pub title: Option<String>,
}
pub type UiPromptEndEvent = UiPromptStartEvent;
#[derive(Clone, Debug, PartialEq)]
pub struct SystemPromptChangeEvent {
    pub system_prompt: String,
    pub previous_system_prompt: String,
    pub system_prompt_name: Option<String>,
    pub model: Model,
    pub previous_model: Option<Model>,
    pub source: SystemPromptChangeSource,
}
impl ExtensionEvent {
    pub fn ui_prompt_start_event(&self) -> Option<UiPromptStartEvent> {
        match self {
            Self::UiPromptStart { kind, title } => Some(UiPromptStartEvent {
                reason: UiPromptReason::UiPrompt,
                kind: *kind,
                title: title.clone(),
            }),
            _ => None,
        }
    }
    pub fn ui_prompt_end_event(&self) -> Option<UiPromptEndEvent> {
        match self {
            Self::UiPromptEnd { kind, title } => Some(UiPromptEndEvent {
                reason: UiPromptReason::UiPrompt,
                kind: *kind,
                title: title.clone(),
            }),
            _ => None,
        }
    }
    pub fn system_prompt_change_event(&self) -> Option<SystemPromptChangeEvent> {
        match self {
            Self::SystemPromptChange {
                system_prompt,
                previous_system_prompt,
                system_prompt_name,
                model,
                previous_model,
            } => Some(SystemPromptChangeEvent {
                system_prompt: system_prompt.clone(),
                previous_system_prompt: previous_system_prompt.clone(),
                system_prompt_name: system_prompt_name.clone(),
                model: model.clone(),
                previous_model: previous_model.clone(),
                source: SystemPromptChangeSource::ModelSelect,
            }),
            _ => None,
        }
    }
    pub const fn ui_prompt_reason(&self) -> Option<UiPromptReason> {
        match self {
            Self::UiPromptStart { .. } | Self::UiPromptEnd { .. } => Some(UiPromptReason::UiPrompt),
            _ => None,
        }
    }
    pub const fn system_prompt_change_source(&self) -> Option<SystemPromptChangeSource> {
        match self {
            Self::SystemPromptChange { .. } => Some(SystemPromptChangeSource::ModelSelect),
            _ => None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputDisposition {
    Handled,
    Queued,
    Started,
    Rejected,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionIdentity {
    pub path: String,
    pub resolved_path: String,
}
#[derive(Clone, Debug)]
pub struct TreePreparation {
    pub target_id: String,
    pub old_leaf_id: Option<String>,
    pub common_ancestor_id: Option<String>,
    pub entries_to_summarize: Vec<SessionEntry>,
    pub user_wants_summary: bool,
    pub custom_instructions: Option<String>,
    pub replace_instructions: Option<bool>,
    pub label: Option<String>,
}
pub enum EventResult {
    None,
    ProjectTrust(ProjectTrustEventResult),
    ResourcesDiscover(ResourcesDiscoverResult),
    SessionBefore(SessionBeforeEventResult),
    Context {
        messages: Option<Vec<AgentMessage>>,
    },
    ProviderPayload(JsonValue),
    BeforeAgentStart(BeforeAgentStartEventResult),
    ModelSelect(ModelSelectEventResult),
    MessageEnd {
        message: Option<AgentMessage>,
    },
    ToolCall(ToolCallEventResult),
    ToolResult(ToolResultEventResult),
    UserBash {
        operations: Option<Arc<dyn maho_tools::bash::BashOperations>>,
        result: Option<maho_tools::bash_executor::BashResult>,
    },
    Input(InputEventResult),
}
pub type ExtensionHandler = Arc<
    dyn for<'a> Fn(&'a mut ExtensionEvent, &'a ExtensionContext) -> ExtensionFuture<'a, EventResult>
        + Send
        + Sync,
>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpServerDeclaration {
    pub transport: Option<McpTransport>,
    pub url: Option<String>,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: Option<BTreeMap<String, String>>,
    pub cwd: Option<String>,
    pub headers: Option<BTreeMap<String, String>>,
    pub auth: Option<McpAuth>,
    pub bearer_token_env: Option<String>,
    pub oauth: Option<McpOAuth>,
    pub enabled: Option<bool>,
    pub lifecycle: Option<McpLifecycle>,
    pub idle_timeout_min: Option<f64>,
    pub request_timeout_ms: Option<f64>,
    pub connect_timeout_ms: Option<f64>,
    pub startup_timeout_ms: Option<f64>,
    pub include_tools: Option<Vec<String>>,
    pub exclude_tools: Option<Vec<String>>,
    pub direct_tools: Option<McpDirectTools>,
    pub exposure: Option<McpExposure>,
    pub log_level: Option<McpLogLevel>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpTransport {
    Stdio,
    Http,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpAuth {
    Bearer,
    OAuth,
    Disabled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpLifecycle {
    Lazy,
    Eager,
    KeepAlive,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpExposure {
    Auto,
    Direct,
    Search,
    Proxy,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpLogLevel {
    Debug,
    Info,
    Notice,
    Warning,
    Error,
    Critical,
    Alert,
    Emergency,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpDirectTools {
    Enabled(bool),
    Names(Vec<String>),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthFlow {
    Code,
    ClientCredentials,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpOAuth {
    pub client_id: Option<String>,
    pub callback_port: Option<u16>,
    pub scopes: Option<Vec<String>>,
    pub client_metadata_url: Option<String>,
    pub flow: Option<OAuthFlow>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct RegisteredMcpServerDeclaration {
    pub name: String,
    pub config: McpServerDeclaration,
    pub extension_path: String,
    pub registration_cwd: PathBuf,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlagValue {
    Boolean(bool),
    String(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlagType {
    Boolean { default: Option<bool> },
    String { default: Option<String> },
}
#[derive(Clone, Debug)]
pub struct ExtensionFlag {
    pub name: String,
    pub description: Option<String>,
    pub kind: FlagType,
    pub extension_path: String,
}
pub type CommandHandler =
    Arc<dyn for<'a> Fn(&'a str, &'a ExtensionContext) -> ExtensionFuture<'a, ()> + Send + Sync>;
pub type CommandArgumentCompletions = Arc<
    dyn for<'a> Fn(
            &'a str,
        )
            -> ExtensionFuture<'a, Option<Vec<maho_tui::autocomplete::AutocompleteItem>>>
        + Send
        + Sync,
>;
pub type CommandContextHandler = Arc<
    dyn for<'a> Fn(&'a str, &'a ExtensionCommandContext) -> ExtensionFuture<'a, ()> + Send + Sync,
>;
#[derive(Clone, Debug, Default)]
pub struct ExtensionTreeNavigationOptions {
    pub summarize: Option<bool>,
    pub custom_instructions: Option<String>,
    pub replace_instructions: Option<bool>,
    pub label: Option<String>,
    pub expected_leaf_id: Option<String>,
}
#[derive(Clone, Debug, Default)]
pub struct EditMessageOptions {
    pub summarize: Option<bool>,
    pub custom_instructions: Option<String>,
    pub expected_leaf_id: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionNavigationResult {
    pub cancelled: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditMessageResult {
    pub cancelled: bool,
    pub unchanged: Option<bool>,
    pub entry_id: Option<String>,
}
pub type WithSession =
    Arc<dyn for<'a> Fn(&'a ReplacedSessionContext) -> ExtensionFuture<'a, ()> + Send + Sync>;
pub type SessionSetup =
    Arc<dyn for<'a> Fn(&'a dyn SessionManager) -> ExtensionFuture<'a, ()> + Send + Sync>;
#[derive(Clone, Default)]
pub struct NewSessionOptions {
    pub parent_session: Option<String>,
    pub setup: Option<SessionSetup>,
    pub with_session: Option<WithSession>,
}
#[derive(Clone, Default)]
pub struct ForkOptions {
    pub position: Option<ForkPosition>,
    pub with_session: Option<WithSession>,
}
#[derive(Clone, Default)]
pub struct SwitchSessionOptions {
    pub with_session: Option<WithSession>,
}
pub trait ExtensionCommandContextActions: Send + Sync {
    fn wait_for_idle(&self) -> ExtensionFuture<'_, ()>;
    fn new_session(
        &self,
        options: NewSessionOptions,
    ) -> ExtensionFuture<'_, SessionNavigationResult>;
    fn fork<'a>(
        &'a self,
        entry_id: &'a str,
        options: ForkOptions,
    ) -> ExtensionFuture<'a, SessionNavigationResult>;
    fn navigate_tree<'a>(
        &'a self,
        target_id: &'a str,
        options: ExtensionTreeNavigationOptions,
    ) -> ExtensionFuture<'a, SessionNavigationResult>;
    fn edit_assistant_message<'a>(
        &'a self,
        entry_id: &'a str,
        text: &'a str,
        options: EditMessageOptions,
    ) -> ExtensionFuture<'a, EditMessageResult>;
    fn edit_user_message<'a>(
        &'a self,
        entry_id: &'a str,
        text: &'a str,
        options: EditMessageOptions,
    ) -> ExtensionFuture<'a, EditMessageResult>;
    fn switch_session<'a>(
        &'a self,
        path: &'a str,
        options: SwitchSessionOptions,
    ) -> ExtensionFuture<'a, SessionNavigationResult>;
    fn reload(&self) -> ExtensionFuture<'_, ()>;
}
#[derive(Clone)]
pub struct ExtensionCommandContext {
    pub context: ExtensionContext,
    pub actions: Arc<dyn ExtensionCommandContextActions>,
    pub runtime: ExtensionRuntime,
}
impl std::ops::Deref for ExtensionCommandContext {
    type Target = ExtensionContext;
    fn deref(&self) -> &Self::Target {
        &self.context
    }
}
impl ExtensionCommandContext {
    pub async fn wait_for_idle(&self) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        self.actions.wait_for_idle().await?;
        self.runtime.assert_active()
    }
    pub async fn new_session(
        &self,
        options: NewSessionOptions,
    ) -> Result<SessionNavigationResult, ExtensionFailure> {
        self.runtime.assert_active()?;
        self.actions.new_session(options).await
    }
    pub async fn fork(
        &self,
        entry_id: &str,
        options: ForkOptions,
    ) -> Result<SessionNavigationResult, ExtensionFailure> {
        self.runtime.assert_active()?;
        self.actions.fork(entry_id, options).await
    }
    pub async fn navigate_tree(
        &self,
        target_id: &str,
        options: ExtensionTreeNavigationOptions,
    ) -> Result<SessionNavigationResult, ExtensionFailure> {
        self.runtime.assert_active()?;
        let result = self.actions.navigate_tree(target_id, options).await?;
        self.runtime.assert_active()?;
        Ok(result)
    }
    pub async fn edit_assistant_message(
        &self,
        entry_id: &str,
        text: &str,
        options: EditMessageOptions,
    ) -> Result<EditMessageResult, ExtensionFailure> {
        self.runtime.assert_active()?;
        let result = self
            .actions
            .edit_assistant_message(entry_id, text, options)
            .await?;
        self.runtime.assert_active()?;
        Ok(result)
    }
    pub async fn edit_user_message(
        &self,
        entry_id: &str,
        text: &str,
        options: EditMessageOptions,
    ) -> Result<EditMessageResult, ExtensionFailure> {
        self.runtime.assert_active()?;
        let result = self
            .actions
            .edit_user_message(entry_id, text, options)
            .await?;
        self.runtime.assert_active()?;
        Ok(result)
    }
    pub async fn switch_session(
        &self,
        path: &str,
        options: SwitchSessionOptions,
    ) -> Result<SessionNavigationResult, ExtensionFailure> {
        self.runtime.assert_active()?;
        self.actions.switch_session(path, options).await
    }
    pub async fn reload(&self) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        self.actions.reload().await
    }
}
#[derive(Clone)]
pub struct ReplacedSessionContext {
    pub context: ExtensionCommandContext,
    pub message_actions: Arc<dyn ExtensionActions>,
}
impl std::ops::Deref for ReplacedSessionContext {
    type Target = ExtensionCommandContext;
    fn deref(&self) -> &Self::Target {
        &self.context
    }
}
impl ReplacedSessionContext {
    pub async fn send_message(
        &self,
        message: CustomMessage,
        options: SendMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        self.context.runtime.assert_active()?;
        self.message_actions.send_message(message, options)
    }
    pub async fn send_user_message(
        &self,
        content: UserMessageContent,
        options: SendUserMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        self.context.runtime.assert_active()?;
        self.message_actions.send_user_message(content, options)
    }
}
#[derive(Clone)]
pub struct RegisteredCommand {
    pub name: String,
    pub source_info: SourceInfo,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
    pub handler: CommandHandler,
}
#[derive(Clone)]
pub struct ResolvedCommand {
    pub command: RegisteredCommand,
    pub invocation_name: String,
}
#[derive(Clone)]
pub struct RegisteredTool {
    pub definition: ToolDefinition,
    pub source_info: SourceInfo,
}
#[derive(Clone, Debug)]
pub struct ToolInfo {
    pub name: String,
    pub label: String,
    pub description: String,
    pub parameters: JsonValue,
    pub prompt_guidelines: Option<Vec<String>>,
    pub source_info: SourceInfo,
    pub exposure: ToolExposure,
    pub search_text: Option<String>,
    pub search_keywords: Vec<String>,
    pub search_group: Option<String>,
    pub allow_lazy_activation: bool,
    pub kernel_prelude: Option<maho_tools::KernelPrelude>,
}
pub fn normalize_tool_exposure(tool: &ToolDefinition, source_info: SourceInfo) -> ToolInfo {
    let exposure = tool.exposure.unwrap_or(ToolExposure::Direct);
    ToolInfo {
        name: tool.name.clone(),
        label: tool.label.clone(),
        description: tool.description.clone(),
        parameters: tool.parameters.clone(),
        prompt_guidelines: tool.prompt_guidelines.clone(),
        source_info,
        exposure,
        search_text: if exposure == ToolExposure::Search {
            tool.search_text.clone()
        } else {
            None
        },
        search_keywords: tool.search_keywords.clone().unwrap_or_default(),
        search_group: tool.search_group.clone(),
        allow_lazy_activation: tool.allow_lazy_activation != Some(false),
        kernel_prelude: tool.kernel_prelude.clone(),
    }
}
#[derive(Clone, Debug, Default)]
pub struct MessageRenderOptions {
    pub expanded: bool,
    pub output_pad: usize,
}
#[derive(Clone, Debug, Default)]
pub struct EntryRenderOptions {
    pub expanded: bool,
}
pub type MessageRenderer = Arc<
    dyn Fn(&CustomMessage, &MessageRenderOptions, &Theme) -> Option<Box<dyn Component>>
        + Send
        + Sync,
>;
pub type EntryRenderer = Arc<
    dyn Fn(&SessionEntry, &EntryRenderOptions, &Theme) -> Option<Box<dyn Component>> + Send + Sync,
>;
pub type EntryReplaces = Arc<dyn Fn(&SessionEntry, &SessionEntry) -> bool + Send + Sync>;
#[derive(Clone, Default)]
pub struct EntryRendererOptions {
    pub replaces: Option<EntryReplaces>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SessionKind {
    #[default]
    Interactive,
    Worker,
}
pub type SessionContext = BTreeMap<String, String>;
pub const EMPTY_SESSION_CONTEXT: SessionContext = BTreeMap::new();
pub const DEFAULT_EXTENSION_SESSION_PROFILE: ExtensionSessionProfile = ExtensionSessionProfile {
    shared_host_enabled: false,
    session_kind: SessionKind::Interactive,
    session_context: EMPTY_SESSION_CONTEXT,
};
#[derive(Clone, Debug, Default)]
pub struct ExtensionSessionProfile {
    pub shared_host_enabled: bool,
    pub session_kind: SessionKind,
    pub session_context: BTreeMap<String, String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliverAs {
    Steer,
    FollowUp,
    NextTurn,
}
#[derive(Clone, Debug, Default)]
pub struct SendMessageOptions {
    pub trigger_turn: bool,
    pub deliver_as: Option<DeliverAs>,
}

type MessageDeliveryCallback = Box<dyn FnOnce(Result<(), String>) + Send + 'static>;

#[derive(Default)]
struct MessageDeliveryState {
    outcome: Option<Result<(), String>>,
    callback: Option<MessageDeliveryCallback>,
}

/// Settles once when a `send_message` call is durably recorded in the session transcript or fails
/// (admission rejection, abort, queue clear, reload, or disposal). A dropped receipt resolves as
/// failed so a lost task or runtime can never leave a producer waiting forever.
#[derive(Clone, Default)]
pub struct MessageDeliveryReceipt {
    inner: Arc<Mutex<MessageDeliveryState>>,
}

impl MessageDeliveryReceipt {
    pub fn pending() -> Self {
        Self::default()
    }

    pub fn delivered() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MessageDeliveryState {
                outcome: Some(Ok(())),
                callback: None,
            })),
        }
    }

    pub fn failed(reason: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(MessageDeliveryState {
                outcome: Some(Err(reason.into())),
                callback: None,
            })),
        }
    }

    pub fn resolve_delivered(&self) {
        self.settle(Ok(()));
    }

    pub fn resolve_failed(&self, reason: impl Into<String>) {
        self.settle(Err(reason.into()));
    }

    pub fn outcome(&self) -> Option<Result<(), String>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .outcome
            .clone()
    }

    pub fn on_settled(&self, callback: impl FnOnce(Result<(), String>) + Send + 'static) {
        let mut callback = Some(callback);
        let settled = {
            let mut state = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match state.outcome.clone() {
                Some(outcome) => Some(outcome),
                None => {
                    let previous = state.callback.take();
                    let next = callback.take();
                    state.callback = Some(Box::new(move |outcome: Result<(), String>| {
                        if let Some(previous) = previous {
                            previous(outcome.clone());
                        }
                        if let Some(next) = next {
                            next(outcome);
                        }
                    }));
                    None
                }
            }
        };
        if let Some(outcome) = settled
            && let Some(callback) = callback
        {
            callback(outcome);
        }
    }

    fn settle(&self, outcome: Result<(), String>) {
        let callback = {
            let mut state = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.outcome.is_some() {
                return;
            }
            state.outcome = Some(outcome.clone());
            state.callback.take()
        };
        if let Some(callback) = callback {
            callback(outcome);
        }
    }
}

/// Drops with `"custom message delivery dropped before settlement"` unless `disarm` was called.
pub struct MessageDeliveryGuard {
    receipt: Option<MessageDeliveryReceipt>,
    reason: &'static str,
}

impl MessageDeliveryGuard {
    pub fn new(receipt: MessageDeliveryReceipt, reason: &'static str) -> Self {
        Self {
            receipt: Some(receipt),
            reason,
        }
    }

    pub fn disarm(&mut self) {
        self.receipt = None;
    }
}

impl Drop for MessageDeliveryGuard {
    fn drop(&mut self) {
        if let Some(receipt) = self.receipt.take() {
            receipt.resolve_failed(self.reason);
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct SendUserMessageOptions {
    pub deliver_as: Option<StreamingBehavior>,
    pub expand_prompt_templates: bool,
}
#[derive(Clone, Debug)]
pub enum UserMessageContent {
    Text(String),
    Blocks(Vec<ToolContent>),
}
#[derive(Clone, Debug)]
pub struct SendMessageItem {
    pub item_id: String,
    pub producer: String,
    pub run_ref: Option<JsonValue>,
    pub content: Option<Vec<ToolContent>>,
    pub details: Option<JsonValue>,
}
#[derive(Clone, Debug)]
pub struct SendMessageReceipt {
    pub item_id: String,
    pub disposition: String,
    pub notice_id: Option<String>,
    pub entry_id: String,
}

pub trait ExtensionActions: Send + Sync {
    fn lifecycle(&self) -> Result<Arc<dyn crate::LifecycleApi>, ExtensionFailure> {
        Err(ExtensionFailure::new(
            "Extension lifecycle is not bound to a session",
        ))
    }
    fn send_itemized_message(
        &self,
        message: CustomMessage,
        options: SendMessageOptions,
        _items: Vec<SendMessageItem>,
    ) -> ExtensionFuture<'_, Option<Vec<SendMessageReceipt>>> {
        Box::pin(async move {
            self.send_message(message, options)?;
            Ok(None)
        })
    }
    fn arm_tool_barrier(&self, _spec: JsonValue) -> Result<(), ExtensionFailure> {
        Err(ExtensionFailure::new(
            "This runtime has no tool barrier (senpi#3717).",
        ))
    }
    fn release_tool_barrier(&self, _id: &str) -> Result<bool, ExtensionFailure> {
        Ok(false)
    }
    fn relocation_offered(&self) -> Result<bool, ExtensionFailure> {
        Ok(false)
    }
    fn relocation_targets(&self) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err(ExtensionFailure::new("Session relocation is not bound")) })
    }
    fn request_relocation(&self, _request: JsonValue) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err(ExtensionFailure::new("Session relocation is not bound")) })
    }

    fn send_message(
        &self,
        message: CustomMessage,
        options: SendMessageOptions,
    ) -> Result<(), ExtensionFailure>;
    fn send_user_message(
        &self,
        content: UserMessageContent,
        options: SendUserMessageOptions,
    ) -> Result<(), ExtensionFailure>;
    fn append_entry(
        &self,
        custom_type: &str,
        data: Option<JsonValue>,
    ) -> Result<(), ExtensionFailure>;
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure>;
    /// `send_user_message` with the origin captured at call admission.
    fn send_user_message_with_origin(
        &self,
        content: UserMessageContent,
        options: SendUserMessageOptions,
        _input_origin: InputOrigin,
    ) -> Result<(), ExtensionFailure> {
        self.send_user_message(content, options)
    }
    /// Host upload redactor; `None` when the host has none (nothing may then be uploaded).
    fn redact_for_upload(
        &self,
        _text: &str,
        _extra_literals: &[String],
    ) -> Option<UploadRedaction> {
        None
    }
    /// The running host version, when the host reports one.
    fn host_version(&self) -> Option<String> {
        None
    }
    fn get_subscription_limits(&self) -> Vec<AccountLimits> {
        Vec::new()
    }
    fn on_subscription_limits(
        &self,
        _listener: SubscriptionLimitsListener,
    ) -> Option<UiUnsubscribe> {
        None
    }
    /// `send_message` with a delivery receipt that settles only once the message is appended or dropped.
    fn send_message_with_receipt(
        &self,
        message: CustomMessage,
        options: SendMessageOptions,
    ) -> Result<MessageDeliveryReceipt, ExtensionFailure> {
        self.send_message(message, options)?;
        Ok(MessageDeliveryReceipt::delivered())
    }
}

#[derive(Clone, Default)]
pub struct ExecuteToolOptions {
    pub signal: Option<maho_ai::utils::abort::AbortSignal>,
    pub on_update: Option<maho_agent::types::AgentToolUpdateCallback>,
    pub activate_inactive_tool: Option<bool>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecuteToolErrorCode {
    UnknownTool,
    InactiveTool,
    InvalidParams,
    Blocked,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecuteToolError {
    pub code: ExecuteToolErrorCode,
    pub tool_name: String,
    pub message: String,
    pub active_tools: Vec<String>,
}
impl fmt::Display for ExecuteToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ExecuteToolError {}
pub type ExecuteToolFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<maho_agent::types::AgentToolResult, ExecuteToolError>>
            + Send
            + 'a,
    >,
>;
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SlashCommandInfo {
    pub name: String,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
    pub source_info: Option<SourceInfo>,
}
#[derive(Clone, Debug, Default)]
pub struct ExecOptions {
    pub signal: Option<AbortSignal>,
    pub timeout_ms: Option<u64>,
    pub cwd: Option<PathBuf>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
    pub killed: bool,
}

/// Additive session actions keep the original ExtensionActions implementors source-compatible.
/// Hosts bind these separately; an unbound capability fails explicitly, never silently succeeds.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelocationTarget {
    pub name: String,
    pub online: bool,
    pub capacity: Option<String>,
    pub about_to_sleep: Option<bool>,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum RelocationTargetsResult {
    Listed { targets: Vec<RelocationTarget> },
    Unavailable { reason: String, detail: String },
    Refused { reason: String, detail: String },
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum RelocationRequestResult {
    Scheduled {
        target: String,
        #[serde(rename = "moveId")]
        move_id: String,
    },
    Unavailable {
        reason: String,
        detail: String,
    },
    Refused {
        reason: String,
        detail: String,
    },
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RelocationRequest {
    pub target: String,
    pub reason: String,
}
pub trait ExtensionSessionActions: Send + Sync {
    fn relocation_offered(&self) -> bool {
        false
    }
    fn relocation_targets(
        &self,
        _signal: AbortSignal,
    ) -> ExtensionFuture<'_, RelocationTargetsResult> {
        Box::pin(async {
            Ok(RelocationTargetsResult::Unavailable {
                reason: "not_offered".into(),
                detail: "no app connected to this session offers moving it to another computer"
                    .into(),
            })
        })
    }
    fn request_relocation(
        &self,
        _request: RelocationRequest,
        _signal: AbortSignal,
    ) -> ExtensionFuture<'_, RelocationRequestResult> {
        Box::pin(async {
            Ok(RelocationRequestResult::Unavailable {
                reason: "not_offered".into(),
                detail: "no app connected to this session offers moving it to another computer"
                    .into(),
            })
        })
    }
    fn set_session_name(&self, name: &str) -> Result<(), ExtensionFailure>;
    fn get_session_name(&self) -> Result<Option<String>, ExtensionFailure>;
    fn set_label(&self, entry_id: &str, label: Option<&str>) -> Result<(), ExtensionFailure>;
    fn execute_tool<'a>(
        &'a self,
        name: &'a str,
        params: JsonValue,
        options: ExecuteToolOptions,
    ) -> ExecuteToolFuture<'a>;
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure>;
    fn set_active_tools(&self, names: Vec<String>) -> Result<(), ExtensionFailure>;
    fn refresh_tools(&self) -> Result<(), ExtensionFailure>;
    fn install_registered_tool(&self, _tool: RegisteredTool) -> Result<(), ExtensionFailure> {
        self.refresh_tools()
    }
    fn register_removed_tool_hint(&self, name: &str, hint: &str) -> Result<(), ExtensionFailure>;
    fn register_lazy_tool_activator(
        &self,
        activator: LazyToolActivator,
    ) -> Result<(), ExtensionFailure>;
    /// Lets the session consume a rejected native tool-search request (upstream
    /// `getToolSearchService().takeNativeInjectionFailure()`). Hosts without retry ignore it.
    fn register_native_tool_search_injection_failure_source(
        &self,
        _source: NativeToolSearchInjectionFailureSource,
    ) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn get_commands(&self) -> Result<Vec<SlashCommandInfo>, ExtensionFailure>;
    fn set_model(&self, model: Model) -> ExtensionFuture<'_, bool>;
    fn get_thinking_level(&self) -> Result<ThinkingLevel, ExtensionFailure>;
    fn set_thinking_level(&self, level: ThinkingLevel) -> Result<(), ExtensionFailure>;
    fn set_session_model(&self, model: Model) -> ExtensionFuture<'_, bool>;
    fn set_session_thinking_level(&self, level: ThinkingLevel) -> Result<(), ExtensionFailure>;
    /// Off-aware session level: the effective level INCLUDING `off`.
    ///
    /// `ThinkingLevel` (provider effort) cannot express `off`, so a host that must implement
    /// `/reasoning off` reads and writes `ModelThinkingLevel` here instead of guessing
    /// `Off -> Minimal`. Defaults fail explicitly so existing implementors stay source-compatible.
    fn get_model_thinking_level(&self) -> Result<ModelThinkingLevel, ExtensionFailure> {
        Err(ExtensionFailure::new(
            "Model thinking level is not supported by this session",
        ))
    }
    /// Set the session level and persist it as the model's durable thinking level.
    fn set_model_thinking_level(&self, _level: ModelThinkingLevel) -> Result<(), ExtensionFailure> {
        Err(ExtensionFailure::new(
            "Model thinking level is not supported by this session",
        ))
    }
    /// Set the session level only, leaving the durable model level untouched.
    fn set_session_model_thinking_level(
        &self,
        _level: ModelThinkingLevel,
    ) -> Result<(), ExtensionFailure> {
        Err(ExtensionFailure::new(
            "Model thinking level is not supported by this session",
        ))
    }
    fn set_session_fast_mode(&self, enabled: bool) -> Result<(), ExtensionFailure>;
    fn exec<'a>(
        &'a self,
        command: &'a str,
        args: &'a [String],
        cwd: &'a Path,
        options: ExecOptions,
    ) -> ExtensionFuture<'a, ExecResult>;
}

/// Typed companion to the upstream `config-watch:register` in-process protocol.
/// The JSON event bus cannot carry a callable, so in-process registrations carry
/// their `validate` callback here and are delivered to typed listeners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigWatchTargetKind {
    File,
    Dir,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigWatchTargetSpec {
    pub path: PathBuf,
    pub kind: ConfigWatchTargetKind,
    pub filter_globs: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigWatchValidation {
    Ok,
    Rejected { errors: Vec<String> },
}
pub type ConfigWatchValidator =
    Arc<dyn Fn(Vec<PathBuf>) -> ExtensionFuture<'static, ConfigWatchValidation> + Send + Sync>;
#[derive(Clone)]
pub struct RegisteredConfigWatch {
    pub id: String,
    pub display_name: String,
    pub targets: Vec<ConfigWatchTargetSpec>,
    pub validate: ConfigWatchValidator,
}
/// Typed delivery listener: receives the owning extension path plus each registration.
pub type ConfigWatchRegistrationListener = Arc<dyn Fn(&str, &RegisteredConfigWatch) + Send + Sync>;
/// Upstream wire channel for the JSON (callable-free) registration payload.
pub const CONFIG_WATCH_REGISTER_CHANNEL: &str = "config-watch:register";
/// Serializes a typed registration into the upstream `config-watch:register` wire shape.
pub fn config_watch_registration_json(registration: &RegisteredConfigWatch) -> JsonValue {
    let targets: Vec<JsonValue> = registration
        .targets
        .iter()
        .map(|target| {
            JsonValue::Object(
                [
                    (
                        String::from("path"),
                        JsonValue::String(target.path.to_string_lossy().into_owned()),
                    ),
                    (
                        String::from("kind"),
                        JsonValue::String(
                            (match target.kind {
                                ConfigWatchTargetKind::File => "file",
                                ConfigWatchTargetKind::Dir => "dir",
                            })
                            .into(),
                        ),
                    ),
                    (
                        String::from("filterGlobs"),
                        JsonValue::Array(
                            target
                                .filter_globs
                                .iter()
                                .cloned()
                                .map(JsonValue::String)
                                .collect(),
                        ),
                    ),
                ]
                .into_iter()
                .collect(),
            )
        })
        .collect();
    JsonValue::Object(
        [
            (
                String::from("id"),
                JsonValue::String(registration.id.clone()),
            ),
            (
                String::from("displayName"),
                JsonValue::String(registration.display_name.clone()),
            ),
            (String::from("targets"), JsonValue::Array(targets)),
        ]
        .into_iter()
        .collect(),
    )
}

pub type BusHandler = Arc<dyn Fn(&JsonValue) + Send + Sync>;
type NativeBusHandler = Arc<dyn Fn(&(dyn std::any::Any + Send + Sync)) + Send + Sync>;
#[derive(Clone, Default)]
struct BusState {
    next_id: u64,
    handlers: BTreeMap<String, Vec<(u64, BusHandler)>>,
    native_handlers: BTreeMap<String, Vec<(u64, NativeBusHandler)>>,
    config_watches: Vec<(u64, String, RegisteredConfigWatch)>,
}
#[derive(Clone, Default)]
pub struct EventBus {
    state: Arc<Mutex<BusState>>,
    registration_stale: Arc<std::sync::atomic::AtomicBool>,
    runtime: Option<ExtensionRuntime>,
    registration_subscriptions: Arc<Mutex<Vec<u64>>>,
}
pub struct BusSubscription {
    state: Arc<Mutex<BusState>>,
    channel: String,
    id: u64,
}
impl Drop for BusSubscription {
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(handlers) = state.handlers.get_mut(&self.channel) {
            handlers.retain(|(id, _)| *id != self.id);
        }
        if let Some(handlers) = state.native_handlers.get_mut(&self.channel) {
            handlers.retain(|(id, _)| *id != self.id);
        }
    }
}
impl EventBus {
    pub fn registration_scope(&self) -> Self {
        Self {
            state: self.state.clone(),
            registration_stale: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            runtime: self.runtime.clone(),
            registration_subscriptions: Arc::default(),
        }
    }
    pub fn bind_runtime(&mut self, runtime: ExtensionRuntime) {
        self.runtime = Some(runtime);
    }
    fn assert_active_or_panic(&self) {
        if self
            .registration_stale
            .load(std::sync::atomic::Ordering::Acquire)
        {
            std::panic::panic_any(ExtensionFailure::new("Extension factory failed to load"));
        }
        if let Some(runtime) = &self.runtime {
            runtime.assert_active_or_panic();
        }
    }
    pub fn invalidate_registration(&self) {
        self.registration_stale
            .store(true, std::sync::atomic::Ordering::Release);
        let owned = std::mem::take(
            &mut *self
                .registration_subscriptions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for handlers in state.handlers.values_mut() {
            handlers.retain(|(id, _)| !owned.contains(id));
        }
        for handlers in state.native_handlers.values_mut() {
            handlers.retain(|(id, _)| !owned.contains(id));
        }
        state
            .config_watches
            .retain(|(id, _, _)| !owned.contains(id));
    }
    pub fn registration_checkpoint(&self) -> EventBusCheckpoint {
        EventBusCheckpoint(
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        )
    }
    pub fn rollback_registration(&self, checkpoint: EventBusCheckpoint) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let next_id = state.next_id;
        *state = checkpoint.0;
        state.next_id = next_id;
    }
    pub fn on(&self, channel: &str, handler: BusHandler) -> BusSubscription {
        self.assert_active_or_panic();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state
            .handlers
            .entry(channel.into())
            .or_default()
            .push((id, handler));
        self.registration_subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(id);
        BusSubscription {
            state: Arc::clone(&self.state),
            channel: channel.into(),
            id,
        }
    }
    pub fn emit(&self, channel: &str, data: &JsonValue) {
        self.assert_active_or_panic();
        if channel == "config-watch:unregister"
            && let Some(id) = data.get("id").and_then(JsonValue::as_str)
        {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .config_watches
                .retain(|(_, _, registration)| registration.id != id);
        }
        let handlers = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .handlers
            .get(channel)
            .cloned()
            .unwrap_or_default();
        for (_, handler) in handlers {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(data))).is_err() {
                eprintln!("Event handler error ({channel}): native handler panicked");
            }
        }
    }
    /// Subscribe to native payloads without serializing contexts or callback objects.
    pub fn on_native<T: Send + Sync + 'static>(
        &self,
        channel: &str,
        handler: Arc<dyn Fn(&T) + Send + Sync>,
    ) -> BusSubscription {
        self.assert_active_or_panic();
        let erased: NativeBusHandler = Arc::new(move |data| {
            if let Some(data) = data.downcast_ref::<T>() {
                handler(data);
            }
        });
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state
            .native_handlers
            .entry(channel.into())
            .or_default()
            .push((id, erased));
        self.registration_subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(id);
        BusSubscription {
            state: Arc::clone(&self.state),
            channel: channel.into(),
            id,
        }
    }
    /// Publish the same borrowed native object to every matching subscriber.
    pub fn emit_native<T: Send + Sync + 'static>(&self, channel: &str, data: &T) {
        self.assert_active_or_panic();
        let handlers = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .native_handlers
            .get(channel)
            .cloned()
            .unwrap_or_default();
        for (_, handler) in handlers {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(data))).is_err() {
                eprintln!("Event handler error ({channel}): native handler panicked");
            }
        }
    }
    /// Retain callable registrations in the same checkpointed, scoped bus state.
    pub fn publish_config_watch(&self, path: &str, registration: RegisteredConfigWatch) {
        self.assert_active_or_panic();
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let id = state.next_id;
            state.next_id = state.next_id.wrapping_add(1);
            state
                .config_watches
                .retain(|(_, owner, existing)| owner != path || existing.id != registration.id);
            state
                .config_watches
                .push((id, path.into(), registration.clone()));
            self.registration_subscriptions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(id);
        }
        self.emit_native(
            CONFIG_WATCH_REGISTER_CHANNEL,
            &(path.to_owned(), registration.clone()),
        );
        self.emit(
            CONFIG_WATCH_REGISTER_CHANNEL,
            &config_watch_registration_json(&registration),
        );
    }
    pub fn config_watch_registrations(&self) -> Vec<(String, RegisteredConfigWatch)> {
        self.assert_active_or_panic();
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .config_watches
            .iter()
            .map(|(_, path, registration)| (path.clone(), registration.clone()))
            .collect()
    }
    pub fn clear(&self) {
        if self
            .registration_stale
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.handlers.clear();
        state.native_handlers.clear();
        state.config_watches.clear();
    }
}
pub struct EventBusCheckpoint(BusState);

/// Native factories are passed by the CLI as an explicit Vec<Box<dyn Extension>>.
pub trait Extension: Send + Sync {
    fn register(&self, api: &mut ExtensionApi);
}
#[derive(Clone)]
pub struct LoadedExtension {
    pub identity: ExtensionIdentity,
    /// senpi `Extension.hidden` (extensions/types.ts:2573): omitted from the startup Extensions list.
    pub hidden: bool,
    pub source_info: SourceInfo,
    pub registration_cwd: PathBuf,
    pub handlers: BTreeMap<EventKind, Vec<ExtensionHandler>>,
    pub handler_options: BTreeMap<EventKind, Vec<ExtensionHandlerOptions>>,
    pub tools: Vec<RegisteredTool>,
    pub commands: Vec<RegisteredCommand>,
    pub flags: Vec<ExtensionFlag>,
    pub message_renderers: BTreeMap<String, MessageRenderer>,
    pub entry_renderers: BTreeMap<String, EntryRenderer>,
    pub entry_renderer_options: BTreeMap<String, EntryRendererOptions>,
    pub mcp_servers: Vec<RegisteredMcpServerDeclaration>,
    pub removed_tool_hints: BTreeMap<String, String>,
    pub filesystem_policies: Vec<FilesystemPolicy>,
    pub shortcuts: BTreeMap<String, ExtensionShortcut>,
    pub lazy_tool_activators: Vec<LazyToolActivator>,
    pub native_tool_search_injection_failure_sources: Vec<NativeToolSearchInjectionFailureSource>,
    pub markdown_transformer: Option<MarkdownTransformer>,
    pub rpc_handlers: BTreeMap<String, ExtensionRpcRequestHandler>,
    pub command_context_handlers: BTreeMap<String, CommandContextHandler>,
    pub command_argument_completions: BTreeMap<String, CommandArgumentCompletions>,
    pub tool_renderers: BTreeMap<String, Arc<dyn std::any::Any + Send + Sync>>,
    pub erased_tool_renderers: BTreeMap<String, Arc<dyn ErasedToolRenderers>>,
    pub config_watch_registrations: Vec<RegisteredConfigWatch>,
    pub fast_mode_action: Option<FastModeAction>,
    /// Child modules registered through `register_child_module`, keyed by registration id.
    pub child_modules: BTreeMap<String, RegisteredChildModule>,
}
impl LoadedExtension {
    pub fn new(path: &str, cwd: PathBuf, source_info: SourceInfo) -> Self {
        Self {
            identity: ExtensionIdentity {
                path: path.into(),
                resolved_path: path.into(),
            },
            hidden: false,
            source_info,
            registration_cwd: cwd,
            handlers: BTreeMap::new(),
            handler_options: BTreeMap::new(),
            tools: Vec::new(),
            commands: Vec::new(),
            flags: Vec::new(),
            message_renderers: BTreeMap::new(),
            entry_renderers: BTreeMap::new(),
            entry_renderer_options: BTreeMap::new(),
            mcp_servers: Vec::new(),
            removed_tool_hints: BTreeMap::new(),
            filesystem_policies: Vec::new(),
            shortcuts: BTreeMap::new(),
            lazy_tool_activators: Vec::new(),
            native_tool_search_injection_failure_sources: Vec::new(),
            markdown_transformer: None,
            rpc_handlers: BTreeMap::new(),
            command_context_handlers: BTreeMap::new(),
            command_argument_completions: BTreeMap::new(),
            tool_renderers: BTreeMap::new(),
            erased_tool_renderers: BTreeMap::new(),
            config_watch_registrations: Vec::new(),
            fast_mode_action: None,
            child_modules: BTreeMap::new(),
        }
    }
}
#[derive(Clone, Default)]
struct RuntimeState {
    flags: BTreeMap<String, FlagValue>,
    memory_reporters: BTreeMap<String, BTreeMap<String, MemoryReporter>>,
    actions: Option<Arc<dyn ExtensionActions>>,
    stale: Option<String>,
    provider_actions: Option<Arc<dyn ExtensionProviderActions>>,
    pending_providers: Vec<(ProviderRegistration, String)>,
    read_classifiers: Vec<(u64, ReadClassifier)>,
    next_classifier_id: u64,
    session_actions: Option<Arc<dyn ExtensionSessionActions>>,
    provider_errors: Vec<ExtensionError>,
    registered_providers: BTreeMap<String, String>,
    live_handlers: BTreeMap<(String, EventKind), Vec<ExtensionHandler>>,
    live_handler_options: BTreeMap<(String, EventKind), Vec<ExtensionHandlerOptions>>,
    live_commands: BTreeMap<String, LiveCommandRegistrations>,
    live_command_argument_completions:
        BTreeMap<String, BTreeMap<String, CommandArgumentCompletions>>,
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
    live_erased_tool_renderers: BTreeMap<String, BTreeMap<String, Arc<dyn ErasedToolRenderers>>>,
    /// Bumped on every change to `live_tools` / `live_tool_renderers` / `live_erased_tool_renderers`,
    /// so a caller can rebuild a tool or renderer snapshot only when the registry actually moved.
    tool_generation: u64,
    extension_tool_executors: BTreeMap<(String, String), ExtensionToolExecutor>,
    eval_handle_host: Option<SharedEvalHandleHost>,
}
pub type LiveCommandRegistrations = (
    Vec<RegisteredCommand>,
    BTreeMap<String, CommandContextHandler>,
);
pub type LiveEntryRenderers = BTreeMap<String, (EntryRenderer, Option<EntryRendererOptions>)>;
#[derive(Clone, Default)]
pub struct ExtensionRuntime {
    state: Arc<Mutex<RuntimeState>>,
    registration_stale: Arc<Mutex<Option<String>>>,
    registration_pending: Arc<Mutex<Option<RegistrationPending>>>,
    registration_classifiers: Arc<Mutex<Vec<u64>>>,
    person_answer_pending: Arc<std::sync::atomic::AtomicBool>,
}
#[derive(Default)]
struct RegistrationPending {
    flags: BTreeMap<String, FlagValue>,
    memory_reporters: BTreeMap<String, BTreeMap<String, MemoryReporter>>,
    providers: Vec<ProviderRegistrationChange>,
    tool_executors: BTreeMap<(String, String), ExtensionToolExecutor>,
    tools: BTreeMap<String, Vec<RegisteredTool>>,
    tool_renderers: BTreeMap<String, BTreeMap<String, Arc<dyn std::any::Any + Send + Sync>>>,
    erased_tool_renderers: BTreeMap<String, BTreeMap<String, Arc<dyn ErasedToolRenderers>>>,
    eval_handle_host: Option<SharedEvalHandleHost>,
}
enum ProviderRegistrationChange {
    Register(ProviderRegistration, String),
    Unregister(String, String),
}
pub struct RuntimeRegistrationCheckpoint(RuntimeState);
impl ExtensionRuntime {
    pub fn registration_scope(&self) -> Self {
        Self {
            state: self.state.clone(),
            registration_stale: Arc::new(Mutex::new(None)),
            registration_pending: Arc::new(Mutex::new(Some(RegistrationPending::default()))),
            registration_classifiers: Arc::default(),
            person_answer_pending: self.person_answer_pending.clone(),
        }
    }
    /// Marks that a question answer is being delivered: the `send_user_message` that follows
    /// synchronously is a person's input. The mark also lapses on the next scheduler turn.
    pub fn mark_person_answer_pending(&self) {
        self.person_answer_pending
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let pending = self.person_answer_pending.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            drop(handle.spawn(async move {
                pending.store(false, std::sync::atomic::Ordering::SeqCst);
            }));
        }
    }
    /// Classifies a message at admission and resets the token.
    pub fn take_input_origin(&self) -> InputOrigin {
        if self
            .person_answer_pending
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            InputOrigin::Person
        } else {
            InputOrigin::Extension
        }
    }
    pub fn commit_registration(&self) -> Result<(), ExtensionFailure> {
        self.assert_active()?;
        let pending = self
            .registration_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(pending) = pending {
            {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for (name, value) in pending.flags {
                    state.flags.entry(name).or_insert(value);
                }
                for (path, reporters) in pending.memory_reporters {
                    state
                        .memory_reporters
                        .entry(path)
                        .or_default()
                        .extend(reporters);
                }
                state
                    .extension_tool_executors
                    .extend(pending.tool_executors);
                state.live_tools.extend(pending.tools);
                state.live_tool_renderers.extend(pending.tool_renderers);
                state
                    .live_erased_tool_renderers
                    .extend(pending.erased_tool_renderers);
                state.tool_generation += 1;
                if let Some(host) = pending.eval_handle_host {
                    state.eval_handle_host = Some(host);
                }
            }
            for change in pending.providers {
                match change {
                    ProviderRegistrationChange::Register(registration, path) => {
                        self.register_provider(registration, &path)?
                    }
                    ProviderRegistrationChange::Unregister(name, path) => {
                        self.unregister_provider(&name, &path)?
                    }
                }
            }
        }
        Ok(())
    }
    fn register_flag_default(&self, name: &str, value: FlagValue) {
        let mut pending = self
            .registration_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pending) = pending.as_mut() {
            pending.flags.entry(name.into()).or_insert(value);
        } else {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .flags
                .entry(name.into())
                .or_insert(value);
        }
    }
    /// Stores a reporter without invoking it, staging it until factory registration commits.
    ///
    /// # Errors
    /// Returns an error for a stale runtime or a reserved report section name.
    pub fn register_memory_reporter(
        &self,
        path: &str,
        name: &str,
        reporter: MemoryReporter,
    ) -> Result<(), ExtensionFailure> {
        self.assert_active()?;
        if RESERVED_MEMORY_REPORT_KEYS.contains(&name) {
            return Err(ExtensionFailure::new(format!(
                "Memory reporter name \"{name}\" is reserved by the memory report"
            )));
        }
        let mut pending = self
            .registration_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pending) = pending.as_mut() {
            pending
                .memory_reporters
                .entry(path.into())
                .or_default()
                .insert(name.into(), reporter);
        } else {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .memory_reporters
                .entry(path.into())
                .or_default()
                .insert(name.into(), reporter);
        }
        Ok(())
    }
    /// Clones a loaded extension's callbacks; no extension code runs under the registry lock.
    pub fn memory_reporters(&self, path: &str) -> Vec<NamedMemoryReporter> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .memory_reporters
            .get(path)
            .into_iter()
            .flat_map(|reporters| reporters.iter())
            .map(|(name, reporter)| NamedMemoryReporter {
                name: name.clone(),
                reporter: Arc::clone(reporter),
            })
            .collect()
    }
    pub fn invalidate_registration(&self, message: &str) {
        self.registration_stale
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert_with(|| message.into());
        self.registration_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let owned = std::mem::take(
            &mut *self
                .registration_classifiers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .read_classifiers
            .retain(|(id, _)| !owned.contains(id));
    }
    pub fn registration_checkpoint(&self) -> RuntimeRegistrationCheckpoint {
        RuntimeRegistrationCheckpoint(
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        )
    }
    pub fn rollback_registration(&self, checkpoint: RuntimeRegistrationCheckpoint) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let next_id = state.next_classifier_id;
        let tool_generation = state.tool_generation;
        *state = checkpoint.0;
        state.next_classifier_id = next_id;
        state.tool_generation = tool_generation + 1;
    }
    pub fn bind_session_actions(&self, actions: Arc<dyn ExtensionSessionActions>) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session_actions = Some(actions);
    }
    pub fn session_actions(&self) -> Result<Arc<dyn ExtensionSessionActions>, ExtensionFailure> {
        self.assert_active()?;
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session_actions
            .clone()
            .ok_or_else(|| {
                ExtensionFailure::new(
                    "Extension session actions are unavailable during registration",
                )
            })
    }
    pub fn message_actions(&self) -> Result<Arc<dyn ExtensionActions>, ExtensionFailure> {
        self.assert_active()?;
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .actions
            .clone()
            .ok_or_else(|| {
                ExtensionFailure::new(
                    "Extension message actions are unavailable during registration",
                )
            })
    }
    pub fn eval_handle_host(&self) -> Result<Option<SharedEvalHandleHost>, ExtensionFailure> {
        self.assert_active()?;
        Ok(self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .eval_handle_host
            .clone())
    }
    pub fn provide_eval_handle_host(
        &self,
        host: SharedEvalHandleHost,
    ) -> Result<(), ExtensionFailure> {
        self.assert_active()?;
        let mut pending = self
            .registration_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pending) = pending.as_mut() {
            pending.eval_handle_host = Some(host);
        } else {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .eval_handle_host = Some(host);
        }
        Ok(())
    }
    fn assert_active_or_panic(&self) {
        if let Err(error) = self.assert_active() {
            std::panic::panic_any(error);
        }
    }
    pub fn assert_active(&self) -> Result<(), ExtensionFailure> {
        if let Some(message) = &*self
            .registration_stale
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return Err(ExtensionFailure::new(message.clone()));
        }
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(message) = &state.stale {
            return Err(ExtensionFailure::new(message.clone()));
        }
        Ok(())
    }
    pub fn invalidate(&self, message: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.stale.get_or_insert_with(|| message.into());
        state.read_classifiers.clear();
        state.memory_reporters.clear();
        state.pending_providers.clear();
        state.live_handlers.clear();
        state.live_handler_options.clear();
        state.live_commands.clear();
        state.live_command_argument_completions.clear();
        state.live_shortcuts.clear();
        state.live_markdown_transformers.clear();
        state.live_rpc_handlers.clear();
        state.live_flags.clear();
        state.live_tools.clear();
        state.live_tool_renderers.clear();
        state.live_erased_tool_renderers.clear();
        state.extension_tool_executors.clear();
        state.tool_generation += 1;
        state.eval_handle_host = None;
        state.live_mcp_servers.clear();
        state.live_message_renderers.clear();
        state.live_entry_renderers.clear();
        state.live_filesystem_policies.clear();
    }
    pub fn bind_providers(
        &self,
        actions: Arc<dyn ExtensionProviderActions>,
    ) -> Result<(), ExtensionFailure> {
        self.assert_active()?;
        let pending = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.provider_actions = Some(Arc::clone(&actions));
            std::mem::take(&mut state.pending_providers)
        };
        for (registration, path) in pending {
            let name = registration.name().to_owned();
            if let Err(error) = actions.register_provider(registration, &path) {
                self.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .provider_errors
                    .push(ExtensionError {
                        extension_path: path,
                        event: "register_provider".into(),
                        error: error.message,
                        stack: error.stack,
                    });
            } else {
                self.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .registered_providers
                    .insert(name, path);
            }
        }
        Ok(())
    }
    pub fn dispose_providers(&self) -> Result<(), ExtensionFailure> {
        let (actions, providers) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                state.provider_actions.clone(),
                std::mem::take(&mut state.registered_providers),
            )
        };
        if let Some(actions) = actions {
            for (name, path) in providers {
                actions.unregister_provider(&name, &path)?;
            }
        }
        Ok(())
    }
    pub fn take_provider_errors(&self) -> Vec<ExtensionError> {
        std::mem::take(
            &mut self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .provider_errors,
        )
    }
    pub fn has_live_handlers(&self, path: &str, kind: EventKind) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_handlers
            .get(&(path.into(), kind))
            .is_some_and(|handlers| !handlers.is_empty())
    }
    pub fn live_handlers(&self, path: &str, kind: EventKind) -> Option<Vec<ExtensionHandler>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_handlers
            .get(&(path.into(), kind))
            .cloned()
    }
    pub fn live_handlers_with_options(
        &self,
        path: &str,
        kind: EventKind,
    ) -> Option<(Vec<ExtensionHandler>, Vec<ExtensionHandlerOptions>)> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Some((
            state.live_handlers.get(&(path.into(), kind))?.clone(),
            state
                .live_handler_options
                .get(&(path.into(), kind))?
                .clone(),
        ))
    }
    pub fn live_commands(&self, path: &str) -> Option<LiveCommandRegistrations> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_commands
            .get(path)
            .cloned()
    }
    pub fn live_shortcuts(&self, path: &str) -> Option<BTreeMap<String, ExtensionShortcut>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_shortcuts
            .get(path)
            .cloned()
    }
    pub fn live_markdown_transformer(&self, path: &str) -> Option<MarkdownTransformer> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_markdown_transformers
            .get(path)
            .cloned()
    }
    pub fn live_rpc_handlers(
        &self,
        path: &str,
    ) -> Option<BTreeMap<String, ExtensionRpcRequestHandler>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_rpc_handlers
            .get(path)
            .cloned()
    }
    pub fn live_flags(&self, path: &str) -> Option<Vec<ExtensionFlag>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_flags
            .get(path)
            .cloned()
    }
    pub fn live_tools(&self, path: &str) -> Option<Vec<RegisteredTool>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_tools
            .get(path)
            .cloned()
    }
    /// Whether the live tool list for `path` has a tool named `name`, checked under the lock without
    /// cloning the list; `None` when no live list is published for `path` (same as `live_tools`).
    pub fn live_tools_contain(&self, path: &str, name: &str) -> Option<bool> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_tools
            .get(path)
            .map(|tools| tools.iter().any(|tool| tool.definition.name == name))
    }
    /// Changes whenever the live tool or tool-renderer registrations change.
    pub fn tool_generation(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .tool_generation
    }
    pub fn live_tool_definition(&self, name: &str) -> Option<ToolDefinition> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for tools in state.live_tools.values() {
            if let Some(tool) = tools.iter().rev().find(|t| t.definition.name == name) {
                return Some(tool.definition.clone());
            }
        }
        None
    }
    pub fn live_mcp_servers(&self, path: &str) -> Option<Vec<RegisteredMcpServerDeclaration>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_mcp_servers
            .get(path)
            .cloned()
    }
    pub fn live_filesystem_policies(&self, path: &str) -> Option<Vec<FilesystemPolicy>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_filesystem_policies
            .get(path)
            .cloned()
    }
    pub fn live_command_argument_completions(
        &self,
        path: &str,
    ) -> Option<BTreeMap<String, CommandArgumentCompletions>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_command_argument_completions
            .get(path)
            .cloned()
    }
    pub fn live_message_renderers(&self, path: &str) -> Option<BTreeMap<String, MessageRenderer>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_message_renderers
            .get(path)
            .cloned()
    }
    pub fn live_entry_renderers(&self, path: &str) -> Option<LiveEntryRenderers> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_entry_renderers
            .get(path)
            .cloned()
    }
    pub fn live_tool_renderer(
        &self,
        path: &str,
        name: &str,
    ) -> Option<Option<Arc<dyn std::any::Any + Send + Sync>>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_tool_renderers
            .get(path)
            .map(|renderers| renderers.get(name).cloned())
    }
    pub fn live_erased_tool_renderer(
        &self,
        path: &str,
        name: &str,
    ) -> Option<Option<Arc<dyn ErasedToolRenderers>>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_erased_tool_renderers
            .get(path)
            .map(|renderers| renderers.get(name).cloned())
    }
    pub fn extension_tool_executor(&self, path: &str, name: &str) -> Option<ExtensionToolExecutor> {
        let execute = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extension_tool_executors
            .get(&(path.into(), name.into()))
            .cloned()?;
        let runtime = self.clone();
        Some(Arc::new(move |id, params, signal, update, context| {
            let execute = execute.clone();
            let runtime = runtime.clone();
            Box::pin(async move {
                runtime.assert_active()?;
                let result = execute(id, params, signal, update, context).await?;
                runtime.assert_active()?;
                Ok(result)
            })
        }))
    }
    pub fn register_provider(
        &self,
        registration: ProviderRegistration,
        path: &str,
    ) -> Result<(), ExtensionFailure> {
        self.assert_active()?;
        let config = match &registration {
            ProviderRegistration::Config { name, config } => Some((name, config.as_ref())),
            ProviderRegistration::ConfigOptions { name, options } => Some((name, &options.config)),
            ProviderRegistration::Native(_) => None,
        };
        if let Some((name, config)) = config {
            if config.stream_simple.is_some() && config.api.is_none() {
                return Err(ExtensionFailure::new(format!(
                    "Provider {name}: \"api\" is required when registering streamSimple."
                )));
            }
            if config
                .extra_body
                .as_ref()
                .is_some_and(|body| !body.is_object())
            {
                return Err(ExtensionFailure::new(format!(
                    "Provider {name}: extraBody must be an object"
                )));
            }
            for model in config.models.iter().flatten() {
                if model
                    .extra_body
                    .as_ref()
                    .is_some_and(|body| !body.is_object())
                {
                    return Err(ExtensionFailure::new(format!(
                        "Provider {name}, model {}: extraBody must be an object",
                        model.id
                    )));
                }
            }
        }
        {
            let mut pending = self
                .registration_pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(pending) = pending.as_mut() {
                pending.providers.push(ProviderRegistrationChange::Register(
                    registration,
                    path.into(),
                ));
                return Ok(());
            }
        }
        let actions = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match &state.provider_actions {
                Some(actions) => Arc::clone(actions),
                None => {
                    state.pending_providers.push((registration, path.into()));
                    return Ok(());
                }
            }
        };
        let name = registration.name().to_owned();
        actions.register_provider(registration, path)?;
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .registered_providers
            .insert(name, path.into());
        Ok(())
    }
    pub fn unregister_provider(&self, name: &str, path: &str) -> Result<(), ExtensionFailure> {
        self.assert_active()?;
        {
            let mut pending = self
                .registration_pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(pending) = pending.as_mut() {
                pending
                    .providers
                    .push(ProviderRegistrationChange::Unregister(
                        name.into(),
                        path.into(),
                    ));
                return Ok(());
            }
        }
        let actions = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state
                .pending_providers
                .retain(|(registration, _)| registration.name() != name);
            state.provider_actions.clone()
        };
        if let Some(actions) = actions {
            actions.unregister_provider(name, path)?;
        }
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .registered_providers
            .remove(name);
        Ok(())
    }
    pub fn classify_read(&self, path: &Path, cwd: &Path) -> Option<CompactReadClassification> {
        let classifiers = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .read_classifiers
            .clone();
        for (_, classifier) in classifiers {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| classifier(path, cwd))) {
                Ok(Some(classification)) => return Some(classification),
                Ok(None) => {}
                Err(_) => eprintln!("Read classifier failed: native classifier panicked"),
            }
        }
        None
    }
    pub fn bind(&self, actions: Arc<dyn ExtensionActions>) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .actions = Some(actions);
    }
    fn actions(&self) -> Result<Arc<dyn ExtensionActions>, ExtensionFailure> {
        self.assert_active()?;
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .actions
            .clone()
            .ok_or_else(|| {
                ExtensionFailure::new("Extension actions are unavailable during registration")
            })
    }
    pub fn get_flag(&self, name: &str) -> Option<FlagValue> {
        self.assert_active_or_panic();
        let existing = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .flags
            .get(name)
            .cloned();
        existing.or_else(|| {
            self.registration_pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .and_then(|pending| pending.flags.get(name).cloned())
        })
    }
    pub fn set_flag(&self, name: &str, value: FlagValue) {
        self.assert_active_or_panic();
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .flags
            .insert(name.into(), value);
    }
    /// Snapshot of `runtime.flagValues`: every committed flag value (registered defaults and CLI overrides).
    pub fn flag_values(&self) -> BTreeMap<String, FlagValue> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .flags
            .clone()
    }
}
pub struct ExtensionApi {
    pub cwd: PathBuf,
    pub profile: ExtensionSessionProfile,
    pub events: EventBus,
    pub runtime: ExtensionRuntime,
    pub registered: LoadedExtension,
}
impl ExtensionApi {
    pub fn new(
        registered: LoadedExtension,
        profile: ExtensionSessionProfile,
        mut events: EventBus,
        runtime: ExtensionRuntime,
    ) -> Self {
        events.bind_runtime(runtime.clone());
        Self {
            cwd: registered.registration_cwd.clone(),
            profile,
            events,
            runtime,
            registered,
        }
    }
    pub fn provide_eval_handle_host(&self, host: SharedEvalHandleHost) {
        if let Err(error) = self.runtime.provide_eval_handle_host(host) {
            std::panic::panic_any(error);
        }
    }
    /// Adds figures read only when a memory report is requested.
    ///
    /// # Errors
    /// Returns an error for a stale registration or a reserved report section name.
    pub fn register_memory_reporter(
        &self,
        name: &str,
        reporter: MemoryReporter,
    ) -> Result<(), ExtensionFailure> {
        self.runtime
            .register_memory_reporter(&self.registered.identity.path, name, reporter)
    }
    pub fn on(&mut self, event: EventKind, handler: ExtensionHandler) {
        self.on_with_options(event, handler, ExtensionHandlerOptions::default());
    }
    pub fn on_with_options(
        &mut self,
        event: EventKind,
        handler: ExtensionHandler,
        options: ExtensionHandlerOptions,
    ) {
        self.runtime.assert_active_or_panic();
        let handlers = self.registered.handlers.entry(event).or_default();
        handlers.push(handler);
        self.registered
            .handler_options
            .entry(event)
            .or_default()
            .push(options);
        if self
            .runtime
            .registration_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
        {
            let mut state = self
                .runtime
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.live_handlers.insert(
                (self.registered.identity.path.clone(), event),
                handlers.clone(),
            );
            state.live_handler_options.insert(
                (self.registered.identity.path.clone(), event),
                self.registered
                    .handler_options
                    .get(&event)
                    .cloned()
                    .unwrap_or_default(),
            );
        }
    }
    pub fn register_provider(
        &self,
        name: &str,
        config: ProviderConfig,
    ) -> Result<(), ExtensionFailure> {
        self.runtime.register_provider(
            ProviderRegistration::Config {
                name: name.into(),
                config: Box::new(config),
            },
            &self.registered.identity.path,
        )
    }
    pub fn register_provider_with_options(
        &self,
        name: &str,
        options: ProviderConfigOptions,
    ) -> Result<(), ExtensionFailure> {
        self.runtime.register_provider(
            ProviderRegistration::ConfigOptions {
                name: name.into(),
                options: Box::new(options),
            },
            &self.registered.identity.path,
        )
    }
    pub fn register_provider_object(
        &self,
        name: &str,
        mut config: ProviderObjectConfig,
    ) -> Result<(), ExtensionFailure> {
        if let Some(body) = config.extra_body {
            config.config.extra_body = Some(JsonValue::Object(body.into_iter().collect()));
        }
        if let Some(models) = &mut config.config.models {
            for model in models {
                if let Some(body) = config.model_extra_bodies.remove(&model.id) {
                    model.extra_body = Some(JsonValue::Object(body.into_iter().collect()));
                }
            }
        }
        self.register_provider(name, config.config)
    }
    pub fn register_native_provider(
        &self,
        provider: Arc<dyn maho_ai::models::Provider>,
    ) -> Result<(), ExtensionFailure> {
        self.runtime.register_provider(
            ProviderRegistration::Native(provider),
            &self.registered.identity.path,
        )
    }
    pub fn unregister_provider(&self, name: &str) -> Result<(), ExtensionFailure> {
        self.runtime
            .unregister_provider(name, &self.registered.identity.path)
    }
    pub fn register_shortcut(
        &mut self,
        shortcut: &str,
        description: Option<String>,
        handler: ShortcutHandler,
    ) {
        self.runtime.assert_active_or_panic();
        self.registered.shortcuts.insert(
            shortcut.into(),
            ExtensionShortcut {
                shortcut: shortcut.into(),
                description,
                handler,
                extension_path: self.registered.identity.path.clone(),
            },
        );
        if self
            .runtime
            .registration_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
        {
            self.runtime
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .live_shortcuts
                .insert(
                    self.registered.identity.path.clone(),
                    self.registered.shortcuts.clone(),
                );
        }
    }
    pub fn register_lazy_tool_activator(&mut self, activator: LazyToolActivator) {
        self.runtime.assert_active_or_panic();
        let actions = self
            .runtime
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session_actions
            .clone();
        if let Some(actions) = actions
            && let Err(error) = actions.register_lazy_tool_activator(Arc::clone(&activator))
        {
            std::panic::panic_any(error);
        }
        self.registered.lazy_tool_activators.push(activator);
    }
    pub fn register_native_tool_search_injection_failure_source(
        &mut self,
        source: NativeToolSearchInjectionFailureSource,
    ) {
        self.runtime.assert_active_or_panic();
        let actions = self
            .runtime
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session_actions
            .clone();
        if let Some(actions) = actions
            && let Err(error) =
                actions.register_native_tool_search_injection_failure_source(Arc::clone(&source))
        {
            std::panic::panic_any(error);
        }
        self.registered
            .native_tool_search_injection_failure_sources
            .push(source);
    }
    pub fn register_markdown_transformer(&mut self, transformer: MarkdownTransformer) {
        self.runtime.assert_active_or_panic();
        self.registered.markdown_transformer = Some(transformer.clone());
        if self
            .runtime
            .registration_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
        {
            self.runtime
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .live_markdown_transformers
                .insert(self.registered.identity.path.clone(), transformer);
        }
    }
    pub fn register_read_classifier(
        &self,
        classifier: ReadClassifier,
    ) -> Result<ReadClassifierSubscription, ExtensionFailure> {
        self.runtime.assert_active()?;
        let mut state = self
            .runtime
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = state.next_classifier_id;
        state.next_classifier_id = id.wrapping_add(1);
        state.read_classifiers.push((id, classifier));
        self.runtime
            .registration_classifiers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(id);
        Ok(ReadClassifierSubscription {
            state: Arc::clone(&self.runtime.state),
            id,
        })
    }
    pub fn rpc_handle(
        &mut self,
        name: &str,
        handler: ExtensionRpcRequestHandler,
    ) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        let name = name.trim();
        if name.is_empty() {
            return Err(ExtensionFailure::new(
                "RPC extension request name must not be empty",
            ));
        }
        if self.registered.rpc_handlers.contains_key(name) {
            return Err(ExtensionFailure::new(format!(
                "RPC extension request handler already registered: {name}"
            )));
        }
        self.registered.rpc_handlers.insert(name.into(), handler);
        if self
            .runtime
            .registration_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
        {
            self.runtime
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .live_rpc_handlers
                .insert(
                    self.registered.identity.path.clone(),
                    self.registered.rpc_handlers.clone(),
                );
        }
        Ok(())
    }
    pub fn register_fast_mode_action(
        &mut self,
        action: FastModeAction,
    ) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        if self.registered.fast_mode_action.is_some() {
            return Err(ExtensionFailure::new("Fast mode action already registered"));
        }
        self.registered.fast_mode_action = Some(action);
        Ok(())
    }
    pub fn rpc_emit(&self, name: &str, data: &JsonValue) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        let name = name.trim();
        if name.is_empty() {
            return Err(ExtensionFailure::new(
                "RPC extension event name must not be empty",
            ));
        }
        let event = JsonValue::Object(
            [
                (String::from("name"), JsonValue::String(name.into())),
                (String::from("data"), data.clone()),
            ]
            .into_iter()
            .collect(),
        );
        self.events.emit("senpi:extension-rpc-event", &event);
        Ok(())
    }
    pub fn register_tool(&mut self, definition: ToolDefinition) {
        if let Err(error) = self.try_register_tool(definition) {
            std::panic::panic_any(error);
        }
    }
    pub fn register_tool_with_extension_context(
        &mut self,
        definition: ToolDefinition,
        execute: ExtensionToolExecutor,
    ) -> Result<(), ExtensionFailure> {
        let name = definition.name.clone();
        self.try_register_tool(definition)?;
        let path = if self.registered.source_info.path.is_empty() {
            self.registered.identity.path.clone()
        } else {
            self.registered.source_info.path.clone()
        };
        let key = (path, name);
        let mut pending = self
            .runtime
            .registration_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pending) = pending.as_mut() {
            pending.tool_executors.insert(key, execute);
        } else {
            self.runtime
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extension_tool_executors
                .insert(key, execute);
        }
        Ok(())
    }
    pub fn register_typed_tool<
        TArgs: serde::de::DeserializeOwned + Send + 'static,
        TDetails: serde::Serialize + Send + 'static,
    >(
        &mut self,
        definition: ToolDefinition,
        execute: TypedExtensionToolExecutor<TArgs, TDetails>,
    ) -> Result<(), ExtensionFailure> {
        self.register_tool_with_extension_context(
            definition,
            Arc::new(move |id, params, signal, update, context| {
                let execute = execute.clone();
                Box::pin(async move {
                    let params = serde_json::from_value(params)
                        .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                    let update = update.map(|update| {
                        Arc::new(move |result: TypedAgentToolResult<TDetails>| {
                            update(result.into_agent_result()?);
                            Ok(())
                        }) as TypedToolUpdateCallback<TDetails>
                    });
                    execute(id, params, signal, update, context)
                        .await?
                        .into_agent_result()
                })
            }),
        )
    }
    pub fn register_tool_with_renderers<
        TState: Default + Send + 'static,
        TArgs: Clone + serde::de::DeserializeOwned + Send + 'static,
    >(
        &mut self,
        definition: ToolDefinition,
        renderers: ToolRenderers<TState, TArgs>,
    ) -> Result<(), ExtensionFailure> {
        let name = definition.name.clone();
        self.try_register_tool(definition)?;
        let renderers = Arc::new(renderers);
        self.registered.erased_tool_renderers.insert(
            name.clone(),
            erase_tool_renderers(Arc::clone(&renderers), self.cwd.clone()),
        );
        self.registered.tool_renderers.insert(name, renderers);
        self.publish_tools();
        Ok(())
    }
    pub fn register_tool_with_extension_context_and_renderers<
        TState: Default + Send + 'static,
        TArgs: Clone + serde::de::DeserializeOwned + Send + 'static,
    >(
        &mut self,
        definition: ToolDefinition,
        execute: ExtensionToolExecutor,
        renderers: ToolRenderers<TState, TArgs>,
    ) -> Result<(), ExtensionFailure> {
        let name = definition.name.clone();
        self.register_tool_with_extension_context(definition, execute)?;
        let renderers = Arc::new(renderers);
        self.registered.erased_tool_renderers.insert(
            name.clone(),
            erase_tool_renderers(Arc::clone(&renderers), self.cwd.clone()),
        );
        self.registered.tool_renderers.insert(name, renderers);
        self.publish_tools();
        Ok(())
    }
    pub fn try_register_tool(
        &mut self,
        definition: ToolDefinition,
    ) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        if definition.name == "tool_search" && self.registered.source_info.source != "builtin" {
            return Err(ExtensionFailure::new(
                "Tool name \"tool_search\" is reserved for the builtin tool-search extension.",
            ));
        }
        if !definition.parameters.is_object() {
            return Err(ExtensionFailure::new(format!(
                "Tool \"{}\" registered by extension \"{}\" must define an object parameter schema.",
                definition.name, self.registered.identity.path
            )));
        }
        let mut source_info = self.registered.source_info.clone();
        if source_info.path.is_empty() {
            source_info.path = self.registered.identity.path.clone();
        }
        let key = (source_info.path.clone(), definition.name.clone());
        let mut pending = self
            .runtime
            .registration_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pending) = pending.as_mut() {
            pending.tool_executors.remove(&key);
        } else {
            self.runtime
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extension_tool_executors
                .remove(&key);
        }
        drop(pending);
        self.registered.tool_renderers.remove(&definition.name);
        self.registered
            .erased_tool_renderers
            .remove(&definition.name);
        let tool = RegisteredTool {
            definition,
            source_info,
        };
        if let Some(existing) = self
            .registered
            .tools
            .iter_mut()
            .find(|t| t.definition.name == tool.definition.name)
        {
            *existing = tool.clone();
        } else {
            self.registered.tools.push(tool.clone());
        }
        let actions = self
            .runtime
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session_actions
            .clone();
        if let Some(actions) = actions {
            actions.install_registered_tool(tool)?;
        }
        self.publish_tools();
        Ok(())
    }
    fn publish_tools(&self) {
        let mut pending = self
            .runtime
            .registration_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pending) = pending.as_mut() {
            pending.tools.insert(
                self.registered.identity.path.clone(),
                self.registered.tools.clone(),
            );
            pending.tool_renderers.insert(
                self.registered.identity.path.clone(),
                self.registered.tool_renderers.clone(),
            );
            pending.erased_tool_renderers.insert(
                self.registered.identity.path.clone(),
                self.registered.erased_tool_renderers.clone(),
            );
        } else {
            let mut state = self
                .runtime
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.live_tools.insert(
                self.registered.identity.path.clone(),
                self.registered.tools.clone(),
            );
            state.live_tool_renderers.insert(
                self.registered.identity.path.clone(),
                self.registered.tool_renderers.clone(),
            );
            state.live_erased_tool_renderers.insert(
                self.registered.identity.path.clone(),
                self.registered.erased_tool_renderers.clone(),
            );
            state.tool_generation += 1;
        }
    }
    pub fn register_command(
        &mut self,
        name: &str,
        description: Option<String>,
        argument_hint: Option<String>,
        handler: CommandHandler,
    ) {
        self.runtime.assert_active_or_panic();
        self.registered.command_context_handlers.remove(name);
        self.registered.command_argument_completions.remove(name);
        let command = RegisteredCommand {
            name: name.into(),
            source_info: self.registered.source_info.clone(),
            description,
            argument_hint,
            handler,
        };
        if let Some(existing) = self.registered.commands.iter_mut().find(|c| c.name == name) {
            *existing = command;
        } else {
            self.registered.commands.push(command);
        }
        self.publish_commands();
    }
    fn publish_commands(&self) {
        if self
            .runtime
            .registration_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
        {
            let mut state = self
                .runtime
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.live_commands.insert(
                self.registered.identity.path.clone(),
                (
                    self.registered.commands.clone(),
                    self.registered.command_context_handlers.clone(),
                ),
            );
            state.live_command_argument_completions.insert(
                self.registered.identity.path.clone(),
                self.registered.command_argument_completions.clone(),
            );
        }
    }
    pub fn register_command_with_completions(
        &mut self,
        name: &str,
        description: Option<String>,
        argument_hint: Option<String>,
        handler: CommandHandler,
        completions: CommandArgumentCompletions,
    ) {
        self.register_command(name, description, argument_hint, handler);
        self.registered
            .command_argument_completions
            .insert(name.into(), completions);
        self.publish_commands();
    }
    /// Registers a command carrying BOTH a command-context handler and argument completions in one
    /// call: `register_command_with_context` and `register_command_with_completions` each clear the
    /// other half via `register_command`, so this installs both without clearing either.
    pub fn register_command_with_context_and_completions(
        &mut self,
        name: &str,
        description: Option<String>,
        argument_hint: Option<String>,
        handler: CommandContextHandler,
        completions: CommandArgumentCompletions,
    ) {
        self.register_command(
            name,
            description,
            argument_hint,
            Arc::new(|_, _| {
                Box::pin(async {
                    Err(ExtensionFailure::new(
                        "Command requires a command-capable context",
                    ))
                })
            }),
        );
        self.registered
            .command_context_handlers
            .insert(name.into(), handler);
        self.registered
            .command_argument_completions
            .insert(name.into(), completions);
        self.publish_commands();
    }
    pub fn register_command_with_context(
        &mut self,
        name: &str,
        description: Option<String>,
        argument_hint: Option<String>,
        handler: CommandContextHandler,
    ) {
        self.register_command(
            name,
            description,
            argument_hint,
            Arc::new(|_, _| {
                Box::pin(async {
                    Err(ExtensionFailure::new(
                        "Command requires a command-capable context",
                    ))
                })
            }),
        );
        self.registered
            .command_context_handlers
            .insert(name.into(), handler);
        self.publish_commands();
    }
    pub fn register_config_watch(&mut self, registration: RegisteredConfigWatch) {
        self.runtime.assert_active_or_panic();
        if let Some(existing) = self
            .registered
            .config_watch_registrations
            .iter_mut()
            .find(|existing| existing.id == registration.id)
        {
            *existing = registration.clone();
        } else {
            self.registered
                .config_watch_registrations
                .push(registration.clone());
        }
        self.events
            .publish_config_watch(&self.registered.identity.path, registration);
    }
    pub fn on_config_watch_registration(
        &self,
        listener: ConfigWatchRegistrationListener,
    ) -> BusSubscription {
        self.events.on_native::<(String, RegisteredConfigWatch)>(
            CONFIG_WATCH_REGISTER_CHANNEL,
            Arc::new(move |(path, registration)| listener(path, registration)),
        )
    }
    pub fn register_flag(&mut self, name: &str, kind: FlagType, description: Option<String>) {
        self.runtime.assert_active_or_panic();
        let default = match &kind {
            FlagType::Boolean { default } => default.map(FlagValue::Boolean),
            FlagType::String { default } => default.clone().map(FlagValue::String),
        };
        if let Some(value) = default {
            self.runtime.register_flag_default(name, value);
        }
        let flag = ExtensionFlag {
            name: name.into(),
            description,
            kind,
            extension_path: self.registered.identity.path.clone(),
        };
        if let Some(existing) = self.registered.flags.iter_mut().find(|f| f.name == name) {
            *existing = flag;
        } else {
            self.registered.flags.push(flag);
        }
        if self
            .runtime
            .registration_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
        {
            self.runtime
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .live_flags
                .insert(
                    self.registered.identity.path.clone(),
                    self.registered.flags.clone(),
                );
        }
    }
    pub fn get_flag(&self, name: &str) -> Option<FlagValue> {
        self.runtime.assert_active_or_panic();
        if self.registered.flags.iter().any(|flag| flag.name == name) {
            self.runtime.get_flag(name)
        } else {
            None
        }
    }
    pub fn set_flag(&self, name: &str, value: FlagValue) {
        self.runtime.set_flag(name, value);
    }
    pub fn register_message_renderer(&mut self, custom_type: &str, renderer: MessageRenderer) {
        self.runtime.assert_active_or_panic();
        self.registered
            .message_renderers
            .insert(custom_type.into(), renderer);
        if self
            .runtime
            .registration_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
        {
            self.runtime
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .live_message_renderers
                .insert(
                    self.registered.identity.path.clone(),
                    self.registered.message_renderers.clone(),
                );
        }
    }
    pub fn register_entry_renderer(
        &mut self,
        custom_type: &str,
        renderer: EntryRenderer,
        options: EntryRendererOptions,
    ) {
        self.register_entry_renderer_optional(custom_type, renderer, Some(options));
    }
    pub fn register_entry_renderer_optional(
        &mut self,
        custom_type: &str,
        renderer: EntryRenderer,
        options: Option<EntryRendererOptions>,
    ) {
        self.runtime.assert_active_or_panic();
        self.registered
            .entry_renderers
            .insert(custom_type.into(), renderer);
        if let Some(options) = options {
            self.registered
                .entry_renderer_options
                .insert(custom_type.into(), options);
        } else {
            self.registered.entry_renderer_options.remove(custom_type);
        }
        if self
            .runtime
            .registration_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
        {
            let renderers = self
                .registered
                .entry_renderers
                .iter()
                .map(|(name, renderer)| {
                    (
                        name.clone(),
                        (
                            renderer.clone(),
                            self.registered.entry_renderer_options.get(name).cloned(),
                        ),
                    )
                })
                .collect();
            self.runtime
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .live_entry_renderers
                .insert(self.registered.identity.path.clone(), renderers);
        }
    }
    pub fn register_mcp_server(&mut self, name: &str, config: McpServerDeclaration) {
        if let Err(error) = self.try_register_mcp_server(name, config) {
            std::panic::panic_any(error);
        }
    }
    pub fn try_register_mcp_server(
        &mut self,
        name: &str,
        config: McpServerDeclaration,
    ) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        for (field, value) in [
            ("idleTimeoutMin", config.idle_timeout_min),
            ("requestTimeoutMs", config.request_timeout_ms),
            ("connectTimeoutMs", config.connect_timeout_ms),
            ("startupTimeoutMs", config.startup_timeout_ms),
        ] {
            if value.is_some_and(|value| !value.is_finite()) {
                return Err(ExtensionFailure::new(format!(
                    "Invalid MCP server declaration \"{name}\": {field}: must be number"
                )));
            }
        }
        if config.enabled != Some(false) {
            let transport = config.transport.unwrap_or_else(|| {
                if config.url.as_ref().is_some_and(|url| !url.is_empty()) {
                    McpTransport::Http
                } else {
                    McpTransport::Stdio
                }
            });
            let (field, endpoint, kind) = match transport {
                McpTransport::Stdio => ("command", config.command.as_deref(), "stdio"),
                McpTransport::Http => ("url", config.url.as_deref(), "http"),
            };
            if endpoint.is_none_or(|endpoint| endpoint.trim().is_empty()) {
                return Err(ExtensionFailure::new(format!(
                    "Invalid MCP server declaration \"{name}\": mcpServers.{name}.{field}: Required for enabled {kind} server"
                )));
            }
        }
        let declaration = RegisteredMcpServerDeclaration {
            name: name.into(),
            config,
            extension_path: self.registered.identity.path.clone(),
            registration_cwd: self.cwd.clone(),
        };
        if let Some(existing) = self
            .registered
            .mcp_servers
            .iter_mut()
            .find(|s| s.name == name)
        {
            *existing = declaration;
        } else {
            self.registered.mcp_servers.push(declaration);
        }
        if self
            .runtime
            .registration_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
        {
            self.runtime
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .live_mcp_servers
                .insert(
                    self.registered.identity.path.clone(),
                    self.registered.mcp_servers.clone(),
                );
        }
        Ok(())
    }
    pub fn register_removed_tool_hint(&mut self, name: &str, hint: &str) {
        self.runtime.assert_active_or_panic();
        let actions = self
            .runtime
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session_actions
            .clone();
        if let Some(actions) = actions
            && let Err(error) = actions.register_removed_tool_hint(name, hint)
        {
            std::panic::panic_any(error);
        }
        self.registered
            .removed_tool_hints
            .insert(name.into(), hint.into());
    }
    pub fn register_filesystem_policy(&mut self, policy: FilesystemPolicy) {
        self.runtime.assert_active_or_panic();
        self.registered.filesystem_policies.push(policy);
        if self
            .runtime
            .registration_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
        {
            self.runtime
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .live_filesystem_policies
                .insert(
                    self.registered.identity.path.clone(),
                    self.registered.filesystem_policies.clone(),
                );
        }
    }
    pub fn lifecycle(&self) -> crate::LazyLifecycleApi {
        let runtime = self.runtime.clone();
        crate::create_lifecycle_api(move || runtime.actions()?.lifecycle())
    }
    pub async fn send_itemized_message(
        &self,
        message: CustomMessage,
        options: SendMessageOptions,
        items: Vec<SendMessageItem>,
    ) -> Result<Option<Vec<SendMessageReceipt>>, ExtensionFailure> {
        self.runtime
            .actions()?
            .send_itemized_message(message, options, items)
            .await
    }
    pub fn arm_tool_barrier(&self, spec: JsonValue) -> Result<(), ExtensionFailure> {
        self.runtime.actions()?.arm_tool_barrier(spec)
    }
    pub fn release_tool_barrier(&self, id: &str) -> Result<bool, ExtensionFailure> {
        self.runtime.actions()?.release_tool_barrier(id)
    }
    pub fn relocation_offered(&self) -> Result<bool, ExtensionFailure> {
        self.runtime.actions()?.relocation_offered()
    }
    pub async fn relocation_targets(&self) -> Result<JsonValue, ExtensionFailure> {
        self.runtime.actions()?.relocation_targets().await
    }
    pub async fn request_relocation(
        &self,
        request: JsonValue,
    ) -> Result<JsonValue, ExtensionFailure> {
        self.runtime.actions()?.request_relocation(request).await
    }
    pub fn send_message(
        &self,
        message: CustomMessage,
        options: SendMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        self.runtime.actions()?.send_message(message, options)
    }
    pub fn send_message_with_receipt(
        &self,
        message: CustomMessage,
        options: SendMessageOptions,
    ) -> Result<MessageDeliveryReceipt, ExtensionFailure> {
        self.runtime
            .actions()?
            .send_message_with_receipt(message, options)
    }
    pub fn send_user_message(
        &self,
        content: UserMessageContent,
        options: SendUserMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        let origin = self.runtime.take_input_origin();
        self.runtime
            .actions()?
            .send_user_message_with_origin(content, options, origin)
    }
    /// Registers a file-backed child module under `registration.id`.
    ///
    /// # Errors
    /// Fails for a stale registration or an invalid registration.
    pub fn register_child_module(
        &mut self,
        registration: ChildModuleRegistration,
    ) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        validate_child_module_registration(&registration)?;
        let id = registration.id.clone();
        self.registered.child_modules.insert(
            id,
            RegisteredChildModule {
                registration,
                source_info: self.registered.source_info.clone(),
            },
        );
        Ok(())
    }
    /// Redacts `text` for upload with the host's redactor; `None` when the host has none.
    ///
    /// # Errors
    /// Fails for a stale registration or when actions are not bound.
    pub fn redact_for_upload(
        &self,
        text: &str,
        extra_literals: &[String],
    ) -> Result<Option<UploadRedaction>, ExtensionFailure> {
        Ok(self
            .runtime
            .actions()?
            .redact_for_upload(text, extra_literals))
    }
    pub fn host_version(&self) -> Result<Option<String>, ExtensionFailure> {
        Ok(self.runtime.actions()?.host_version())
    }
    pub fn get_subscription_limits(&self) -> Result<Vec<AccountLimits>, ExtensionFailure> {
        Ok(self.runtime.actions()?.get_subscription_limits())
    }
    pub fn on_subscription_limits(
        &self,
        listener: SubscriptionLimitsListener,
    ) -> Result<Option<UiUnsubscribe>, ExtensionFailure> {
        Ok(self.runtime.actions()?.on_subscription_limits(listener))
    }
    pub fn append_entry(
        &self,
        custom_type: &str,
        data: Option<JsonValue>,
    ) -> Result<(), ExtensionFailure> {
        self.runtime.actions()?.append_entry(custom_type, data)
    }
    pub fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
        self.runtime.actions()?.get_all_tools()
    }
    pub fn set_session_name(&self, name: &str) -> Result<(), ExtensionFailure> {
        self.runtime.session_actions()?.set_session_name(name)
    }
    pub fn get_session_name(&self) -> Result<Option<String>, ExtensionFailure> {
        self.runtime.session_actions()?.get_session_name()
    }
    pub fn set_label(&self, entry_id: &str, label: Option<&str>) -> Result<(), ExtensionFailure> {
        self.runtime.session_actions()?.set_label(entry_id, label)
    }
    pub fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> {
        self.runtime.session_actions()?.get_active_tools()
    }
    pub fn set_active_tools(&self, names: Vec<String>) -> Result<(), ExtensionFailure> {
        self.runtime.session_actions()?.set_active_tools(names)
    }
    pub fn refresh_tools(&self) -> Result<(), ExtensionFailure> {
        self.runtime.session_actions()?.refresh_tools()
    }
    pub fn get_commands(&self) -> Result<Vec<SlashCommandInfo>, ExtensionFailure> {
        self.runtime.session_actions()?.get_commands()
    }
    pub async fn set_model(&self, model: Model) -> Result<bool, ExtensionFailure> {
        let result = self.runtime.session_actions()?.set_model(model).await?;
        self.runtime.assert_active()?;
        Ok(result)
    }
    pub fn get_thinking_level(&self) -> Result<ThinkingLevel, ExtensionFailure> {
        self.runtime.session_actions()?.get_thinking_level()
    }
    pub fn set_thinking_level(&self, level: ThinkingLevel) -> Result<(), ExtensionFailure> {
        self.runtime.session_actions()?.set_thinking_level(level)
    }
    pub async fn set_session_model(&self, model: Model) -> Result<bool, ExtensionFailure> {
        let result = self
            .runtime
            .session_actions()?
            .set_session_model(model)
            .await?;
        self.runtime.assert_active()?;
        Ok(result)
    }
    pub fn set_session_thinking_level(&self, level: ThinkingLevel) -> Result<(), ExtensionFailure> {
        self.runtime
            .session_actions()?
            .set_session_thinking_level(level)
    }
    pub fn get_model_thinking_level(&self) -> Result<ModelThinkingLevel, ExtensionFailure> {
        self.runtime.session_actions()?.get_model_thinking_level()
    }
    pub fn set_model_thinking_level(
        &self,
        level: ModelThinkingLevel,
    ) -> Result<(), ExtensionFailure> {
        self.runtime
            .session_actions()?
            .set_model_thinking_level(level)
    }
    pub fn set_session_model_thinking_level(
        &self,
        level: ModelThinkingLevel,
    ) -> Result<(), ExtensionFailure> {
        self.runtime
            .session_actions()?
            .set_session_model_thinking_level(level)
    }
    pub fn set_session_fast_mode(&self, enabled: bool) -> Result<(), ExtensionFailure> {
        self.runtime
            .session_actions()?
            .set_session_fast_mode(enabled)
    }
    pub async fn execute_tool(
        &self,
        name: &str,
        params: JsonValue,
        options: ExecuteToolOptions,
    ) -> Result<maho_agent::types::AgentToolResult, ExecuteToolError> {
        let actions = self
            .runtime
            .session_actions()
            .map_err(|error| ExecuteToolError {
                code: ExecuteToolErrorCode::Blocked,
                tool_name: name.into(),
                message: error.message,
                active_tools: Vec::new(),
            })?;
        let result = actions.execute_tool(name, params, options).await?;
        self.runtime
            .assert_active()
            .map_err(|error| ExecuteToolError {
                code: ExecuteToolErrorCode::Blocked,
                tool_name: name.into(),
                message: error.message,
                active_tools: Vec::new(),
            })?;
        Ok(result)
    }
    pub async fn exec(
        &self,
        command: &str,
        args: &[String],
        options: ExecOptions,
    ) -> Result<ExecResult, ExtensionFailure> {
        let result = self
            .runtime
            .session_actions()?
            .exec(command, args, &self.cwd, options)
            .await?;
        self.runtime.assert_active()?;
        Ok(result)
    }
}

pub const RUNTIME_EXTENSION_PATH: &str = "<runtime>";
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionError {
    pub extension_path: String,
    pub event: String,
    pub error: String,
    pub stack: Option<String>,
}

/// Additional session-owned events from agent-session.ts:312-494. Agent lifecycle events
/// retain the real agent union; open records stay data, with no dependency on maho-core.
#[derive(Clone, Debug)]
pub enum AgentSessionEvent {
    Agent(AgentEvent),
    AgentEnd {
        messages: Vec<AgentMessage>,
        aborted: bool,
        will_retry: bool,
        abort_source: Option<AbortSource>,
    },
    MessageUpdate {
        message: AgentMessage,
        assistant_message_event: JsonValue,
    },
    AgentSettled,
    AgentIdle,
    SessionAbort,
    ResumeCompactionRequired {
        projection: JsonValue,
        notice: String,
    },
    ResumeContextReduced {
        tokens_before: u64,
        tokens_after: u64,
        dropped_entries: usize,
        notice: String,
    },
    ContinuationError {
        error_message: String,
    },
    SkillInvocation {
        skills: Vec<SkillInvocation>,
    },
    CommandInvocation {
        command: JsonValue,
    },
    QueueUpdate {
        steering: Vec<String>,
        follow_up: Vec<String>,
        ordered: Vec<QueuedInput>,
    },
    CompactionStart {
        reason: CompactionReason,
        request_id: Option<String>,
    },
    CompactionProgress {
        reason: CompactionReason,
        delta: Option<String>,
        text: Option<String>,
    },
    EntryAppended {
        entry: SessionEntry,
    },
    SessionInfoChanged {
        name: Option<String>,
    },
    ToolHookStatus(ToolHookLifecycleEvent),
    SystemPromptChange {
        system_prompt: String,
        previous_system_prompt: String,
        system_prompt_name: Option<String>,
        model: Model,
        previous_model: Option<Model>,
    },
    ThinkingLevelChanged {
        level: ThinkingLevel,
        thinking_selection: Option<maho_ai::types::ThinkingSelection>,
    },
    HighReasoningWarning {
        model_id: String,
        provider: String,
        thinking_level: ThinkingLevel,
    },
    SettingsSourceSelected {
        selection: JsonValue,
    },
    ModelChanged {
        model: Model,
        thinking_level: ThinkingLevel,
        source: ModelSelectSource,
    },
    ModelChangeRejected {
        model: Model,
        reason: String,
        detail: String,
        budget: Option<ModelBudget>,
    },
    ModelChangeSkipped {
        model: Model,
        budget: ModelBudget,
        direction: String,
    },
    ModelChangePending {
        model: Model,
        budget: ModelBudget,
        notice: String,
    },
    ServiceTierChanged {
        tier: Option<ServiceTier>,
        fast_mode: bool,
    },
    SessionSettingsChanged {
        steering_mode: String,
        follow_up_mode: String,
        auto_compaction_enabled: bool,
    },
    CompactionEnd {
        reason: CompactionReason,
        result: Option<CompactionResult>,
        aborted: bool,
        will_retry: bool,
        request_id: Option<String>,
        accepted: Option<bool>,
        rejection_cause: Option<CompactionRejectionCause>,
        error_message: Option<String>,
    },
    AutoRetryStart {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        error_message: String,
    },
    AutoRetryEnd {
        success: bool,
        attempt: u32,
        final_error: Option<String>,
    },
    RetryFallbackApplied {
        from: String,
        to: String,
        chain_key: String,
        reason: String,
    },
    RetryFallbackSucceeded {
        model: String,
        chain_key: String,
    },
    RetryFallbackReverted {
        from: String,
        to: String,
    },
    RetryFallbackExhausted {
        chain_key: String,
        last_error: String,
    },
    ServerFallbackAborted {
        from: String,
        to: String,
        chain_configured: bool,
    },
    AuthLoginUrl {
        provider: String,
        url: String,
    },
    AuthLoginEnd {
        provider: String,
        success: bool,
        error: Option<String>,
    },
    SummarizationRetryScheduled {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        error_message: String,
    },
    SummarizationRetryAttemptStart {
        source: String,
        reason: Option<CompactionReason>,
    },
    SummarizationRetryFinished,
    RetryProbeScheduled {
        selector: String,
        at_ms: u64,
        probe_index: u8,
    },
    RetryProbeResult {
        selector: String,
        ok: bool,
        error_message: Option<String>,
    },
    /// senpi `session_tool_child`: `state` is "spawned" | "exited" | "respawned"; `pid` is -1 when the child has none.
    SessionToolChild {
        state: String,
        generation: u64,
        pid: i64,
    },
    /// senpi `offload_module_stuck` (`StuckModuleNotice`).
    OffloadModuleStuck {
        module_path: String,
        export: String,
        deadline_ms: u64,
        files: Vec<String>,
        notice: String,
    },
    BashExecutionUpdate {
        id: Option<String>,
        delta: String,
    },
    SessionPaused {
        pause_seq: u64,
        kind: String,
        at: String,
        notice_count: usize,
        persisted: bool,
    },
    SessionResumed {
        pause_seq: u64,
        cause: String,
        at: String,
    },
    SessionNotice {
        notice_ids: Vec<String>,
        notice_count: usize,
        disposition: String,
    },
}
#[derive(Clone, Debug)]
pub struct SkillInvocation {
    pub name: String,
    pub path: String,
    pub syntax: String,
}
#[derive(Clone, Debug)]
pub struct QueuedInput {
    pub text: String,
    pub mode: StreamingBehavior,
    pub enqueue_order: u64,
}
#[derive(Clone, Debug)]
pub struct ModelBudget {
    pub context_window: u64,
    pub live_context_tokens: u64,
    pub required_tokens: u64,
    pub shortfall_tokens: u64,
    pub safety_margin_profile: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolHookName {
    PreToolUse,
    PostToolUse,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolHookStatus {
    Completed,
    Blocked,
    Failed,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolHookPhase {
    Start,
    Update,
    End {
        completed_at: u64,
        status: ToolHookStatus,
        error_message: Option<String>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolHookLifecycleEvent {
    pub hook_run_id: String,
    pub hook_name: ToolHookName,
    pub tool_name: String,
    pub tool_call_id: String,
    pub extension_path: String,
    pub status_message: String,
    pub started_at: u64,
    pub phase: ToolHookPhase,
}

#[cfg(test)]
mod memory_reporter_tests {
    use super::*;

    fn api(runtime: ExtensionRuntime) -> ExtensionApi {
        ExtensionApi::new(
            LoadedExtension::new("<memory-test>", PathBuf::new(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            runtime,
        )
    }

    #[test]
    fn reporter_registration_is_lazy_and_committed_before_publication() {
        // Given
        let runtime = ExtensionRuntime::default();
        let api = api(runtime.registration_scope());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        api.register_memory_reporter(
            "cache",
            Arc::new(move || {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                [("bytes".into(), 42)].into()
            }),
        )
        .expect("register");
        assert!(runtime.memory_reporters("<memory-test>").is_empty());
        // When
        api.runtime.commit_registration().expect("commit");
        // Then
        let reporters = runtime.memory_reporters("<memory-test>");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(reporters.len(), 1);
        assert_eq!(reporters[0].name, "cache");
        assert_eq!((reporters[0].reporter)()["bytes"], 42);
    }

    #[test]
    fn failed_factory_reporters_never_publish() {
        // Given
        let runtime = ExtensionRuntime::default();
        let api = api(runtime.registration_scope());
        api.register_memory_reporter("cache", Arc::new(BTreeMap::new))
            .expect("register");
        // When
        api.runtime.invalidate_registration("factory failed");
        // Then
        assert!(runtime.memory_reporters("<memory-test>").is_empty());
        assert!(api.runtime.commit_registration().is_err());
        assert!(
            api.register_memory_reporter("late", Arc::new(BTreeMap::new))
                .is_err()
        );
    }

    #[test]
    fn reserved_names_cannot_be_registered() {
        // Given
        let runtime = ExtensionRuntime::default();
        let api = api(runtime.clone());
        // When
        let rejected: Vec<_> = RESERVED_MEMORY_REPORT_KEYS
            .iter()
            .map(|name| {
                api.register_memory_reporter(name, Arc::new(BTreeMap::new))
                    .is_err()
            })
            .collect();
        // Then
        assert!(rejected.into_iter().all(|rejected| rejected));
        assert!(runtime.memory_reporters("<memory-test>").is_empty());
    }

    #[test]
    fn late_registration_replaces_same_extension_reporter_without_holding_read_lock() {
        // Given
        let runtime = ExtensionRuntime::default();
        let api = api(runtime.registration_scope());
        api.register_memory_reporter("cache", Arc::new(|| [("bytes".into(), 99)].into()))
            .expect("register");
        api.runtime.commit_registration().expect("commit");
        let reentrant = runtime.clone();
        // When
        api.register_memory_reporter(
            "cache",
            Arc::new(move || {
                let count = reentrant.memory_reporters("<memory-test>").len();
                [("bytes".into(), count)].into()
            }),
        )
        .expect("replace");
        // Then
        let reporters = runtime.memory_reporters("<memory-test>");
        assert_eq!(reporters.len(), 1);
        assert_eq!((reporters[0].reporter)()["bytes"], 1);
    }

    #[test]
    fn invalidation_releases_reporter_captures() {
        // Given
        let runtime = ExtensionRuntime::default();
        let api = api(runtime.clone());
        let captured = Arc::new(());
        let weak = Arc::downgrade(&captured);
        api.register_memory_reporter(
            "cache",
            Arc::new(move || {
                let _capture = &captured;
                BTreeMap::new()
            }),
        )
        .expect("register");
        // When
        runtime.invalidate("reloaded");
        // Then
        assert!(weak.upgrade().is_none());
        assert!(runtime.memory_reporters("<memory-test>").is_empty());
    }
}
