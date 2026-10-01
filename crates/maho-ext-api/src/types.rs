//! Rust mirror of senpi core/extensions/types.ts at fe8c564b.
//! Session, resource, registry and theme ports live here to keep the dependency graph acyclic.
use std::{collections::BTreeMap, fmt, future::Future, path::{Path, PathBuf}, pin::Pin, sync::{Arc, Mutex}};
pub use maho_agent::types::{AgentEvent, AgentMessage};
pub use maho_ai::{model::Model, types::{JsonValue, ThinkingLevel, Usage, ImageContent}};
pub use maho_tools::{ToolContext, ToolDefinition, FilesystemPolicy, FilesystemPolicyChecker, FilesystemPolicyDecision, FilesystemPolicyRequest};
pub use maho_tools::definition::{AbortSignal, ToolContent, ToolResult, ToolSessionManager, ToolExposure, ToolExecutionMode};
pub use maho_tools::filesystem_policy::FilesystemOperation;
pub use maho_tui::tui::Component;

pub type ExtensionFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ExtensionFailure>> + Send + 'a>>;
pub type UiFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type ToolHookStatusUpdater = Arc<dyn Fn(&str) + Send + Sync>;

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

/// Host implementations adapt their owning registry, without an ext-api -> core edge.
pub trait ModelRegistry: Send + Sync {
    fn get_all(&self) -> Vec<Model>;
    fn get_available(&self) -> Vec<Model>;
    fn find(&self, provider: &str, id: &str) -> Option<Model>;
    fn has_configured_auth(&self, model: &Model) -> bool;
    fn get_api_key_for_provider<'a>(&'a self, provider: &'a str) -> ExtensionFuture<'a, Option<String>>;
}
#[derive(Clone, Debug, PartialEq)]
pub struct SessionEntry { pub id: String, pub parent_id: Option<String>, pub timestamp: String, pub kind: String, pub data: JsonValue }
pub trait SessionManager: ToolSessionManager {
    fn get_entries(&self) -> Vec<SessionEntry>;
    fn get_branch(&self) -> Vec<SessionEntry>;
    fn get_leaf_id(&self) -> Option<String>;
    fn get_session_name(&self) -> Option<String>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WidgetPlacement { #[default] AboveEditor, BelowEditor }
impl WidgetPlacement { pub const fn as_str(self) -> &'static str { match self { Self::AboveEditor => "aboveEditor", Self::BelowEditor => "belowEditor" } } }
#[derive(Clone, Debug, Default)]
pub struct ExtensionUiDialogOptions { pub signal: Option<AbortSignal>, pub timeout_ms: Option<u64> }
#[derive(Clone, Debug, Default)]
pub struct ExtensionWidgetOptions { pub placement: WidgetPlacement }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationType { Info, Warning, Error }
/// Components retain senpi's render/invalidate/input contract; creation occurs on the UI thread.
pub type ComponentFactory = Arc<dyn Fn(&Theme) -> Box<dyn Component> + Send + Sync>;
#[derive(Clone)]
pub enum WidgetContent { Lines(Vec<String>), Component(ComponentFactory) }
#[derive(Clone, Debug, Default)]
pub struct CustomUiOptions { pub overlay: bool, pub overlay_options: Option<JsonValue> }
pub trait ExtensionUi: Send + Sync {
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
    pub fn is_idle(&self) -> bool { (self.is_idle_fn)() }
    pub async fn wait_for_idle(&self) { (self.wait_for_idle_fn)().await; }
    pub fn is_project_trusted(&self) -> bool { (self.is_project_trusted_fn)() }
    pub fn is_compacting(&self) -> bool { (self.is_compacting_fn)() }
    pub fn get_system_prompt(&self) -> String { (self.get_system_prompt_fn)() }
    pub fn get_system_prompt_options(&self) -> BuildSystemPromptOptions { (self.get_system_prompt_options_fn)() }
    pub fn get_registered_mcp_servers(&self) -> &[RegisteredMcpServerDeclaration] { &self.registered_mcp_servers }
}
impl ToolContext for ExtensionContext {
    fn cwd(&self) -> &Path { &self.cwd }
    fn model(&self) -> Option<&Model> { self.model.as_ref() }
    fn thinking_level(&self) -> Option<ThinkingLevel> { self.thinking_level }
    fn session_manager(&self) -> &dyn ToolSessionManager { self.session_manager.as_ref() }
    fn goal_store_file(&self) -> Option<&Path> { self.goal_store_file.as_deref() }
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactionSettings { pub enabled: bool, pub reserve_tokens: u64, pub keep_recent_tokens: u64 }
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

pub type BusHandler = Arc<dyn Fn(&JsonValue) + Send + Sync>;
#[derive(Default)]
struct BusState { next_id: u64, handlers: BTreeMap<String, Vec<(u64, BusHandler)>> }
#[derive(Clone, Default)]
pub struct EventBus { state: Arc<Mutex<BusState>> }
pub struct BusSubscription { state: Arc<Mutex<BusState>>, channel: String, id: u64 }
impl Drop for BusSubscription {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(handlers) = state.handlers.get_mut(&self.channel) { handlers.retain(|(id, _)| *id != self.id); }
    }
}
impl EventBus {
    pub fn on(&self, channel: &str, handler: BusHandler) -> BusSubscription {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = state.next_id; state.next_id = state.next_id.wrapping_add(1);
        state.handlers.entry(channel.into()).or_default().push((id, handler));
        BusSubscription { state: Arc::clone(&self.state), channel: channel.into(), id }
    }
    pub fn emit(&self, channel: &str, data: &JsonValue) {
        let handlers = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).handlers.get(channel).cloned().unwrap_or_default();
        for (_, handler) in handlers { handler(data); }
    }
}

/// Native factories are passed by the CLI as an explicit Vec<Box<dyn Extension>>.
pub trait Extension: Send + Sync { fn register(&self, api: &mut ExtensionApi); }
#[derive(Clone)]
pub struct LoadedExtension {
    pub identity: ExtensionIdentity, pub source_info: SourceInfo, pub registration_cwd: PathBuf,
    pub handlers: BTreeMap<EventKind, Vec<ExtensionHandler>>, pub tools: Vec<RegisteredTool>,
    pub commands: Vec<RegisteredCommand>, pub flags: Vec<ExtensionFlag>, pub message_renderers: BTreeMap<String, MessageRenderer>,
    pub entry_renderers: BTreeMap<String, EntryRenderer>, pub entry_renderer_options: BTreeMap<String, EntryRendererOptions>,
    pub mcp_servers: Vec<RegisteredMcpServerDeclaration>, pub removed_tool_hints: BTreeMap<String, String>, pub filesystem_policies: Vec<FilesystemPolicy>,
}
impl LoadedExtension {
    pub fn new(path: &str, cwd: PathBuf, source_info: SourceInfo) -> Self {
        Self { identity: ExtensionIdentity { path: path.into(), resolved_path: path.into() }, source_info, registration_cwd: cwd,
            handlers: BTreeMap::new(), tools: Vec::new(), commands: Vec::new(), flags: Vec::new(), message_renderers: BTreeMap::new(),
            entry_renderers: BTreeMap::new(), entry_renderer_options: BTreeMap::new(), mcp_servers: Vec::new(), removed_tool_hints: BTreeMap::new(), filesystem_policies: Vec::new() }
    }
}
#[derive(Default)]
struct RuntimeState { flags: BTreeMap<String, FlagValue>, actions: Option<Arc<dyn ExtensionActions>>, stale: Option<String> }
#[derive(Clone, Default)]
pub struct ExtensionRuntime { state: Arc<Mutex<RuntimeState>> }
impl ExtensionRuntime {
    pub fn assert_active(&self) -> Result<(), ExtensionFailure> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(message) = &state.stale { return Err(ExtensionFailure::new(message.clone())); } Ok(())
    }
    pub fn invalidate(&self, message: &str) { self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).stale.get_or_insert_with(|| message.into()); }
    pub fn bind(&self, actions: Arc<dyn ExtensionActions>) { self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).actions = Some(actions); }
    fn actions(&self) -> Result<Arc<dyn ExtensionActions>, ExtensionFailure> {
        self.assert_active()?;
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).actions.clone().ok_or_else(|| ExtensionFailure::new("Extension actions are unavailable during registration"))
    }
    pub fn get_flag(&self, name: &str) -> Option<FlagValue> { self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).flags.get(name).cloned() }
    pub fn set_flag(&self, name: &str, value: FlagValue) { self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).flags.insert(name.into(), value); }
}
pub struct ExtensionApi {
    pub cwd: PathBuf, pub profile: ExtensionSessionProfile, pub events: EventBus, pub runtime: ExtensionRuntime, pub registered: LoadedExtension,
}
impl ExtensionApi {
    pub fn new(registered: LoadedExtension, profile: ExtensionSessionProfile, events: EventBus, runtime: ExtensionRuntime) -> Self {
        Self { cwd: registered.registration_cwd.clone(), profile, events, runtime, registered }
    }
    pub fn on(&mut self, event: EventKind, handler: ExtensionHandler) { self.registered.handlers.entry(event).or_default().push(handler); }
    pub fn register_tool(&mut self, definition: ToolDefinition) {
        let tool = RegisteredTool { definition, source_info: self.registered.source_info.clone() };
        if let Some(existing) = self.registered.tools.iter_mut().find(|t| t.definition.name == tool.definition.name) { *existing = tool; } else { self.registered.tools.push(tool); }
    }
    pub fn register_command(&mut self, name: &str, description: Option<String>, argument_hint: Option<String>, handler: CommandHandler) {
        let command = RegisteredCommand { name: name.into(), source_info: self.registered.source_info.clone(), description, argument_hint, handler };
        if let Some(existing) = self.registered.commands.iter_mut().find(|c| c.name == name) { *existing = command; } else { self.registered.commands.push(command); }
    }
    pub fn register_flag(&mut self, name: &str, kind: FlagType, description: Option<String>) {
        let default = match &kind { FlagType::Boolean { default } => default.map(FlagValue::Boolean), FlagType::String { default } => default.clone().map(FlagValue::String) };
        if let Some(value) = default { let mut state = self.runtime.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner); state.flags.entry(name.into()).or_insert(value); }
        let flag = ExtensionFlag { name: name.into(), description, kind, extension_path: self.registered.identity.path.clone() };
        if let Some(existing) = self.registered.flags.iter_mut().find(|f| f.name == name) { *existing = flag; } else { self.registered.flags.push(flag); }
    }
    pub fn get_flag(&self, name: &str) -> Option<FlagValue> { self.runtime.get_flag(name) }
    pub fn set_flag(&self, name: &str, value: FlagValue) { self.runtime.set_flag(name, value); }
    pub fn register_message_renderer(&mut self, custom_type: &str, renderer: MessageRenderer) { self.registered.message_renderers.insert(custom_type.into(), renderer); }
    pub fn register_entry_renderer(&mut self, custom_type: &str, renderer: EntryRenderer, options: EntryRendererOptions) { self.registered.entry_renderers.insert(custom_type.into(), renderer); self.registered.entry_renderer_options.insert(custom_type.into(), options); }
    pub fn register_mcp_server(&mut self, name: &str, config: McpServerDeclaration) {
        let declaration = RegisteredMcpServerDeclaration { name: name.into(), config, extension_path: self.registered.identity.path.clone(), registration_cwd: self.cwd.clone() };
        if let Some(existing) = self.registered.mcp_servers.iter_mut().find(|s| s.name == name) { *existing = declaration; } else { self.registered.mcp_servers.push(declaration); }
    }
    pub fn register_removed_tool_hint(&mut self, name: &str, hint: &str) { self.registered.removed_tool_hints.insert(name.into(), hint.into()); }
    pub fn register_filesystem_policy(&mut self, policy: FilesystemPolicy) { self.registered.filesystem_policies.push(policy); }
    pub fn send_message(&self, message: CustomMessage, options: SendMessageOptions) -> Result<(), ExtensionFailure> { self.runtime.actions()?.send_message(message, options) }
    pub fn send_user_message(&self, content: UserMessageContent, options: SendUserMessageOptions) -> Result<(), ExtensionFailure> { self.runtime.actions()?.send_user_message(content, options) }
    pub fn append_entry(&self, custom_type: &str, data: Option<JsonValue>) -> Result<(), ExtensionFailure> { self.runtime.actions()?.append_entry(custom_type, data) }
    pub fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> { self.runtime.actions()?.get_all_tools() }
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
