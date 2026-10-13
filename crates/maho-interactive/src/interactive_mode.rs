//! Native user/assistant transcript slice of interactive-mode.ts.
//! Command dispatch, runtime replacement and extension UI remain partial.
use std::{cell::RefCell, collections::BTreeMap, future::Future, pin::Pin, rc::Rc, sync::Arc};
use maho_agent::types::AgentEvent;
use maho_core::agent_session::{AgentSession, AgentSessionSubscription, PromptDisposition, PromptOptions};
use maho_tui::tui::{Component, Container};
use crate::{components::{assistant_message::AssistantMessageComponent, user_message::UserMessageComponent, markdown_transform::get_markdown_theme}, theme::Theme};
use crate::components::{tool_execution::{ToolExecutionComponent, ToolExecutionOptions, ToolExecutionPresentation}, tool_execution_types::ToolExecutionResult};
use crate::components::{custom_editor::{CustomEditor, CustomEditorOptions}, extension_editor::editor_theme};
use crate::grok::chrome::InteractiveChrome;

type ImageSubmissions = std::collections::VecDeque<(String, Vec<maho_ai::types::ImageContent>)>;
type CustomUiBuild = std::pin::Pin<Box<dyn std::future::Future<Output=Result<Box<dyn Component>, maho_ext_api::ExtensionFailure>>>>;
type QuestionActions = Rc<RefCell<std::collections::VecDeque<QuestionAction>>>;

/// senpi `handleShareCommand`'s in-flight state: the loader replacing the editor, the `gh gist
/// create` result, the cancel handle that kills the process when dropped, and the temp export.
struct ShareInFlight {
    loader: crate::components::bordered_loader::BorderedLoader,
    result: tokio::sync::oneshot::Receiver<std::io::Result<std::process::Output>>,
    _cancel: tokio::sync::oneshot::Sender<()>,
    tmp_file: std::path::PathBuf,
}
type AuthUiEvents = tokio::sync::mpsc::UnboundedReceiver<AuthUiEvent>;
/// A queued tree-edit prefill: `(entryId, editedText, expectedLeafId)`.
type PendingTreeEdit = (String, String, Option<String>);
/// A queued tree-navigation choice: `(entryId, summarize, customInstructions)`.
type PendingTreeNav = (String, bool, Option<String>);
/// The in-flight provider turn (senpi's awaited `session.prompt` in the main loop).
type TurnFuture = Pin<Box<dyn Future<Output = Result<PromptDisposition, String>>>>;
/// A requested abort (`AgentSession::abort`) that `poll_turn` drives next to the turn.
type AbortFuture = Pin<Box<dyn Future<Output = ()>>>;
/// A header slot in the document header container (senpi's `Component` children).
type HeaderSlot = Rc<RefCell<dyn Component>>;
/// The running `!` command: its card and the session's `executeBash`, polled by `poll_turn`.
type PendingBash = (Rc<RefCell<crate::components::bash_execution::BashExecutionComponent>>, Pin<Box<dyn Future<Output = Result<maho_tools::bash_executor::BashResult, String>>>>);
/// A running `flushCompactionQueue`; resolves to the failure notice it reported, if any.
type CompactionFlush = Pin<Box<dyn Future<Output = Option<String>>>>;

/// senpi `compactionQueuedMessages` / `compactionInFlightMessages`, shared with the running flush.
#[derive(Default)]
struct CompactionQueueState {
    queued: Vec<crate::compaction_queue_transfer::CompactionQueuedMessage>,
    in_flight: Vec<crate::compaction_queue_transfer::CompactionQueuedMessage>,
}

/// senpi `flushCompactionQueue`'s transfer dependencies. Messages are identified by their reserved
/// `enqueueOrder` (senpi compares object identity).
struct CompactionQueueTransfer {
    state: Rc<RefCell<CompactionQueueState>>,
    session: Arc<AgentSession>,
    host: Option<Arc<dyn crate::interactive_session::InteractiveSession>>,
    extension_commands: Vec<String>,
    tasks: tokio::task::JoinSet<()>,
    events: tokio::sync::mpsc::UnboundedSender<maho_ext_api::AgentSessionEvent>,
    failure: Option<String>,
}

impl crate::compaction_queue_transfer::TransferDependencies for CompactionQueueTransfer {
    type Error = String;
    fn take_batch(&mut self) -> Vec<crate::compaction_queue_transfer::CompactionQueuedMessage> {
        let mut state = self.state.borrow_mut();
        let batch = std::mem::take(&mut state.queued);
        state.in_flight.extend(batch.iter().cloned());
        batch
    }
    fn commit_accepted(&mut self, message: &crate::compaction_queue_transfer::CompactionQueuedMessage) -> bool {
        let mut state = self.state.borrow_mut();
        let Some(index) = state.in_flight.iter().position(|queued| queued.enqueue_order == message.enqueue_order) else { return false; };
        state.in_flight.remove(index);
        true
    }
    fn restore_undelivered(&mut self, messages: &[crate::compaction_queue_transfer::CompactionQueuedMessage]) -> usize {
        let mut state = self.state.borrow_mut();
        let restorable: Vec<_> = messages.iter().filter(|message| state.in_flight.iter().any(|queued| queued.enqueue_order == message.enqueue_order)).cloned().collect();
        state.in_flight.retain(|queued| !restorable.iter().any(|message| message.enqueue_order == queued.enqueue_order));
        let count = restorable.len();
        let rest = std::mem::take(&mut state.queued);
        state.queued = restorable.into_iter().chain(rest).collect();
        count
    }
    fn is_command(&self, message: &crate::compaction_queue_transfer::CompactionQueuedMessage) -> bool {
        is_extension_command_text(&message.text, &self.extension_commands)
    }
    async fn deliver_command(&mut self, message: &crate::compaction_queue_transfer::CompactionQueuedMessage) -> Result<(), String> {
        match self.host.clone() {
            Some(host) => host.prompt(message.text.clone(), PromptOptions::default()).await,
            None => self.session.prompt(&message.text, PromptOptions::default()).await.map(|_| ()),
        }
    }
    async fn deliver_first_prompt(&mut self, message: &crate::compaction_queue_transfer::CompactionQueuedMessage) -> Result<PromptDisposition, String> {
        let behavior = message.mode;
        if let Some(host) = self.host.clone() {
            return host.prompt(message.text.clone(), PromptOptions { streaming_behavior: Some(behavior), ..Default::default() }).await.map(|_| PromptDisposition::Started);
        }
        let session = self.session.clone();
        let text = message.text.clone();
        let events = self.events.clone();
        crate::compaction_queue_transfer::wait_for_prompt_disposition(&mut self.tasks, move |preflight, disposition| async move {
            let admitted: Arc<dyn Fn(PromptDisposition) + Send + Sync> = Arc::new(move |next| { preflight(true); disposition(next); });
            session.prompt(&text, PromptOptions { streaming_behavior: Some(behavior), prompt_admitted: Some(admitted), ..Default::default() }).await.map(|_| ())
        }, move |error: String| {
            let _ = events.send(maho_ext_api::AgentSessionEvent::ContinuationError { error_message: format!("Queued prompt failed after acceptance: {error}") });
        }).await.map_err(|error| error.to_string())
    }
    async fn deliver_queued(&mut self, message: &crate::compaction_queue_transfer::CompactionQueuedMessage) -> Result<(), String> {
        let follow_up = message.mode == maho_ext_api::StreamingBehavior::FollowUp;
        match self.host.clone() {
            Some(host) => if follow_up { host.follow_up(message.text.clone()).await } else { host.steer(message.text.clone()).await },
            None => {
                let options = maho_core::agent_session::QueuedInputOptions { enqueue_order: message.enqueue_order, ..Default::default() };
                if follow_up { self.session.follow_up(&message.text, None, options).await } else { self.session.steer(&message.text, None, options).await }
            }
        }
    }
    fn report_failure(&mut self, error: String, undelivered_count: usize) {
        self.failure = Some(format!("Failed to send queued message{}: {error}", if undelivered_count == 1 { "" } else { "s" }));
    }
}

/// senpi `isExtensionCommand`: a `/name` registered by an extension.
fn is_extension_command_text(text: &str, extension_commands: &[String]) -> bool {
    let Some(rest) = text.strip_prefix('/') else { return false; };
    let name = rest.split(' ').next().unwrap_or_default();
    extension_commands.iter().any(|command| command == name)
}

/// A step that needs the terminal released: the loop owning the terminal stops it, performs the
/// step (`InteractiveMode::open_external_editor` / process suspend), then restarts and force-renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalRequest { Suspend, ExternalEditor }

/// Work the collapsed question widget's mouse callbacks hand back to the mode, which owns the
/// registry and cannot be reached from a `Box<dyn FnMut>`.
enum QuestionAction {
    Expand,
    Next,
    ClickOption(usize),
    OwnAnswer,
    Expire(String),
    Progress(String, crate::components::ask_user_question_state::QuestionDraft),
    ExpandRequest(String),
    CloseList,
}

/// One login or logout flow's messages from its runtime task back to the single-threaded UI.
enum AuthUiEvent {
    Prompt(maho_ai::auth::types::AuthPrompt, tokio::sync::oneshot::Sender<Result<String, String>>),
    Notify(maho_ai::auth::types::AuthEvent),
    LoginFinished { provider_id: String, provider_name: String, method: crate::login_outcome::LoginMethod, result: Result<(), crate::login_outcome::LoginFailure>, selection_error: Option<String> },
    LoginCatalogFinished { provider_name: String, action: String, result: Result<(), String>, selection_error: Option<String>, selected_model: Option<maho_ai::model::Model> },
    AnthropicSubscriptionAuth,
    LogoutFinished { provider_name: String, auth_type: crate::components::oauth_selector::AuthSelectorAuthType, result: Result<(), String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthLifecycleOutcome {
    Success,
    Cancelled,
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthLifecycleEvent {
    PromptReady,
    LoginCompleted { provider_name: String, method: crate::login_outcome::LoginMethod, outcome: AuthLifecycleOutcome },
    LoginCatalogCompleted { provider_name: String, outcome: Result<(), String>, selection_error: Option<String> },
    LogoutCompleted { provider_name: String, auth_type: crate::components::oauth_selector::AuthSelectorAuthType, outcome: Result<(), String> },
}

/// A select prompt's answer slot: the selector callbacks fill it, `drain_auth_ui_events` forwards it.
type AuthSelectReply = Rc<RefCell<Option<Result<String, String>>>>;

struct InteractiveAuthInteraction {
    sender: tokio::sync::mpsc::UnboundedSender<AuthUiEvent>,
    signal: maho_ai::utils::abort::AbortSignal,
}

#[async_trait::async_trait]
impl maho_ai::auth::types::AuthInteraction for InteractiveAuthInteraction {
    fn signal(&self) -> Option<maho_ai::utils::abort::AbortSignal> { Some(self.signal.clone()) }

    async fn prompt(&self, prompt: maho_ai::auth::types::AuthPrompt) -> anyhow::Result<String> {
        if self.signal.aborted() { anyhow::bail!(crate::login_outcome::LOGIN_CANCELLED_NOTICE); }
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.sender.send(AuthUiEvent::Prompt(prompt, reply)).map_err(|_| anyhow::anyhow!(crate::login_outcome::LOGIN_CANCELLED_NOTICE))?;
        // senpi `showAuthPrompt`: the dialog's answer races the abort signal.
        tokio::select! {
            answer = receiver => answer.map_err(|_| anyhow::anyhow!(crate::login_outcome::LOGIN_CANCELLED_NOTICE))?.map_err(anyhow::Error::msg),
            () = self.signal.cancelled() => anyhow::bail!(crate::login_outcome::LOGIN_CANCELLED_NOTICE),
        }
    }

    fn notify(&self, event: maho_ai::auth::types::AuthEvent) { let _ = self.sender.send(AuthUiEvent::Notify(event)); }
}

/// senpi `ExpandableText` (`interactive-mode.ts:339`): a `Text` whose body follows the collapsed or
/// expanded render closure, toggled through `setExpanded` (senpi's `Expandable` probe).
pub struct ExpandableText {
    get_collapsed: Box<dyn Fn() -> String>,
    get_expanded: Box<dyn Fn() -> String>,
    text: String,
    padding_x: usize,
    padding_y: usize,
    expanded: bool,
}

impl ExpandableText {
    pub fn new(get_collapsed: Box<dyn Fn() -> String>, get_expanded: Box<dyn Fn() -> String>, expanded: bool, padding_x: usize, padding_y: usize) -> Self {
        let text = if expanded { get_expanded() } else { get_collapsed() };
        Self { get_collapsed, get_expanded, text, padding_x, padding_y, expanded }
    }

    /// senpi `setExpanded`.
    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
        self.text = if expanded { (self.get_expanded)() } else { (self.get_collapsed)() };
    }

    pub fn is_expanded(&self) -> bool { self.expanded }
}

impl Component for ExpandableText {
    fn render(&mut self, width: usize) -> Vec<String> {
        maho_tui::components::text::Text::with_padding(self.text.as_str(), self.padding_x, self.padding_y).render(width)
    }
}

/// The `InteractiveModeOptions` fields the header port consumes (senpi `interactive-mode.ts:793`).
#[derive(Default)]
pub struct InteractiveModeOptions {
    /// senpi `verbose`: force verbose startup, overriding `quietStartup`.
    pub verbose: bool,
    /// senpi `chrome`: select an experimental interactive chrome (`GrokChrome`).
    pub chrome: Option<crate::grok::chrome::GrokChrome>,
}

pub struct InteractiveMode {
    session: Arc<AgentSession>,
    events: tokio::sync::mpsc::UnboundedReceiver<maho_ext_api::AgentSessionEvent>,
    _subscription: AgentSessionSubscription,
    /// The shared-host runtime when the mode joined one; replacement commands route through it.
    session_host: Option<Arc<dyn crate::interactive_session::InteractiveSession>>,
    /// Kept alive so dropping it (replacement or dispose) tears down the remote event bridge.
    session_host_subscription: Option<crate::interactive_host_runtime::SessionSubscription>,
    /// Sender into the mode's event channel, so the remote bridge can feed decoded session events.
    events_sender: tokio::sync::mpsc::UnboundedSender<maho_ext_api::AgentSessionEvent>,
    /// The authoritative remote history fetched from the host, published before a synchronous
    /// rebuild. `None` means no fetch has completed; the local session is never used as a fallback
    /// while a host is mounted.
    remote_history: Option<Vec<maho_agent::types::AgentMessage>>,
    remote_history_generation: u64,
    remote_history_tx: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>,
    remote_history_rx: tokio::sync::mpsc::UnboundedReceiver<(u64, Result<Vec<serde_json::Value>, String>)>,
    /// The remote available-model catalog fetched from the host, so the model selectors read the
    /// remote catalog. `None` until a fetch completes; the local registry is never used while a host
    /// is mounted.
    remote_models: Option<Vec<maho_ai::model::Model>>,
    remote_models_generation: u64,
    remote_models_tx: tokio::sync::mpsc::UnboundedSender<(u64, Result<Vec<serde_json::Value>, String>)>,
    remote_models_rx: tokio::sync::mpsc::UnboundedReceiver<(u64, Result<Vec<serde_json::Value>, String>)>,
    /// The remote session stats fetched from the host, so the sync footer and `/session` read them.
    remote_stats: Option<maho_core::agent_session::SessionStats>,
    remote_stats_generation: u64,
    remote_stats_tx: tokio::sync::mpsc::UnboundedSender<(u64, Result<serde_json::Value, String>)>,
    remote_stats_rx: tokio::sync::mpsc::UnboundedReceiver<(u64, Result<serde_json::Value, String>)>,
    /// `true` once a shared host is mounted, so the local session's events are dropped instead of
    /// contaminating the remote view.
    remote_active: Arc<std::sync::atomic::AtomicBool>,
    /// Per-tool erased renderer snapshots resolved from the session's live runner and refreshed at
    /// async ingress, so the synchronous card path can consume registered renderers.
    tool_renderer_snapshots: std::collections::HashMap<String, Rc<RefCell<dyn crate::tools::renderers::ToolRenderers>>>,
    native_tool_renderer_snapshots: std::collections::HashMap<String, Rc<RefCell<dyn crate::tools::renderers::ToolRenderers>>>,
    /// senpi `chatContainer`: an `ExplorationTranscriptContainer` that groups read/search calls into
    /// Exploring/Explored cells and paints a bounded tail first when resuming long sessions.
    chat: crate::components::exploration_transcript_container::ExplorationTranscriptContainer,
    streaming: Option<Rc<RefCell<AssistantMessageComponent>>>,
    assistant_segments: BTreeMap<usize, Rc<RefCell<AssistantMessageComponent>>>,
    pending_tools: BTreeMap<String, Rc<RefCell<ToolExecutionComponent>>>,
    theme: Theme,
    pub editor: CustomEditor,
    submissions: Rc<RefCell<std::collections::VecDeque<String>>>,
    tree_copies: Rc<RefCell<std::collections::VecDeque<Option<String>>>>,
    /// senpi `editAssistantMessageFromTree`: the (entryId, editedText, expectedLeafId) captured when
    /// the tree edit editor opened (the leaf is the token the edit is checked against).
    pending_tree_edit: Rc<RefCell<Option<PendingTreeEdit>>>,
    /// senpi `runTreeNavigation`: the (entryId, summarize, customInstructions) chosen by the branch
    /// summary prompt, consumed by `/tree-navigate`.
    pending_tree_nav: Rc<RefCell<Option<PendingTreeNav>>>,
    /// The in-flight provider turn: `submit_start` initiates it and returns, `poll_turn` advances it
    /// so input pumping, event consumption and rendering keep running while a turn is in flight.
    pending_turn: Option<TurnFuture>,
    /// A requested abort (`AgentSession::abort`). `poll_turn` drives it next to the turn so the
    /// interrupt key never blocks the loop behind a held provider.
    pending_abort: Option<AbortFuture>,
    rename_input: Option<crate::components::extension_input::ExtensionInputComponent>,
    rename_result: Rc<RefCell<Option<Option<String>>>>,
    shortcut_overlay: bool,
    last_clear_ms: Option<u64>,
    /// senpi `lastEscapeTime`: the first press of a double-escape on an empty editor.
    last_escape_ms: Option<u64>,
    /// A terminal hand-off (suspend / external editor) the loop that owns the terminal performs.
    terminal_request: Option<TerminalRequest>,
    /// senpi `/hotkeys` Extensions rows: (normalized key, description or extension path).
    extension_shortcut_rows: Vec<(String, String)>,
    pub shutdown_requested: bool,
    pub agent_idle: bool,
    pub extension_ui: Arc<crate::interactive_extension_ui::InteractiveExtensionUi>,
    ui_requests: tokio::sync::mpsc::UnboundedReceiver<crate::interactive_extension_ui::UiRequest>,
    ui_dialog: Option<Box<dyn Component>>,
    login_dialog: Option<crate::components::login_dialog::LoginDialogComponent>,
    /// senpi `handleShareCommand`: the in-flight gist upload whose loader replaces the editor.
    share: Option<ShareInFlight>,
    ui_reply: Rc<RefCell<Option<tokio::sync::oneshot::Sender<Option<String>>>>>,
    /// senpi `headerContainer`: the document header slot holding the built-in header (with its tip
    /// sibling) or an extension header.
    header_container: Container,
    /// senpi `loadedResourcesContainer`: the startup resource listing between header and chat.
    loaded_resources_container: Container,
    /// The `LoadedResourceSection`s in `loaded_resources_container`, which follow the tools toggle.
    loaded_resource_sections: Vec<Rc<RefCell<crate::components::loaded_resource_section::LoadedResourceSection>>>,
    /// senpi `resourceLoader.getExtensions().extensions` as path + source info.
    loaded_extensions: Vec<crate::loaded_resource_scopes::ScopedResource>,
    /// senpi `builtInHeader`.
    built_in_header: Option<HeaderSlot>,
    /// The built-in header when it is an `ExpandableText` (senpi's `isExpandable` probe).
    built_in_expandable: Option<Rc<RefCell<ExpandableText>>>,
    /// senpi `customHeader`: the extension `setHeader` override.
    custom_header: Option<HeaderSlot>,
    /// senpi `options.verbose`.
    verbose: bool,
    /// senpi `this.chrome`.
    chrome: Option<crate::grok::chrome::GrokChrome>,
    footer: Option<Box<dyn Component>>,
    footer_data: Arc<maho_core::footer_data_provider::FooterDataProvider>,
    widgets: Vec<(String, Box<dyn Component>, maho_ext_api::WidgetPlacement)>,
    pub terminal_title: Option<String>,
    markdown_transformers: Vec<crate::components::markdown_transform::MarkdownTransformer>,
    /// The theme the Mermaid transformer styles diagrams with; senpi reads its live `theme` proxy.
    mermaid_theme: Rc<RefCell<Theme>>,
    reveal: crate::streaming_reveal::StreamingRevealController,
    clock: std::time::Instant,
    tool_reveal: crate::tool_result_reveal::ToolResultRevealController,
    tool_args_reveal: crate::tool_args_reveal::ToolArgsRevealController,
    tool_partial_json: BTreeMap<String, String>,
    last_status: Option<(usize, Rc<RefCell<maho_tui::components::text::Text>>)>,
    assistant_cards: Vec<Rc<RefCell<AssistantMessageComponent>>>,
    tool_cards: Vec<Rc<RefCell<ToolExecutionComponent>>>,
    pub tools_expanded: bool,
    local_dialog_reply: Option<tokio::sync::oneshot::Receiver<Option<String>>>,
    auth_ui_events: AuthUiEvents,
    auth_ui_sender: tokio::sync::mpsc::UnboundedSender<AuthUiEvent>,
    auth_lifecycle_events: tokio::sync::broadcast::Sender<AuthLifecycleEvent>,
    /// The running login's dialog cancellation; Escape in the dialog aborts the provider flow.
    auth_abort: Option<maho_ai::utils::abort::AbortController>,
    /// A text/secret/manual prompt awaiting the dialog's submitted value.
    pending_auth_prompt: Option<AuthPromptReply>,
    /// A select prompt awaiting the selector's choice.
    pending_auth_select: Option<(AuthSelectReply, tokio::sync::oneshot::Sender<Result<String, String>>)>,
    working_started_ms: Option<f64>,
    working_message: Option<String>,
    working_visible: bool,
    editor_host: Rc<dyn maho_tui::components::editor::EditorTuiHost>,
    hidden_thinking_label: String,
    history_expansion: Vec<Box<dyn FnMut(bool)>>,
    question: Option<crate::components::ask_user_question::AskUserQuestionComponent>,
    async_question_widget: Option<crate::components::ask_user_async_widget::AskUserAsyncWidget>,
    queued_questions: std::collections::VecDeque<crate::interactive_extension_ui::UiRequest>,
    question_reply: Rc<RefCell<Option<tokio::sync::oneshot::Sender<maho_ext_api::QuestionResponse>>>>,
    questions: crate::question_registry::QuestionRegistry,
    question_actions: QuestionActions,
    question_result: Rc<RefCell<Option<maho_ext_api::QuestionResponse>>>,
    blocking_question: bool,
    pending_images: Rc<RefCell<BTreeMap<u64, maho_ai::types::ImageContent>>>,
    submission_images: Rc<RefCell<ImageSubmissions>>,
    working_indicator: Option<maho_ext_api::WorkingIndicatorOptions>,
    custom_editor: Option<Box<dyn maho_tui::editor_component::EditorComponent>>,
    /// senpi `fdPath`: the managed `fd` binary for the `@` file search.
    fd_path: Option<String>,
    /// senpi `autocompleteProviderWrappers`: extension factories applied over the base provider.
    autocomplete_wrappers: Vec<maho_ext_api::AutocompleteProviderFactory>,
    managed_tool_status_started: bool,
    custom_ui_builds: Vec<CustomUiBuild>,
    custom_ui_result: Rc<RefCell<Option<serde_json::Value>>>,
    custom_ui_reply: Option<tokio::sync::oneshot::Sender<Result<serde_json::Value, maho_ext_api::ExtensionFailure>>>,
    mounted_renderer: Option<std::rc::Weak<RefCell<crate::tui_renderer::InteractiveTui>>>,
    terminal_dimensions: Rc<std::cell::Cell<(u16,u16)>>,
    custom_overlay: Option<maho_tui::tui::OverlayHandle>,
    debug_log_path: Option<String>,
    changelog_markdown: Option<String>,
    startup_notices_shown: bool,
    /// senpi `updateEditorBorderColor` inputs (thinking level, bash mode) last applied to the editor.
    editor_border_key: Option<(maho_ai::types::ModelThinkingLevel, bool)>,
    /// senpi `ArminComponent`'s `setInterval`: live animations with the time of their last frame.
    armin_animations: Vec<(Rc<RefCell<crate::components::armin::ArminComponent>>, f64)>,
    /// senpi `anthropicSubscriptionWarningShown`.
    anthropic_subscription_warning_shown: bool,
    /// senpi `continuityNotices`.
    continuity_notices: crate::components::continuity_notice::ContinuityNoticeTracker,
    /// senpi `activeStatusIndicator` for the non-working kinds (retry, compaction, branch summary).
    /// The working indicator is derived per frame by `working_frame`.
    active_status_indicator: Option<Rc<RefCell<crate::components::status_indicator::StatusIndicator>>>,
    /// senpi `idleStatus`: the placeholder that reserves the cleared status row's height.
    idle_status: Option<crate::components::status_indicator::IdleStatus>,
    /// senpi's compaction queue: submissions typed while compaction runs.
    compaction_queue: Rc<RefCell<CompactionQueueState>>,
    pending_compaction_flush: Option<CompactionFlush>,
    queued_compaction_flushes: std::collections::VecDeque<crate::compaction_queue_transfer::TransferOptions>,
    /// The local `!` command in flight (senpi `bashComponent` + `session.isBashRunning`).
    pending_bash: Option<PendingBash>,
    /// senpi `pendingBashComponents`: `!` cards started while streaming, shown above the editor
    /// until the next ordinary submission moves them into the chat.
    pending_bash_components: Vec<Rc<RefCell<crate::components::bash_execution::BashExecutionComponent>>>,
    /// senpi `questionMousePaused`; paused until a terminal is attached.
    question_mouse_paused: bool,
    /// senpi `releaseMouseLease` / `releaseAlwaysMouseLease`, held as renderer lease tokens.
    question_mouse_lease: Option<u64>,
    always_mouse_lease: Option<u64>,
    /// senpi `InteractiveThemeController.terminalTheme`.
    terminal_theme: crate::theme::TerminalTheme,
    terminal_color_scheme_reports: Rc<RefCell<Vec<maho_tui::terminal_colors::TerminalColorScheme>>>,
    terminal_color_scheme_listener: Option<u64>,
    theme_detection: Option<ThemeDetection>,
    /// A requested `applyFromSettings` (the theme setting to resolve) not yet started.
    pending_theme_setting: Option<Option<String>>,
    /// senpi `autoSyncEnabled` with its parsed `light/dark` pair from `currentThemeSetting`.
    theme_auto_sync: Option<(String, String)>,
}

/// senpi `resolveAutoTheme` / `resolveBackgroundTheme` in flight: the auto light/dark pair (when
/// the setting is `light/dark`) and the outstanding terminal query tickets.
struct ThemeDetection {
    auto: Option<(String, String)>,
    color_scheme: Option<maho_tui::tui::TerminalQueryTicket<Option<maho_tui::terminal_colors::TerminalColorScheme>>>,
    background: maho_tui::tui::TerminalQueryTicket<Option<maho_tui::terminal_colors::RgbColor>>,
}

/// senpi's detection timeout for both terminal theme queries.
const THEME_DETECTION_TIMEOUT_MS: u64 = 100;

fn terminal_theme_for_scheme(scheme: maho_tui::terminal_colors::TerminalColorScheme) -> crate::theme::TerminalTheme {
    match scheme {
        maho_tui::terminal_colors::TerminalColorScheme::Dark => crate::theme::TerminalTheme::Dark,
        maho_tui::terminal_colors::TerminalColorScheme::Light => crate::theme::TerminalTheme::Light,
    }
}
const ANTHROPIC_SUBSCRIPTION_AUTH_WARNING: &str = "Anthropic subscription auth is active. Third-party harness usage draws from extra usage and is billed per token, not your Claude plan limits. Manage extra usage at https://claude.ai/settings/usage. Disable this warning in /settings.";

fn is_anthropic_subscription_auth_key(api_key: Option<&str>) -> bool {
    api_key.is_some_and(|key| key.starts_with("sk-ant-oat"))
}

type AuthPromptReply = (tokio::sync::oneshot::Receiver<String>, tokio::sync::oneshot::Sender<Result<String, String>>);

impl InteractiveMode {
    pub fn subscribe_auth_lifecycle(&self) -> tokio::sync::broadcast::Receiver<AuthLifecycleEvent> {
        self.auth_lifecycle_events.subscribe()
    }

    pub async fn drain_next_auth_event(&mut self) -> Result<(), &'static str> {
        let event = self.auth_ui_events.recv().await.ok_or("Authentication event channel closed")?;
        self.handle_auth_ui_event(event);
        Ok(())
    }

    pub(crate) fn clock_ms(&self) -> u64 { u64::try_from(self.clock.elapsed().as_millis()).unwrap_or(u64::MAX) }
    pub(crate) fn tick_now(&mut self) { self.tick(self.clock.elapsed().as_secs_f64()*1000.0); }
    pub fn install_native_tool_renderers<S: Default + 'static>(&mut self, name: String, renderers: Arc<maho_ext_api::ToolRenderers<S, serde_json::Value>>) {
        self.native_tool_renderer_snapshots.insert(name, Rc::new(RefCell::new(crate::tools::renderers::native::NativeToolRenderers::new(renderers))));
    }
    pub fn clear_native_tool_renderers(&mut self) { self.native_tool_renderer_snapshots.clear(); }
    pub fn install_native_tool_renderer_snapshot<S: Default + 'static>(&mut self, snapshot: impl IntoIterator<Item = (String, Arc<maho_ext_api::ToolRenderers<S, serde_json::Value>>)>) {
        self.clear_native_tool_renderers();
        for (name, renderers) in snapshot { self.install_native_tool_renderers(name, renderers); }
    }
    pub(crate) fn set_terminal_dimensions(&self, columns:usize, rows:usize) { self.terminal_dimensions.set((u16::try_from(columns).unwrap_or(u16::MAX),u16::try_from(rows).unwrap_or(u16::MAX))); }
    pub(crate) fn set_mounted_renderer(&mut self, renderer: Rc<RefCell<crate::tui_renderer::InteractiveTui>>) { self.mounted_renderer = Some(Rc::downgrade(&renderer)); self.apply_smooth_streaming_render_fps(); }
    /// senpi `applySmoothStreamingRenderFps`: the render cap follows the smooth-streaming FPS while
    /// smooth streaming is on, and the default 60fps otherwise.
    pub fn smooth_streaming_render_fps(&self) -> f64 {
        if self.reveal.smooth { self.reveal.fps } else { crate::streaming_reveal_pacing::DEFAULT_SMOOTH_FPS }
    }
    fn apply_smooth_streaming_render_fps(&self) {
        let fps = self.smooth_streaming_render_fps();
        if let Some(renderer) = self.mounted_renderer.as_ref().and_then(std::rc::Weak::upgrade) { renderer.borrow_mut().base_mut().set_max_render_fps(fps); }
    }


    /// senpi `terminalMouseMode`.
    fn terminal_mouse_mode(&self) -> String {
        self.session.with_settings_manager(|settings| settings.get_string("terminalMouse")).unwrap_or_else(|| "whilePending".to_owned())
    }

    /// senpi `syncQuestionMouseCapture`: hold a "pending-question" lease while any ask-user
    /// question is pending or shown, plus an "always" lease in regular mode when
    /// `terminalMouse` is `always`. Idempotent, so the host calls it once per frame.
    pub fn sync_question_mouse_capture(&mut self, ui: &mut crate::tui_renderer::InteractiveTui, terminal: &mut dyn maho_tui::terminal::Terminal) {
        let wants_mouse_lease = !self.questions.is_empty() || self.question.is_some();
        let mode = self.terminal_mouse_mode();
        let enabled = !self.question_mouse_paused && mode != "off";
        if enabled && wants_mouse_lease {
            if self.question_mouse_lease.is_none() { self.question_mouse_lease = Some(ui.acquire_mouse_capture("pending-question", terminal)); }
        } else if let Some(token) = self.question_mouse_lease.take() {
            ui.release_mouse_capture(token, terminal);
        }
        if enabled && mode == "always" && ui.is_regular() {
            if self.always_mouse_lease.is_none() { self.always_mouse_lease = Some(ui.acquire_mouse_capture("always", terminal)); }
        } else if let Some(token) = self.always_mouse_lease.take() {
            ui.release_mouse_capture(token, terminal);
        }
    }

    /// senpi `pauseQuestionMouseCapture` (before the renderer stops).
    pub fn pause_question_mouse_capture(&mut self, ui: &mut crate::tui_renderer::InteractiveTui, terminal: &mut dyn maho_tui::terminal::Terminal) {
        self.question_mouse_paused = true;
        if let Some(token) = self.question_mouse_lease.take() { ui.release_mouse_capture(token, terminal); }
        if let Some(token) = self.always_mouse_lease.take() { ui.release_mouse_capture(token, terminal); }
    }

    /// senpi `resumeQuestionMouseCapture` (after the renderer starts).
    pub fn resume_question_mouse_capture(&mut self, ui: &mut crate::tui_renderer::InteractiveTui, terminal: &mut dyn maho_tui::terminal::Terminal) {
        self.question_mouse_paused = false;
        self.sync_question_mouse_capture(ui, terminal);
    }

    /// senpi `InteractiveThemeController` construction/`rebindTui`: subscribe to the renderer's
    /// terminal color-scheme reports and schedule `applyFromSettings`. `setting` is the
    /// launch theme setting (`--theme` or the `theme` setting).
    pub fn bind_terminal_theme(&mut self, ui: &mut crate::tui_renderer::InteractiveTui, setting: Option<String>) {
        if let Some(id) = self.terminal_color_scheme_listener.take() { ui.base_mut().remove_terminal_color_scheme_listener(id); }
        let reports = self.terminal_color_scheme_reports.clone();
        self.terminal_color_scheme_listener = Some(ui.base_mut().on_terminal_color_scheme_change(Box::new(move |scheme| reports.borrow_mut().push(scheme))));
        self.request_terminal_theme_detection(setting);
    }

    /// senpi `applyFromSettings` scheduling; the queries are issued on the next host frame.
    pub fn request_terminal_theme_detection(&mut self, setting: Option<String>) {
        self.theme_detection = None;
        self.pending_theme_setting = Some(setting);
    }

    /// The poll-driven half of senpi `InteractiveThemeController`: start a requested detection
    /// (auto `light/dark` settings query DSR 996 and OSC 11; an unset theme queries OSC 11),
    /// settle it once the tickets resolve, and follow live color-scheme reports while auto-sync
    /// is on. Call once per host frame.
    pub fn poll_terminal_theme(&mut self, ui: &mut crate::tui_renderer::InteractiveTui, terminal: &mut dyn maho_tui::terminal::Terminal) {
        let now_ms = u64::try_from(self.clock.elapsed().as_millis()).unwrap_or(u64::MAX);
        if let Some(setting) = self.pending_theme_setting.take() {
            let auto = crate::theme::parse_auto_theme_setting(setting.as_deref()).map(|(light, dark)| (light.to_owned(), dark.to_owned()));
            self.theme_auto_sync = auto.clone();
            ui.base_mut().set_terminal_color_scheme_notifications(auto.is_some(), terminal);
            if auto.is_some() || setting.is_none() {
                let color_scheme = auto.is_some().then(|| ui.base_mut().query_terminal_color_scheme(terminal, THEME_DETECTION_TIMEOUT_MS, now_ms));
                let background = ui.base_mut().query_terminal_background_color(terminal, THEME_DETECTION_TIMEOUT_MS, now_ms);
                self.theme_detection = Some(ThemeDetection { auto, color_scheme, background });
            }
        }
        ui.base_mut().poll_terminal_queries(now_ms);
        let settled = self.theme_detection.as_ref().and_then(|detection| {
            let scheme = match &detection.color_scheme { Some(ticket) => Some(ticket.result()?), None => None };
            Some((scheme.flatten(), detection.background.result()?))
        });
        if let Some((scheme, background)) = settled && let Some(detection) = self.theme_detection.take() {
            self.finish_terminal_theme_detection(detection, scheme, background);
        }
        let reports = std::mem::take(&mut *self.terminal_color_scheme_reports.borrow_mut());
        for scheme in reports { self.apply_terminal_theme(terminal_theme_for_scheme(scheme)); }
    }

    /// senpi `resolveAutoTheme` / `resolveBackgroundTheme` once the terminal answered or timed out.
    fn finish_terminal_theme_detection(&mut self, detection: ThemeDetection, scheme: Option<maho_tui::terminal_colors::TerminalColorScheme>, background: Option<maho_tui::terminal_colors::RgbColor>) {
        let colorfgbg = std::env::var("COLORFGBG").ok();
        let background_detection = match background {
            Some(rgb) => crate::theme::detection::TerminalThemeDetection { theme: crate::theme::theme::get_theme_for_rgb_color([rgb.r, rgb.g, rgb.b]), source: "terminal background", detail: format!("OSC 11 background rgb({}, {}, {})", rgb.r, rgb.g, rgb.b), confidence: "high" },
            None => crate::theme::detection::detect_terminal_background_from_env(colorfgbg.as_deref()),
        };
        let detected = scheme.map_or(background_detection.theme, terminal_theme_for_scheme);
        self.terminal_theme = detected;
        // A cache that cannot be written must never break a launch.
        let _ = crate::theme::terminal_theme_cache::write_terminal_theme_hint(detected, std::path::Path::new(&self.session.agent_dir()));
        match &detection.auto {
            Some((light, dark)) => { let name = if detected == crate::theme::TerminalTheme::Light { light.clone() } else { dark.clone() }; self.apply_theme_name(&name); }
            None => {
                if !self.apply_theme_name(detected.name()) { return; }
                if background_detection.confidence == "high" {
                    let values: maho_core::settings_manager::Settings = [("theme".to_owned(), serde_json::json!(detected.name()))].into_iter().collect();
                    if let Err(error) = self.session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &values)) { self.show_status(error); }
                }
            }
        }
    }

    /// senpi `applyTerminalTheme`: follow a live color-scheme report while the setting is auto.
    fn apply_terminal_theme(&mut self, terminal_theme: crate::theme::TerminalTheme) {
        let Some((light, dark)) = &self.theme_auto_sync else { return; };
        let name = if terminal_theme == crate::theme::TerminalTheme::Light { light.clone() } else { dark.clone() };
        self.terminal_theme = terminal_theme;
        if name != self.theme.name { self.apply_theme_name(&name); }
    }

    /// senpi `applyThemeName`: load and apply `name`, reporting a load failure.
    fn apply_theme_name(&mut self, name: &str) -> bool {
        match crate::theme::registry::ThemeRegistry::new(std::path::Path::new(&self.session.agent_dir()).join("themes"), name, self.theme.get_color_mode()).and_then(|registry| registry.load_theme(name)) {
            Ok(theme) => { self.theme = theme; *self.mermaid_theme.borrow_mut() = self.theme.clone(); self.editor.editor.border_color = editor_theme(&self.theme).border_color; self.editor_border_key = None; *self.extension_ui.theme.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = crate::interactive_extension_ui::extension_theme(&self.theme); self.rebuild_history(); true },
            Err(error) => { self.show_status(error.to_string()); false },
        }
    }
    /// senpi `/debug` writes the agent directory's global debug log; the override lets a caller redirect it.
    pub fn set_debug_log_path(&mut self, path: Option<String>) { self.debug_log_path = path; }
    /// The pending startup changelog markdown (senpi `changelogMarkdown`), once loaded.
    pub fn changelog_markdown(&self) -> Option<&str> { self.changelog_markdown.as_deref() }
    /// senpi `getTipsHistory`: the recorded `tipId -> timestamp` map (the tip persistence seam).
    pub fn tips_history(&self) -> std::collections::HashMap<String, u64> { self.session.with_settings_manager(tips_history) }

    /// senpi's `getChangelogSeen(source)`: the per-source record, or the legacy engine key.
    pub fn changelog_seen(&self, source_id: &str) -> Option<String> {
        self.session.with_settings_manager(|settings| {
            settings.get_value("changelogSeen").and_then(|value| value.get(source_id)).and_then(serde_json::Value::as_str).map(str::to_owned)
                .or_else(|| if source_id == "engine" { settings.get_string("lastChangelogVersion") } else { None })
        })
    }

    /// senpi's `setChangelogSeen(source, version)`: merge the version into the global record.
    fn set_changelog_seen(&mut self, source_id: &str, version: &str) {
        let mut seen = self.session.with_settings_manager(|settings| settings.get_value("changelogSeen").and_then(serde_json::Value::as_object).cloned().unwrap_or_default());
        seen.insert(source_id.to_owned(), serde_json::Value::String(version.to_owned()));
        let values: maho_core::settings_manager::Settings = [("changelogSeen".to_owned(), serde_json::Value::Object(seen))].into_iter().collect();
        let _ = self.session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &values));
    }

    /// senpi's `getChangelogForDisplay`; the production entry reads the resolved source.
    pub fn load_startup_changelog(&mut self) {
        self.load_startup_changelog_from(&maho_core::changelog_source::resolve_changelog_source());
    }

    /// senpi's `getChangelogForDisplay` plus its `setChangelogSeen` side effect. `source` is a seam
    /// (senpi reads `resolveChangelogSource()` inline) so a caller can point at a specific
    /// changelog. senpi's `reportInstallTelemetry` belongs to the telemetry port and is not
    /// invoked here.
    pub fn load_startup_changelog_from(&mut self, source: &maho_core::changelog_source::ChangelogSource) {
        let has_messages = !self.host_messages().is_empty();
        let last_version = self.changelog_seen(&source.id);
        let entries = crate::interactive_changelog::entries(&source.path);
        let display = crate::interactive_changelog::changelog_for_display(has_messages, source, last_version.as_deref(), &entries);
        if let Some(version) = display.record_version { self.set_changelog_seen(&source.id, &version); }
        self.changelog_markdown = display.markdown;
    }

    /// senpi's `showStartupNoticesIfNeeded`: render the pending changelog into the chat once.
    pub fn show_startup_notices_if_needed(&mut self) {
        if self.startup_notices_shown { return; }
        self.startup_notices_shown = true;
        let Some(markdown) = self.changelog_markdown.clone() else { return; };
        if !self.chat.children().is_empty() { self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1))))); }
        self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(crate::components::dynamic_border::DynamicBorder::new(self.theme.clone())))));
        let collapse = self.session.with_settings_manager(|settings| settings.get_bool("collapseChangelog").unwrap_or(false));
        if collapse {
            let latest = crate::interactive_changelog::latest_version_in_markdown(&markdown).unwrap_or_else(|| maho_core::config::display_version(maho_core::engine_build_identity::ENGINE_VERSION));
            let condensed = format!("Updated to v{latest}. Use {} to view full changelog.", self.theme.bold("/changelog"));
            self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding(condensed, 1, 0)))));
        } else {
            self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding(self.theme.bold(&self.theme.fg(crate::theme::ThemeColor::Accent, "What's New")), 1, 0)))));
            self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1)))));
            self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(crate::components::markdown_transform::MarkdownComponent(maho_tui::components::markdown::Markdown::new(markdown.trim(), 1, 0, get_markdown_theme(&self.theme), None, Default::default()))))));
            self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1)))));
        }
        self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(crate::components::dynamic_border::DynamicBorder::new(self.theme.clone())))));
    }
    // ---- senpi's built-in welcome header (`interactive-mode.ts:1598-1673`, `showStartupNoticesIfNeeded`)

    /// senpi's `getStartupExpansionState`: the constructor's `verbose` or the tool-output toggle.
    pub fn get_startup_expansion_state(&self) -> bool { self.verbose || self.tools_expanded }

    /// senpi's `recordShownTip`: merge `tipId -> now` into the global `tipsHistory` record.
    fn set_tip_shown(&mut self, tip_id: &str, now_ms: u64) {
        let history = self.session.with_settings_manager(tips_history);
        let next = crate::tips::history_writer::record_tip_shown(&history, tip_id, now_ms);
        let values: maho_core::settings_manager::Settings = [("tipsHistory".to_owned(), serde_json::to_value(next).unwrap_or_else(|_| serde_json::json!({})))].into_iter().collect();
        let _ = self.session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &values));
    }

    /// senpi's `resolveStartupTipLine` inputs. `has_command` stays `None` until the session exposes
    /// its extension command registry (senpi `hasRegisteredCommand`, a runtime API).
    fn resolve_startup_tip(&self, now_ms: u64) -> Option<crate::tips::startup_tip::StartupTipLine> {
        let (tips_enabled, quiet, history) = self.session.with_settings_manager(|settings| (settings.get_bool("tips").unwrap_or(true), settings.get_bool("quietStartup").unwrap_or(false), tips_history(settings)));
        crate::tips::startup_tip::resolve_startup_tip_line(crate::tips::startup_tip::StartupTipOptions {
            tips_enabled,
            quiet_startup: quiet,
            history: &history,
            now: now_ms,
            definitions: crate::tips::registry::TIP_DEFINITIONS.as_slice(),
            keys: &crate::components::keybinding_hints::key_text,
            has_command: None,
            exclude: None,
        })
    }

    /// senpi's startup header phase: load the changelog, then mount the welcome header and its tip
    /// sibling. Call once after construction (senpi does this inside `initialize`).
    pub fn initialize_startup_header(&mut self) {
        self.load_startup_changelog();
        self.mount_startup_header();
        self.show_loaded_resources(false, true);
    }

    /// senpi `showLoadedResources`: rebuild the loaded-resources container from the session's
    /// context files, skills, prompts, extensions and custom themes.
    pub fn show_loaded_resources(&mut self, force: bool, show_diagnostics_when_quiet: bool) {
        use crate::interactive_loaded_resources::{LoadedPrompt, LoadedResources, LoadedSkill, LoadedThemeResource, build_loaded_resources};
        self.loaded_resources_container.clear();
        self.loaded_resource_sections.clear();
        let quiet = self.session.with_settings_manager(|settings| settings.get_bool("quietStartup").unwrap_or(false));
        let show_listing = force || self.verbose || !quiet;
        let show_diagnostics = show_listing || show_diagnostics_when_quiet;
        if !show_listing && !show_diagnostics { return; }
        let cwd = self.session.cwd();
        let (system_prompt_source, append_sources) = self.session.system_prompt_sources();
        let existing = |source: &String| std::path::Path::new(source).exists().then(|| maho_core::paths::resolve_path(source, &cwd, &maho_core::paths::PathInputOptions::default()));
        let mut context_paths: Vec<String> = system_prompt_source.iter().filter_map(existing).collect();
        context_paths.extend(append_sources.iter().filter_map(existing));
        context_paths.extend(self.session.project_context_files().into_iter().map(|file| file.path));
        let skills_result = self.session.loaded_skills().unwrap_or_default();
        let themes = crate::theme::registry::ThemeRegistry::new(std::path::Path::new(&self.session.agent_dir()).join("themes"), &self.theme.name, self.theme.get_color_mode())
            .map(|registry| registry.get_available_themes_with_paths()).unwrap_or_default()
            .into_iter().filter_map(|theme| theme.path.map(|path| LoadedThemeResource { name: theme.name, source_path: path.to_string_lossy().into_owned(), source_info: None })).collect();
        let resources = LoadedResources {
            cwd,
            context_paths,
            skills: skills_result.skills.into_iter().map(|skill| LoadedSkill { name: skill.name, file_path: skill.file_path, source_info: Some(skill.source_info) }).collect(),
            skill_diagnostics: skills_result.diagnostics,
            prompts: self.session.prompt_templates().into_iter().map(|template| LoadedPrompt { name: template.name, file_path: template.file_path, source_info: Some(template.source_info) }).collect(),
            prompt_diagnostics: Vec::new(),
            extensions: self.loaded_extensions.clone(),
            extension_diagnostics: Vec::new(),
            themes,
            theme_diagnostics: Vec::new(),
        };
        let view = build_loaded_resources(&resources, show_listing, show_diagnostics, self.get_startup_expansion_state(), self.tools_expanded, &self.theme);
        for child in view.children { self.loaded_resources_container.add_child(child); }
        self.loaded_resource_sections = view.sections;
    }

    /// Production `mount_startup_header`, using the wall clock for the tip schedule (senpi `Date.now()`).
    pub fn mount_startup_header(&mut self) {
        let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX));
        self.mount_startup_header_at(now_ms);
    }

    /// senpi's header block: the chrome welcome card, the standard `ExpandableText` welcome content
    /// with its tip sibling, or the minimal quiet header.
    pub fn mount_startup_header_at(&mut self, now_ms: u64) {
        let app = maho_core::config::app_name();
        let version = maho_core::config::display_version(maho_core::engine_build_identity::ENGINE_VERSION);
        if let Some(chrome) = &self.chrome {
            let component = chrome.create_welcome_content(&app, &version);
            let header: HeaderSlot = Rc::new(RefCell::new(crate::interactive_ui_host::BoxedComponent(component)));
            self.header_container.add_child(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1))));
            self.header_container.add_child(header.clone());
            self.header_container.add_child(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1))));
            self.built_in_header = Some(header);
            return;
        }
        let quiet = self.session.with_settings_manager(|settings| settings.get_bool("quietStartup").unwrap_or(false));
        if !self.verbose && quiet {
            let header: HeaderSlot = Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding("", 0, 0)));
            self.header_container.add_child(header.clone());
            self.built_in_header = Some(header);
            return;
        }
        let tip = self.resolve_startup_tip(now_ms);
        let tip_line = match &tip {
            Some(tip) => { self.set_tip_shown(&tip.tip_id, now_ms); Some(self.theme.fg(crate::theme::ThemeColor::Dim, &tip.line)) }
            None => None,
        };
        let theme = self.theme.clone();
        let key = crate::components::keybinding_hints::key_text;
        let hint = |keybinding: &str, description: &str| crate::components::keybinding_hints::key_hint(keybinding, description, &theme);
        let raw = |key: &str, description: &str| crate::components::keybinding_hints::raw_key_hint(key, description, &theme);
        let logo = format!("{}{}", theme.bold(&theme.fg(crate::theme::ThemeColor::Accent, &app)), theme.fg(crate::theme::ThemeColor::Dim, &format!(" {}", crate::version_label::format_display_version(&version))));
        let expanded_instructions = [
            hint("app.interrupt", "to interrupt"),
            hint("app.clear", "to clear"),
            raw(&format!("{} twice", key("app.clear")), "to exit"),
            hint("app.exit", "to exit (empty)"),
            hint("app.suspend", "to suspend"),
            hint("tui.editor.deleteToLineEnd", "to delete to end"),
            hint("app.thinking.cycle", "to cycle thinking level"),
            raw(&format!("{}/{}", key("app.model.cycleForward"), key("app.model.cycleBackward")), "to cycle models"),
            hint("app.model.select", "to select model"),
            hint("app.tools.expand", "to expand tools"),
            hint("app.thinking.toggle", "to expand thinking"),
            hint("app.editor.external", "for external editor"),
            raw("/", "for commands"),
            raw("!", "to run bash"),
            raw("!!", "to run bash (no context)"),
            hint("app.message.followUp", "to queue follow-up"),
            hint("app.message.dequeue", "to edit all queued messages"),
            hint("app.clipboard.pasteImage", "to paste image (with text fallback)"),
            raw("drop files", "to attach"),
        ].join("\n");
        let compact_instructions = [
            hint("app.interrupt", "interrupt"),
            raw(&format!("{}/{}", key("app.clear"), key("app.exit")), "clear/exit"),
            raw("/", "commands"),
            raw("!", "bash"),
            hint("app.tools.expand", "more"),
        ].join(theme.fg(crate::theme::ThemeColor::Muted, " · ").as_str());
        let compact_onboarding = theme.fg(crate::theme::ThemeColor::Dim, &format!("Press {} to show full startup help and loaded resources.", key("app.tools.expand")));
        let onboarding = theme.fg(crate::theme::ThemeColor::Dim, &format!("{app} can explain its own features and look up its docs. Ask it how to use or extend {app}."));
        let collapsed_text = format!("{logo}\n{compact_instructions}\n{compact_onboarding}\n\n{onboarding}");
        let expanded_text = format!("{logo}\n{expanded_instructions}\n\n{onboarding}");
        let expandable = Rc::new(RefCell::new(ExpandableText::new(
            Box::new(move || collapsed_text.clone()),
            Box::new(move || expanded_text.clone()),
            self.get_startup_expansion_state(),
            1,
            0,
        )));
        let header: HeaderSlot = expandable.clone();
        crate::tips::startup_header::append_startup_header(&mut self.header_container, header.clone(), tip_line.as_deref());
        self.built_in_header = Some(header);
        self.built_in_expandable = Some(expandable);
    }

    /// senpi's `setExtensionHeader`: swap the active header slot, keeping the tip/spacer siblings.
    fn set_extension_header(&mut self, factory: Option<HeaderSlot>) {
        let Some(built_in) = self.built_in_header.clone() else { return; };
        let index = {
            let target = self.custom_header.clone().unwrap_or_else(|| built_in.clone());
            self.header_container.children.iter().position(|child| Rc::ptr_eq(child, &target))
        };
        if let Some(factory) = factory {
            self.custom_header = Some(factory.clone());
            // senpi probes `isExpandable(this.customHeader)`, but an extension factory returns
            // `Box<dyn Component>`, which cannot downcast to the private `ExpandableText`; extension
            // headers are therefore never expanded here.
            if let Some(index) = index { self.header_container.children[index] = factory; }
            else { self.header_container.children.insert(0, factory); }
        } else {
            self.custom_header = None;
            if let Some(expandable) = &self.built_in_expandable { expandable.borrow_mut().set_expanded(self.tools_expanded); }
            if let Some(index) = index { self.header_container.children[index] = built_in; }
        }
    }

    pub(crate) fn terminal_settings(&self) -> (bool, bool, bool) {
        self.session.with_settings_manager(|settings| (settings.get_bool("showHardwareCursor").unwrap_or(false), settings.get_bool("clearOnShrink").unwrap_or(false), settings.get_bool("showTerminalProgress").unwrap_or(true)))
    }
    fn keybindings(&self) -> maho_tui::keybindings::KeybindingsManager {
        maho_core::keybindings::KeybindingsManager::create(Some(&self.session.agent_dir())).inner().clone()
    }
    fn output_pad(&self) -> usize { self.session.with_settings_manager(|settings| settings.get_number("outputPad").unwrap_or(1.0) as usize) }
    pub fn new(session: Arc<AgentSession>, theme: Theme, host: Rc<dyn maho_tui::components::editor::EditorTuiHost>) -> Self {
        Self::new_with_options(session, theme, host, InteractiveModeOptions::default())
    }

    pub fn new_with_options(session: Arc<AgentSession>, theme: Theme, host: Rc<dyn maho_tui::components::editor::EditorTuiHost>, options: InteractiveModeOptions) -> Self {
        let (sender, events) = tokio::sync::mpsc::unbounded_channel();
        let events_sender = sender.clone();
        let remote_active = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let local_gate = remote_active.clone();
        let subscription = session.subscribe(Arc::new(move |event| { if !local_gate.load(std::sync::atomic::Ordering::SeqCst) { drop(sender.send(event.clone())); } }));
        let (remote_history_tx, remote_history_rx) = tokio::sync::mpsc::unbounded_channel();
        let (remote_models_tx, remote_models_rx) = tokio::sync::mpsc::unbounded_channel();
        let (remote_stats_tx, remote_stats_rx) = tokio::sync::mpsc::unbounded_channel();
        let submissions = Rc::new(RefCell::new(std::collections::VecDeque::new()));
        let captured = submissions.clone();
        let manager = maho_core::keybindings::KeybindingsManager::create(Some(&session.agent_dir())).inner().clone();
        // senpi `setKeybindings(KeybindingsManager.create())`: the TUI components read the global manager.
        maho_tui::keybindings::set_keybindings(manager.clone());
        let keys = Arc::new(manager);
        let mut editor = CustomEditor::new(host.clone(), editor_theme(&theme), keys, CustomEditorOptions { embed_working_status: true, ..Default::default() });
        let (padding, max_visible) = session.with_settings_manager(|settings| (settings.get_number("editorPaddingX").unwrap_or(0.0), settings.get_number("autocompleteMaxVisible").unwrap_or(10.0)));
        editor.set_padding_x(padding as usize); editor.editor.set_autocomplete_max_visible(max_visible as usize);
        editor.editor.set_autocomplete_provider(Rc::new(RefCell::new(Self::create_base_autocomplete_provider(&session, &session.cwd(), None))));
        let pending_images = Rc::new(RefCell::new(BTreeMap::<u64, maho_ai::types::ImageContent>::new()));
        let submission_images = Rc::new(RefCell::new(std::collections::VecDeque::new()));
        let images = pending_images.clone();
        editor.editor.on_image_markers_changed = Some(Box::new(move |order| {
            let previous = std::mem::take(&mut *images.borrow_mut());
            images.borrow_mut().extend(order.iter().enumerate().filter_map(|(index, id)| {
                previous.get(id).cloned().map(|image| (u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1), image))
            }));
        }));
        let images = pending_images.clone();
        editor.editor.snapshot_attachment_state = Some(Box::new(move || Some(Rc::new(images.borrow().clone()))));
        let images = pending_images.clone();
        editor.editor.restore_attachment_state = Some(Box::new(move |snapshot| {
            if let Some(snapshot) = snapshot.downcast_ref::<BTreeMap<u64, maho_ai::types::ImageContent>>() { *images.borrow_mut() = snapshot.clone(); }
        }));
        let images = pending_images.clone();
        let queued_images = submission_images.clone();
        let paste_submissions = submissions.clone();
        editor.on_paste_image = Some(Box::new(move || paste_submissions.borrow_mut().push_back("/paste-clipboard".into())));
        let question_actions: QuestionActions = Rc::new(RefCell::new(std::collections::VecDeque::new()));
        let empty_submit_actions = question_actions.clone();
        editor.editor.on_submit = Some(Box::new(move |text| {
            // senpi `onSubmit`: Enter on an empty editor opens the pending async question.
            if text.trim().is_empty() { empty_submit_actions.borrow_mut().push_back(QuestionAction::Expand); }
            else {
                let mut payloads = std::mem::take(&mut *images.borrow_mut());
                let ordered = maho_tui::image_markers::IMAGE_MARKER_REGEX.captures_iter(text)
                    .filter_map(|capture| capture.get(1).and_then(|id| id.as_str().parse::<u64>().ok()))
                    .filter_map(|id| payloads.remove(&id)).collect();
                queued_images.borrow_mut().push_back((text.trim().to_owned(), ordered));
                captured.borrow_mut().push_back(text.trim().into());
            }
        }));
        let (auth_ui_sender, auth_ui_events) = tokio::sync::mpsc::unbounded_channel();
        let (auth_lifecycle_events, _) = tokio::sync::broadcast::channel(32);
        let mermaid_session = session.clone();
        let mermaid_theme = Rc::new(RefCell::new(theme.clone()));
        let live_mermaid_theme = mermaid_theme.clone();
        let mermaid_transformer = crate::components::mermaid::create_live_mermaid_markdown_transformer(
            Rc::new(move || mermaid_session.with_settings_manager(|settings| {
                crate::components::mermaid::MermaidRenderingMode::from(settings.get_mermaid_rendering_mode())
            })), Rc::new(move || Some(live_mermaid_theme.borrow().clone())),
        );
        let (extension_ui, ui_requests) = crate::interactive_extension_ui::InteractiveExtensionUi::channel(crate::interactive_extension_ui::extension_theme(&theme));
        *extension_ui.theme_directory.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = std::path::Path::new(&session.agent_dir()).join("themes");
        let chat_host = host.clone();
        let chat = crate::components::exploration_transcript_container::ExplorationTranscriptContainer::new(crate::components::progressive_transcript_container::ProgressiveTranscriptOptions {
            tail_budget: crate::components::progressive_transcript_container::DEFAULT_TAIL_BUDGET,
            warm_chunk_size: crate::components::progressive_transcript_container::DEFAULT_WARM_CHUNK_SIZE,
            request_render: Rc::new(move || chat_host.request_render()),
        }, theme.clone());
        let (smooth, fps, hide) = session.with_settings_manager(|settings| (settings.get_bool("smoothStreaming").unwrap_or(true), settings.get_number("smoothStreamingFps").unwrap_or(60.0), settings.get_bool("hideThinkingBlock").unwrap_or(false)));
        Self { footer_data:Arc::new(maho_core::footer_data_provider::FooterDataProvider::new(&session.cwd())), tree_copies:Default::default(), pending_tree_edit:Default::default(), pending_tree_nav:Default::default(), pending_turn:None, pending_abort:None, queued_questions:Default::default(), terminal_dimensions:Rc::new(std::cell::Cell::new((80,u16::try_from(host.terminal_rows()).unwrap_or(u16::MAX)))), mounted_renderer:None, custom_overlay:None, debug_log_path:None, changelog_markdown:None, startup_notices_shown:false, editor_border_key:None, custom_ui_builds:Vec::new(), custom_ui_result:Rc::new(RefCell::new(None)), custom_ui_reply:None, working_indicator:None, custom_editor:None, last_escape_ms: None, terminal_request: None, extension_shortcut_rows: Vec::new(), question_mouse_paused:true, question_mouse_lease:None, always_mouse_lease:None, terminal_color_scheme_reports:Default::default(), terminal_color_scheme_listener:None, theme_detection:None, pending_theme_setting:None, theme_auto_sync:None, terminal_theme:if theme.name == "light" { crate::theme::TerminalTheme::Light } else { crate::theme::TerminalTheme::Dark }, fd_path:None, autocomplete_wrappers:Vec::new(), managed_tool_status_started:false, pending_images, submission_images, session, events, _subscription: subscription, session_host: None, session_host_subscription: None, events_sender, remote_history: None, remote_history_generation: 0, remote_history_tx, remote_history_rx, remote_models: None, remote_models_generation: 0, remote_models_tx, remote_models_rx, remote_stats: None, remote_stats_generation: 0, remote_stats_tx, remote_stats_rx, remote_active, tool_renderer_snapshots: std::collections::HashMap::new(), native_tool_renderer_snapshots: std::collections::HashMap::new(), chat, streaming: None, assistant_segments: BTreeMap::new(), pending_tools: BTreeMap::new(), theme, editor, submissions, rename_input: None, rename_result: Rc::new(RefCell::new(None)), shortcut_overlay: false, last_clear_ms: None, shutdown_requested: false, agent_idle: true, extension_ui, ui_requests, ui_dialog: None, login_dialog: None, ui_reply: Rc::new(RefCell::new(None)), header_container: Container::new(), built_in_header: None, built_in_expandable: None, custom_header: None, verbose: options.verbose, chrome: options.chrome, footer: None, widgets: Vec::new(), terminal_title: None, markdown_transformers: vec![mermaid_transformer], reveal: crate::streaming_reveal::StreamingRevealController::new(smooth, fps, hide), clock: std::time::Instant::now(), tool_reveal: crate::tool_result_reveal::ToolResultRevealController::new(smooth, fps), tool_args_reveal: crate::tool_args_reveal::ToolArgsRevealController::new(smooth, fps), tool_partial_json: BTreeMap::new(), last_status: None, assistant_cards: Vec::new(), tool_cards: Vec::new(), tools_expanded: false, local_dialog_reply: None, auth_ui_events, auth_ui_sender, auth_lifecycle_events, auth_abort: None, pending_auth_prompt: None, pending_auth_select: None, working_started_ms: None, working_message: None, working_visible: true, editor_host: host, hidden_thinking_label:"Thinking...".into(), history_expansion: Vec::new(), question:None, async_question_widget:None, question_reply:Rc::new(RefCell::new(None)), questions:Default::default(), question_actions:Rc::new(RefCell::new(std::collections::VecDeque::new())), question_result:Rc::new(RefCell::new(None)), blocking_question:false, armin_animations:Vec::new(), anthropic_subscription_warning_shown:false, continuity_notices:Default::default(), loaded_resources_container: Container::new(), loaded_resource_sections: Vec::new(), loaded_extensions: Vec::new(), mermaid_theme, active_status_indicator:None, idle_status:None, compaction_queue:Default::default(), pending_compaction_flush:None, queued_compaction_flushes:Default::default(), pending_bash:None, pending_bash_components:Vec::new(), share: None }
    }

    /// Mount the shared-host runtime as this mode's session host. With a host set, the replacement
    /// commands (`/new`, `/resume`, `/clone`, `/fork`, `/import-confirm`) route through it; the
    /// concrete `AgentSession` still answers the synchronous reads senpi's proxy mirrors.
    pub fn set_session_host(&mut self, host: Arc<dyn crate::interactive_session::InteractiveSession>) {
        let sender = self.events_sender.clone();
        self.session_host_subscription = host.subscribe_session_events(Arc::new(move |event| { let _ = sender.send(event.clone()); }));
        self.session_host = Some(host);
        self.remote_active.store(true, std::sync::atomic::Ordering::SeqCst);
        self.request_remote_history();
        self.request_remote_models();
        self.request_remote_stats();
    }

    /// Ask the mounted host for its session stats, generation-tagged like the other fetches.
    fn request_remote_stats(&mut self) {
        let Some(host) = self.session_host.clone() else { return; };
        self.remote_stats_generation = self.remote_stats_generation.wrapping_add(1);
        host.request_remote_stats(self.remote_stats_generation, self.remote_stats_tx.clone());
    }

    /// Publish a completed stats fetch whose generation is still current.
    fn drain_remote_stats(&mut self) {
        while let Ok((generation, result)) = self.remote_stats_rx.try_recv() {
            if generation != self.remote_stats_generation { continue; }
            if let Ok(value) = result {
                self.remote_stats = Some(session_stats_from_wire(&value));
            }
        }
    }

    /// The session stats the footer and `/session` read: the cached remote stats while a host is
    /// mounted (default until the fetch completes), else the local session's.
    fn host_session_stats(&self) -> maho_core::agent_session::SessionStats {
        match self.remote_stats.clone() {
            Some(stats) => stats,
            None => if self.session_host.is_some() { maho_core::agent_session::SessionStats::default() } else { self.session.get_session_stats() },
        }
    }

    /// Ask the mounted host for its available-model catalog, generation-tagged like the history fetch.
    fn request_remote_models(&mut self) {
        let Some(host) = self.session_host.clone() else { return; };
        self.remote_models_generation = self.remote_models_generation.wrapping_add(1);
        host.request_remote_models(self.remote_models_generation, self.remote_models_tx.clone());
    }

    /// Publish a completed model-catalog fetch whose generation is still current.
    fn drain_remote_models(&mut self) {
        while let Ok((generation, result)) = self.remote_models_rx.try_recv() {
            if generation != self.remote_models_generation { continue; }
            if let Ok(models) = result {
                self.remote_models = Some(models.iter().filter_map(|model| serde_json::from_value(model.clone()).ok()).collect());
            }
        }
    }

    /// Ask the mounted host for its authoritative history, tagged with a fresh generation so a
    /// stale completion (superseded by a newer request or a replacement) is discarded.
    fn request_remote_history(&mut self) {
        let Some(host) = self.session_host.clone() else { return; };
        self.remote_history_generation = self.remote_history_generation.wrapping_add(1);
        host.request_remote_history(self.remote_history_generation, self.remote_history_tx.clone());
    }

    /// Publish any completed history fetch whose generation is still current, then rebuild the
    /// transcript synchronously from the snapshot. A failure reports a status; it never falls back
    /// to the unrelated local session.
    fn drain_remote_history(&mut self) {
        while let Ok((generation, result)) = self.remote_history_rx.try_recv() {
            if generation != self.remote_history_generation { continue; }
            match result {
                Ok(messages) => {
                    self.remote_history = Some(messages.iter().filter_map(|message| serde_json::from_value(message.clone()).ok()).collect());
                    self.rebuild_history();
                }
                Err(error) => self.show_status(format!("Failed to load remote session history: {error}")),
            }
        }
    }

    /// The mounted host's mirrored `RpcSessionState`, when a shared host is active.
    pub fn remote_state_snapshot(&self) -> Option<crate::interactive_host_runtime::RemoteSessionState> {
        self.session_host.as_ref().and_then(|host| host.remote_state())
    }

    /// `false` once a shared host is mounted: the local session's events are dropped so they cannot
    /// contaminate the remote view.
    pub fn local_events_enabled(&self) -> bool {
        !self.remote_active.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Remote-authoritative synchronous reads. When a shared host is mounted these come from the
    /// mirrored `RpcSessionState`; otherwise from the local session. A mounted host never reads the
    /// stale local value, so the view follows the remote.
    fn host_cwd(&self) -> String {
        self.remote_state_snapshot().and_then(|state| state.cwd).unwrap_or_else(|| self.session.cwd())
    }

    fn host_model(&self) -> maho_ai::model::Model {
        self.remote_state_snapshot().and_then(|state| state.model).and_then(|value| serde_json::from_value(value).ok()).unwrap_or_else(|| self.session.model())
    }

    fn host_thinking_level(&self) -> maho_ai::types::ModelThinkingLevel {
        match self.remote_state_snapshot() {
            Some(state) => state.thinking_level.as_deref().and_then(maho_ai::types::ModelThinkingLevel::parse).unwrap_or(maho_ai::types::ModelThinkingLevel::Off),
            None => self.session.thinking_level(),
        }
    }

    fn host_session_name(&self) -> Option<String> {
        match self.remote_state_snapshot() { Some(state) => state.session_name, None => self.session.session_name() }
    }

    fn host_session_file(&self) -> Option<String> {
        match self.remote_state_snapshot() { Some(state) => state.session_file, None => self.session.session_file() }
    }

    fn host_is_streaming(&self) -> bool {
        match self.remote_state_snapshot() { Some(state) => state.is_streaming, None => self.session.is_streaming() }
    }

    fn host_is_compacting(&self) -> bool {
        match self.remote_state_snapshot() { Some(state) => state.is_compacting, None => self.session.is_compacting() }
    }

    fn host_auto_compaction_enabled(&self) -> bool {
        match self.remote_state_snapshot() { Some(state) => state.auto_compaction_enabled, None => self.session.auto_compaction_enabled() }
    }

    fn host_favorite_models(&self) -> Vec<maho_core::agent_session::SessionModelEntry> {
        match self.remote_state_snapshot() { Some(state) => session_model_entries(&state.favorite_models), None => self.session.favorite_models() }
    }

    fn host_scoped_models(&self) -> Vec<maho_core::agent_session::SessionModelEntry> {
        match self.remote_state_snapshot() { Some(state) => session_model_entries(&state.scoped_models), None => self.session.scoped_models() }
    }

    fn host_context_usage(&self) -> Option<maho_core::agent_session::ContextUsage> {
        match self.remote_state_snapshot() { Some(state) => state.context_usage.as_ref().and_then(context_usage_from_wire), None => self.session.get_context_usage() }
    }

    /// The model catalog the selectors read: the fetched remote catalog while a host is mounted
    /// (empty until the fetch completes), else the local registry.
    fn host_available_models(&self) -> Vec<maho_ai::model::Model> {
        if self.session_host.is_some() { self.remote_models.clone().unwrap_or_default() } else { self.session.model_registry().get_available() }
    }

    /// The transcript messages the mode reads: the fetched remote history while a host is mounted,
    /// else the local session's messages.
    fn host_messages(&self) -> Vec<maho_agent::types::AgentMessage> {
        if self.session_host.is_some() { self.remote_history.clone().unwrap_or_default() } else { self.session.messages() }
    }

    // ---- turn/session mutations routed through the mounted host ------------------------------

    async fn host_abort(&self) -> Result<(), String> {
        match self.session_host.clone() { Some(host) => host.abort().await, None => { self.session.abort().await; Ok(()) } }
    }

    async fn host_steer(&self, text: &str) -> Result<(), String> {
        match self.session_host.clone() { Some(host) => host.steer(text.to_owned()).await, None => self.session.steer(text, None, Default::default()).await }
    }

    async fn host_follow_up(&self, text: &str) -> Result<(), String> {
        match self.session_host.clone() { Some(host) => host.follow_up(text.to_owned()).await, None => self.session.follow_up(text, None, Default::default()).await }
    }

    async fn host_compact(&self, instructions: Option<String>) -> Result<(), String> {
        match self.session_host.clone() { Some(host) => host.compact(instructions).await, None => self.session.compact(instructions.as_deref()).await.map(|_| ()) }
    }

    async fn host_navigate_tree(&mut self, entry_id: &str, options: maho_core::agent_session::TreeNavigationOptions) -> Result<maho_core::agent_session::AssistantEditResult, String> {
        match self.session_host.clone() {
            Some(host) => { let result = host.navigate_tree(entry_id.to_owned(), options).await?; self.request_remote_history(); Ok(result) }
            None => self.session.navigate_tree(entry_id, options).await,
        }
    }

    async fn host_edit_assistant_message(&mut self, entry_id: &str, text: &str, options: maho_core::agent_session::TreeNavigationOptions) -> Result<maho_core::agent_session::AssistantEditResult, String> {
        match self.session_host.clone() {
            Some(host) => { let result = host.edit_assistant_message(entry_id.to_owned(), text.to_owned(), options).await?; self.request_remote_history(); Ok(result) }
            None => self.session.edit_assistant_message(entry_id, text, options).await,
        }
    }

    async fn host_reload(&self) -> Result<bool, String> {
        match self.session_host.clone() { Some(host) => host.reload().await, None => self.session.reload().await.map(|decision| !decision.cancelled) }
    }

    /// senpi `clearAllQueues`: the native queue plus the compaction queue, in enqueue order.
    fn host_clear_queue(&self, abort_will_follow: bool) -> Vec<maho_core::agent_session::QueuedInput> {
        let mut ordered = match self.session_host.clone() {
            Some(host) => { host.fire_clear_queue(abort_will_follow); ordered_inputs_from_remote_state(self.remote_state_snapshot().as_ref()) }
            None => self.session.clear_queue(abort_will_follow).ordered,
        };
        let compaction = {
            let mut state = self.compaction_queue.borrow_mut();
            let mut messages = std::mem::take(&mut state.in_flight);
            messages.extend(std::mem::take(&mut state.queued));
            messages
        };
        if compaction.is_empty() { return ordered; }
        let fallback = ordered.iter().map(|message| message.enqueue_order).max().unwrap_or(0);
        ordered.extend(compaction.into_iter().enumerate().map(|(index, message)| maho_core::agent_session::QueuedInput {
            text: message.text, mode: message.mode, enqueue_order: message.enqueue_order.unwrap_or(fallback + index as u64 + 1),
        }));
        ordered.sort_by_key(|message| message.enqueue_order);
        ordered
    }

    async fn host_set_model(&self, provider: &str, id: &str) -> Result<Option<String>, String> {
        match self.session_host.clone() { Some(host) => host.set_model(provider.to_owned(), id.to_owned()).await.map(|()| None), None => { let model = self.session.model_registry().find(provider, id).ok_or_else(|| format!("Model not found: {provider}/{id}"))?; self.session.set_model(model).await.map(|change| change.and_then(|change| change.system_prompt_name)) } }
    }

    /// senpi `findExactModelMatch`: the scoped (else available) snapshot first; with no scope and
    /// no cached hit, refresh the catalogs under a 15s timeout and search the refreshed snapshot.
    async fn find_exact_model_match(&mut self, search_term: &str) -> Option<maho_ai::model::Model> {
        let scoped = self.host_scoped_models();
        let narrowed = !scoped.is_empty();
        let cached: Vec<_> = if narrowed { scoped.into_iter().map(|entry| entry.model).collect() } else { self.host_available_models() };
        let cached_match = maho_core::model_resolver::find_exact_model_reference_match(search_term, &cached).cloned();
        if cached_match.is_some() || narrowed { return cached_match; }
        // A mounted host owns its catalog; only the local runtime is refreshed here.
        if self.session_host.is_none() {
            self.show_status("Refreshing model catalogs…".into());
            let runtime = self.session.model_registry().model_runtime.clone();
            let controller = maho_ai::utils::abort::AbortController::new();
            let signal = controller.signal();
            let start = move |signal: maho_ai::utils::abort::AbortSignal| async move {
                let mut runtime = runtime;
                runtime.refresh(maho_ai::models::ModelsRefreshOptions { signal: Some(signal), ..Default::default() }).await
            };
            let timer = tokio::spawn(async move { tokio::time::sleep(std::time::Duration::from_secs(15)).await; controller.abort(None); });
            let outcome = crate::model_catalog_refresh::refresh_model_catalogs(Arc::as_ptr(&self.session) as usize as u64, &signal, start).await;
            timer.abort();
            let timed_out = signal.aborted();
            match outcome {
                Ok(result) if result.aborted && timed_out => self.show_warning("Model refresh timed out; searching cached models."),
                Ok(result) if !result.errors.is_empty() => self.show_warning(&format!("Could not refresh {}; searching cached models.", result.errors.keys().cloned().collect::<Vec<_>>().join(", "))),
                Ok(_) => {}
                Err(_) if timed_out => self.show_warning("Model refresh timed out; searching cached models."),
                Err(error) => self.show_warning(&format!("Could not refresh model catalogs: {error}")),
            }
        }
        maho_core::model_resolver::find_exact_model_reference_match(search_term, &self.host_available_models()).cloned()
    }

    /// senpi `selectModelFromUi`: switch, then report `Model: <id>` with the optimized-prompt suffix.
    async fn select_model_from_ui(&mut self, model: maho_ai::model::Model) {
        match self.host_set_model(&model.provider, &model.id).await {
            Ok(system_prompt_name) => {
                if let Some(footer) = &mut self.footer { footer.invalidate(); }
                self.update_editor_border_color();
                let suffix = system_prompt_name.map(|name| format!(" (optimized system prompt applied: {name})")).unwrap_or_default();
                self.show_status(format!("Model: {}{suffix}", model.id));
                if crate::risky_main_model_warning::is_risky_main_model(&model) {
                    self.show_notice_box(crate::components::earendil_announcement::NoticeSpec { title: "Risky model warning".into(), tone: Some(crate::theme::ThemeColor::Error), why: crate::risky_main_model_warning::RISKY_MAIN_MODEL_WARNING.into(), extra: Vec::new(), expanded_line: None });
                }
            }
            Err(error) => self.show_error(&error),
        }
    }

    fn complete_provider_authentication(&mut self, provider_id: String, provider_name: String, method: crate::login_outcome::LoginMethod, immediate_selection_error: Option<String>) {
        let action = match method { crate::login_outcome::LoginMethod::Oauth => format!("Logged in to {provider_name}"), crate::login_outcome::LoginMethod::ApiKey => format!("Saved API key for {provider_name}") };
        let previous_model = self.session.model();
        let session_id = self.session.session_id();
        let runtime = self.session.model_registry().model_runtime.clone();
        let defer_selection = previous_model.provider == "unknown" && previous_model.id == "unknown"
            && maho_core::model_resolver::DEFAULT_MODEL_PER_PROVIDER.iter().any(|(provider, _)| *provider == provider_id);
        if let Some(error) = &immediate_selection_error { self.show_status(error.clone()); }
        else if defer_selection { self.show_status(format!("{action}. Credentials saved to {}. Refreshing model catalog...", maho_core::config::get_auth_path())); }
        else { self.show_status(format!("{action}. Credentials saved to {}", maho_core::config::get_auth_path())); }
        let sender = self.auth_ui_sender.clone();
        let session = self.session.clone();
        let host = self.session_host.clone();
        let providers = if provider_id == "cursor" { vec![provider_id.clone(), "cursor-cli-oauth".to_owned()] } else { vec![provider_id.clone()] };
        tokio::spawn(async move {
            let mut runtime = runtime;
            let abort = maho_ai::utils::abort::AbortController::new();
            let timeout_abort = abort.clone();
            let timeout = tokio::spawn(async move { tokio::time::sleep(std::time::Duration::from_secs(15)).await; timeout_abort.abort(None); });
            let refresh = runtime.refresh(maho_ai::models::ModelsRefreshOptions { allow_network: Some(true), providers: Some(providers), signal: Some(abort.signal()), ..Default::default() }).await;
            timeout.abort();
            let mut selection_error = immediate_selection_error;
            if previous_model.provider == "unknown" && previous_model.id == "unknown" && session.model() == previous_model && session.session_id() == session_id {
                let models = runtime.get_available(Some(&provider_id)).await;
                let provider_models = models.iter().filter(|model| model.provider == provider_id).collect::<Vec<_>>();
                if provider_id == "llama.cpp" {
                    selection_error = Some(if provider_models.is_empty() { format!("{action}. No llama.cpp models are loaded. Use /llama to load a model, then /model to select it.") } else { format!("{action}. Use /model to select a loaded llama.cpp model, or /llama to manage models.") });
                } else if !maho_core::model_resolver::DEFAULT_MODEL_PER_PROVIDER.iter().any(|(provider, _)| *provider == provider_id) {
                    selection_error = Some(format!("{action}, but no default model is configured for provider \"{provider_id}\". Use /model to select a model."));
                } else if provider_models.is_empty() {
                    selection_error = Some(format!("{action}, but no models are available for that provider. Use /model to select a model."));
                } else {
                    let default_id = maho_core::model_resolver::DEFAULT_MODEL_PER_PROVIDER.iter().find(|(provider, _)| *provider == provider_id).map(|(_, id)| *id);
                    let selected = provider_models.iter().find(|model| Some(model.id.as_str()) == default_id).copied()
                        .or_else(|| if provider_id == "radius" { provider_models.first().copied() } else { None });
                    match selected {
                        None => selection_error = Some(format!("{action}, but its default model \"{}\" is not available. Use /model to select a model.", default_id.unwrap_or(""))),
                        Some(model) => {
                            let result = match host {
                                Some(host) => host.set_model(provider_id.clone(), model.id.clone()).await,
                                None => session.set_model(model.clone()).await.map(|_| ()).map_err(|error| error.to_string()),
                            };
                            match result {
                                Ok(()) => { let _ = sender.send(AuthUiEvent::LoginCatalogFinished { provider_name: provider_name.clone(), action: format!("{action}. Selected {}.", model.id), result: Ok(()), selection_error: None, selected_model: Some(model.clone()) }); return; }
                                Err(error) => selection_error = Some(format!("{action}, but selecting its default model failed: {error}. Use /model to select a model.")),
                            }
                        }
                    }
                }
            }
            let error = refresh.errors.values().next().map(|error| error.to_string());
            let _ = sender.send(AuthUiEvent::LoginCatalogFinished { provider_name, action, result: if refresh.aborted { Err("aborted".into()) } else if let Some(error) = error { Err(error) } else { Ok(()) }, selection_error, selected_model: None });
        });
    }

    async fn host_cycle_model(&self, forward: bool) -> Result<Option<String>, String> {
        match self.session_host.clone() { Some(host) => host.cycle_model(forward).await, None => Ok(self.session.cycle_model(forward).await?.map(|result| result.model.name)) }
    }

    async fn host_export_jsonl(&self, output_path: Option<&str>) -> Result<Option<String>, String> {
        match self.session_host.clone() { Some(host) => host.export_jsonl(output_path.map(str::to_owned)).await, None => self.session.export_to_jsonl(output_path).map(Some).map_err(|error| error.to_string()) }
    }

    async fn host_export_html(&self, output_path: Option<&str>, theme_name: &str) -> Result<Option<String>, String> {
        if let Some(host) = self.session_host.clone() {
            return host.export_html(output_path.map(str::to_owned), Some(theme_name.to_owned())).await;
        }
        self.session.export_to_html_with_theme(
            output_path, Some(theme_name), crate::interactive_extension_ui::extension_theme(&self.theme),
            Some((self.theme.resolved_colors(), self.theme.export_colors().clone())),
        ).map(Some)
    }

    /// Fire-and-forget session-name set for the synchronous rename-input path; the typed `/rename`
    /// command awaits the host through `apply_session_name`.
    fn fire_set_session_name(&self, name: &str) {
        match self.session_host.clone() {
            Some(host) => { let name = name.to_owned(); tokio::spawn(async move { let _ = host.set_session_name(name).await; }); }
            None => self.session.set_session_name(name),
        }
    }

    /// senpi's `applySessionName`: await the rename, then report the value the session kept.
    async fn apply_session_name(&mut self, name: &str) {
        let host = self.session_host.clone();
        match host {
            Some(host) => { let _ = host.set_session_name(name.to_owned()).await; }
            None => self.session.set_session_name(name),
        }
        self.show_status(format!("Session name set: {}", self.host_session_name().unwrap_or_else(|| name.into())));
    }

    /// Fire-and-forget session thinking-level set for the synchronous paths.
    fn fire_set_session_thinking_level(&self, level: &str) {
        match self.session_host.clone() {
            Some(host) => { let level = level.to_owned(); tokio::spawn(async move { let _ = host.set_session_thinking_level(level).await; }); }
            None => self.session.set_session_thinking_level(maho_ai::types::ModelThinkingLevel::parse(level).unwrap_or(maho_ai::types::ModelThinkingLevel::Off)),
        }
    }

    fn host_find_model(&self, provider: &str, id: &str) -> Option<maho_ai::model::Model> {
        if self.session_host.is_some() {
            self.remote_models.as_ref().and_then(|models| models.iter().find(|model| model.provider == provider && model.id == id).cloned())
        } else {
            self.session.model_registry().find(provider, id)
        }
    }

    /// Thinking levels for the active model: derived from the mirrored remote model while a host is
    /// mounted, else the local session's.
    fn host_available_thinking_levels(&self) -> Vec<maho_ai::types::ModelThinkingLevel> {
        match self.remote_state_snapshot() {
            Some(state) => state.model.and_then(|value| serde_json::from_value::<maho_ai::model::Model>(value).ok()).map(|model| maho_core::thinking_levels::get_supported_thinking_levels(&model)).unwrap_or_default(),
            None => self.session.get_available_thinking_levels(),
        }
    }

    /// The session name the UI shows: the remote-authoritative name when a host is mounted, else the
    /// local session's.
    fn session_display_name(&self) -> Option<String> {
        self.host_session_name()
    }

    pub fn new_with_host(
        session: Arc<AgentSession>,
        host: Arc<dyn crate::interactive_session::InteractiveSession>,
        theme: Theme,
        editor_host: Rc<dyn maho_tui::components::editor::EditorTuiHost>,
    ) -> Self {
        let mut mode = Self::new(session, theme, editor_host);
        mode.set_session_host(host);
        mode
    }

    async fn host_new_session(&mut self, parent_session: Option<String>) -> Result<bool, String> {
        match self.session_host.clone() {
            Some(host) => match host.new_session(parent_session).await? {
                crate::interactive_session::ReplacementOutcome::Replaced => { self.refresh_tool_renderer_snapshots().await; self.request_remote_history(); self.request_remote_models(); self.request_remote_stats(); Ok(true) }
                crate::interactive_session::ReplacementOutcome::Cancelled => Ok(false),
                crate::interactive_session::ReplacementOutcome::LocalHandoff => self.session.new_session(None).await,
            },
            None => self.session.new_session(None).await,
        }
    }

    async fn host_switch_session(&mut self, path: String, cwd_override: Option<String>) -> Result<bool, String> {
        match self.session_host.clone() {
            Some(host) => match host.switch_session_with_cwd(path.clone(), cwd_override.clone()).await? {
                crate::interactive_session::ReplacementOutcome::Replaced => { self.refresh_tool_renderer_snapshots().await; self.request_remote_history(); self.request_remote_models(); self.request_remote_stats(); Ok(true) }
                crate::interactive_session::ReplacementOutcome::Cancelled => Ok(false),
                crate::interactive_session::ReplacementOutcome::LocalHandoff => self.session.switch_session_with_cwd(&path, cwd_override.as_deref()).await,
            },
            None => self.session.switch_session_with_cwd(&path, cwd_override.as_deref()).await,
        }
    }

    /// senpi `runtimeHost.importFromJsonl(path, cwdOverride)`.
    async fn host_import_from_jsonl(&mut self, path: String, cwd_override: Option<String>) -> Result<bool, String> {
        match self.session_host.clone() {
            Some(host) => match host.import_from_jsonl(path.clone(), cwd_override.clone()).await? {
                crate::interactive_session::ReplacementOutcome::Replaced => { self.refresh_tool_renderer_snapshots().await; self.request_remote_history(); self.request_remote_models(); self.request_remote_stats(); Ok(true) }
                crate::interactive_session::ReplacementOutcome::Cancelled => Ok(false),
                crate::interactive_session::ReplacementOutcome::LocalHandoff => self.session.import_from_jsonl(&path, cwd_override.as_deref()).await,
            },
            None => self.session.import_from_jsonl(&path, cwd_override.as_deref()).await,
        }
    }

    /// senpi `promptForMissingSessionCwd`: a Yes/No confirm; Yes queues `confirm_command`, No
    /// queues `cancel_command`.
    fn prompt_for_missing_session_cwd(&mut self, issue: &maho_core::session_cwd::SessionCwdIssue, path: &str, confirm_command: &str, cancel_command: &str) {
        let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
        let selected = self.ui_reply.clone(); let cancelled = selected.clone();
        let submissions = self.submissions.clone(); let cancel_submissions = self.submissions.clone();
        let confirm = format!("{confirm_command} \"{path}\""); let cancel = cancel_command.to_owned(); let dismissed = cancel.clone();
        let title = format!("Session cwd not found\n{}", maho_core::session_cwd::format_missing_session_cwd_prompt(issue));
        self.ui_dialog = Some(Box::new(crate::components::extension_selector::ExtensionSelectorComponent::new(&self.theme, Arc::new(self.keybindings()), &title, vec!["Yes".into(), "No".into()],
            Box::new(move |choice| { submissions.borrow_mut().push_back(if choice == "Yes" { confirm.clone() } else { cancel.clone() }); selected.borrow_mut().take(); }),
            Box::new(move || { cancel_submissions.borrow_mut().push_back(dismissed.clone()); cancelled.borrow_mut().take(); }), Default::default())));
    }

    /// senpi `handleResumeSession`: switch, prompting to continue in the current cwd when the
    /// session's recorded cwd no longer exists.
    async fn handle_resume_session(&mut self, path: String, cwd_override: Option<String>) -> Result<(), String> {
        if cwd_override.is_none() && let Some(issue) = missing_session_cwd_issue(&path, &self.host_cwd()) {
            self.prompt_for_missing_session_cwd(&issue, &path, "/resume-cwd", "/resume-cwd-cancel");
            return Ok(());
        }
        let in_current_cwd = cwd_override.is_some();
        if self.host_switch_session(path, cwd_override).await? {
            self.rebuild_history(); self.editor.editor.set_text("");
            self.show_status(if in_current_cwd { "Resumed session in current cwd" } else { "Resumed session" }.into());
        }
        Ok(())
    }

    /// senpi `handleImportCommand` after its confirm: import through the runtime host, prompting
    /// to continue in the current cwd when the imported session's cwd no longer exists.
    async fn handle_import_session(&mut self, path: String, cwd_override: Option<String>) -> Result<(), String> {
        if cwd_override.is_none() && let Some(issue) = missing_session_cwd_issue(&path, &self.host_cwd()) {
            self.prompt_for_missing_session_cwd(&issue, &path, "/import-cwd", "/import-cancel");
            return Ok(());
        }
        match self.host_import_from_jsonl(path.clone(), cwd_override).await {
            Ok(true) => { self.rebuild_history(); self.editor.editor.set_text(""); self.show_status(format!("Session imported from: {path}")); Ok(()) }
            Ok(false) => { self.show_status("Import cancelled".into()); Ok(()) }
            Err(error) => Err(format!("Failed to import session: {error}")),
        }
    }

    async fn host_fork(&mut self, entry_id: String, include_entry: bool) -> Result<maho_core::agent_session::AssistantEditResult, String> {
        match self.session_host.clone() {
            Some(host) => match host.fork(entry_id.clone(), include_entry).await? {
                crate::interactive_session::ForkOutcome { outcome: crate::interactive_session::ReplacementOutcome::Replaced, editor_text } => { self.refresh_tool_renderer_snapshots().await; self.request_remote_history(); self.request_remote_models(); self.request_remote_stats(); Ok(maho_core::agent_session::AssistantEditResult { editor_text, ..Default::default() }) }
                crate::interactive_session::ForkOutcome { outcome: crate::interactive_session::ReplacementOutcome::Cancelled, .. } => Ok(maho_core::agent_session::AssistantEditResult { cancelled: true, ..Default::default() }),
                crate::interactive_session::ForkOutcome { outcome: crate::interactive_session::ReplacementOutcome::LocalHandoff, .. } => self.session.fork(&entry_id, include_entry).await,
            },
            None => self.session.fork(&entry_id, include_entry).await,
        }
    }

    pub fn use_registered_markdown_transformers(&mut self, extensions: &[maho_ext_api::LoadedExtension]) {
        self.loaded_extensions = extensions.iter().map(|extension| {
            let info = &extension.source_info;
            let scope = match info.scope { maho_ext_api::SourceScope::User => maho_core::source_info::SourceScope::User, maho_ext_api::SourceScope::Project => maho_core::source_info::SourceScope::Project, maho_ext_api::SourceScope::Temporary => maho_core::source_info::SourceScope::Temporary, maho_ext_api::SourceScope::System => maho_core::source_info::SourceScope::System };
            let origin = match info.origin { maho_ext_api::SourceOrigin::Package => maho_core::source_info::SourceOrigin::Package, maho_ext_api::SourceOrigin::TopLevel => maho_core::source_info::SourceOrigin::TopLevel };
            crate::loaded_resource_scopes::ScopedResource { path: extension.identity.path.clone(), source_info: Some(maho_core::source_info::SourceInfo { path: info.path.clone(), source: info.source.clone(), scope, origin, base_dir: info.base_dir.clone() }) }
        }).collect();
        self.markdown_transformers = extensions.iter().filter_map(|extension| extension.markdown_transformer.clone()).map(|transformer| -> crate::components::markdown_transform::MarkdownTransformer {
            Rc::new(move |text, context| {
                use crate::components::markdown_transform::MessageType;
                let message_type = match context.message_type { MessageType::User => maho_ext_api::MarkdownMessageType::User, MessageType::Assistant => maho_ext_api::MarkdownMessageType::Assistant, MessageType::AssistantThinking => maho_ext_api::MarkdownMessageType::AssistantThinking };
                Ok(Some(transformer(text, &maho_ext_api::MarkdownTransformContext { message_type, is_streaming: context.is_streaming, available_width: context.available_width })))
            })
        }).collect();
    }

    fn create_base_autocomplete_provider(session: &AgentSession, cwd: &str, fd_path: Option<String>) -> maho_tui::autocomplete::CombinedAutocompleteProvider {
        use maho_tui::autocomplete::CommandSpec;
        let mut commands: Vec<_> = maho_core::slash_commands::builtin_slash_commands().into_iter().map(|command| CommandSpec::Command { name:command.name.into(), description:Some(command.description.into()), argument_hint:command.argument_hint.map(str::to_owned), get_argument_completions:None }).collect();
        commands.push(CommandSpec::command("answer"));
        let builtin_command_names: std::collections::BTreeSet<String> = commands.iter().map(|command| command.name().to_owned()).collect();
        commands.extend(session.prompt_templates().into_iter().map(|template| CommandSpec::Command { name:template.name, description:Some(template.description), argument_hint:template.argument_hint, get_argument_completions:None }));
        // senpi: registered extension commands (by invocation name) that do not shadow a built-in,
        // then `skill:<name>` entries when `enableSkillCommands` is on.
        let (extension_commands, _, skills) = session.command_groups();
        commands.extend(extension_commands.into_iter().filter(|command| !builtin_command_names.contains(&command.name)).map(|command| CommandSpec::Command { name:command.name, description:command.description, argument_hint:None, get_argument_completions:None }));
        if session.with_settings_manager(|settings| settings.get_bool("enableSkillCommands").unwrap_or(true)) {
            commands.extend(skills.into_iter().map(|skill| CommandSpec::Command { name:format!("skill:{}", skill.name), description:skill.description, argument_hint:None, get_argument_completions:None }));
        }
        maho_tui::autocomplete::CombinedAutocompleteProvider::new(commands, cwd, fd_path)
    }

    /// senpi `setupAutocompleteProvider`: the base provider wrapped by every registered extension
    /// factory, installed on the default editor and on an active custom editor.
    fn setup_autocomplete(&mut self) {
        let build = |mode: &Self| -> Box<dyn maho_tui::autocomplete::AutocompleteProvider> {
            let mut provider: Box<dyn maho_tui::autocomplete::AutocompleteProvider> = Box::new(Self::create_base_autocomplete_provider(&mode.session, &mode.host_cwd(), mode.fd_path.clone()));
            for wrap_provider in &mode.autocomplete_wrappers { provider = (**wrap_provider)(provider); }
            provider
        };
        let provider = build(self);
        maho_tui::editor_component::EditorComponent::set_autocomplete_provider(&mut self.editor, provider);
        if self.custom_editor.is_some() {
            let provider = build(self);
            if let Some(editor) = self.custom_editor.as_mut() { editor.set_autocomplete_provider(provider); }
        }
    }

    /// senpi: the `fd` path resolved by `ensureTool("fd")` after the TUI mounts feeds the `@` file search.
    pub fn set_fd_path(&mut self, fd_path: Option<String>) {
        crate::startup_tools::record_startup_fd_path(fd_path.clone());
        self.fd_path = fd_path;
        self.setup_autocomplete();
    }

    /// senpi `showManagedToolStatus`: managed-tool download progress lines in the chat.
    pub fn show_managed_tool_status(&mut self, kind: &str, message: &str) {
        if !self.managed_tool_status_started {
            self.chat.add_component(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1))));
            self.managed_tool_status_started = true;
        }
        let (message, color) = if kind == "warning" { (format!("Warning: {message}"), crate::theme::ThemeColor::Warning) } else { (message.to_owned(), crate::theme::ThemeColor::Dim) };
        self.chat.add_component(Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding(self.theme.fg(color, &message), 1, 0))));
        self.last_status = None;
    }

    pub fn rebuild_history(&mut self) {
        self.chat.clear(); self.pending_tools.clear(); self.streaming = None; self.assistant_segments.clear();
        self.tool_partial_json.clear(); self.tool_args_reveal.stop(); self.tool_reveal.stop(); self.reveal.stop();
        self.assistant_cards.clear(); self.tool_cards.clear(); self.last_status = None;
        self.history_expansion.clear(); self.armin_animations.clear();
        // senpi: the rebuilt transcript re-derives continuity notices, so suppression state resets.
        self.continuity_notices.reset();
        // A mounted shared host is authoritative: rebuild from the fetched remote snapshot, never
        // the unrelated local session. Without a host, the local session is the source.
        if self.session_host.is_some() {
            if let Some(messages) = self.remote_history.clone() {
                for message in &messages { self.add_history_message(message); }
            }
        } else {
            for message in self.session.messages() { self.add_history_message(&message); }
            // senpi `renderInitialMessages`: show compaction info if the session was compacted.
            let compaction_count = self.session.with_session_manager(|manager| manager.count_compactions());
            if compaction_count > 0 {
                let times = if compaction_count == 1 { "1 time".to_owned() } else { format!("{compaction_count} times") };
                self.show_status(format!("Session compacted {times}"));
            }
        }
    }

    pub fn add_history_message(&mut self, message: &maho_agent::types::AgentMessage) {
        use maho_agent::types::{AgentMessage, CustomAgentMessage};
        match message {
            AgentMessage::Llm(maho_ai::types::Message::Assistant(message)) => { crate::replay_assistant_tools::replay_assistant_tools(message, self); self.add_continuity_notice(message); }
            AgentMessage::Llm(maho_ai::types::Message::ToolResult(message)) => {
                if let Some(component) = self.pending_tools.remove(&message.tool_call_id) {
                    let value = serde_json::to_value(message).expect("tool result");
                    component.borrow_mut().update_result(Self::tool_result(&value, message.is_error), false);
                }
            }
            AgentMessage::Llm(maho_ai::types::Message::User(_)) => {
                self.handle_event(&AgentEvent::MessageStart { message: message.clone() });
                let value = serde_json::to_value(message).expect("user message");
                let text = value["content"].as_str().map(str::to_owned).unwrap_or_else(|| value["content"].as_array().map(|parts| parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default());
                self.editor.editor.add_to_history(&text);
            }
            AgentMessage::Custom(CustomAgentMessage::Custom(message)) if message.display => {
                let component = Rc::new(RefCell::new(crate::components::custom_message::CustomMessageComponent::new(serde_json::to_value(message).expect("custom message"), None, self.theme.clone(), get_markdown_theme(&self.theme), self.output_pad())));
                component.borrow_mut().set_expanded(self.tools_expanded); self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(component.clone()));
                self.history_expansion.push(Box::new(move |expanded| component.borrow_mut().set_expanded(expanded)));
            }
            AgentMessage::Custom(CustomAgentMessage::BranchSummary(message)) => {
                let component = Rc::new(RefCell::new(crate::components::branch_summary_message::BranchSummaryMessageComponent::new(message.summary.clone(), self.theme.clone(), get_markdown_theme(&self.theme), crate::components::keybinding_hints::key_display_text("app.tools.expand"))));
                component.borrow_mut().set_expanded(self.tools_expanded); self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(component.clone()));
                self.history_expansion.push(Box::new(move |expanded| component.borrow_mut().set_expanded(expanded)));
            }
            AgentMessage::Custom(CustomAgentMessage::CompactionSummary(message)) => {
                let component = Rc::new(RefCell::new(crate::components::compaction_summary_message::CompactionSummaryMessageComponent::new(serde_json::to_value(message).expect("summary"), self.theme.clone(), get_markdown_theme(&self.theme), crate::components::keybinding_hints::key_display_text("app.tools.expand"))));
                component.borrow_mut().set_expanded(self.tools_expanded); self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(component.clone()));
                self.history_expansion.push(Box::new(move |expanded| component.borrow_mut().set_expanded(expanded)));
            }
            AgentMessage::Custom(CustomAgentMessage::BashExecution(message)) => {
                let mut component = crate::components::bash_execution::BashExecutionComponent::new(&message.command, message.exclude_from_context.unwrap_or(false), self.theme.clone());
                component.append_output(&message.output);
                component.set_complete(message.exit_code.map(|code| i32::try_from(code).expect("exit code")), message.cancelled, None, message.full_output_path.clone());
                component.set_expanded(self.tools_expanded);
                let component = Rc::new(RefCell::new(component)); self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(component.clone()));
                self.history_expansion.push(Box::new(move |expanded| component.borrow_mut().set_expanded(expanded)));
            }
            _ => {}
        }
    }

    pub async fn bind_extensions(&mut self) {
        let session = self.session.clone();
        let bindings = maho_core::agent_session::ExtensionBindings { ui_context: Some(self.extension_ui.clone()), mode: Some(maho_ext_api::ExtensionMode::Tui), ..Default::default() };
        let binding = session.bind_extensions(bindings);
        tokio::pin!(binding);
        loop { tokio::select! { () = &mut binding => break, Some(request) = self.ui_requests.recv() => self.handle_ui_request(request) } }
        self.drain_ui_requests();
        // senpi `rebindCurrentSession` -> `setupAutocompleteProvider`: extension commands and skills
        // are known only once the session is bound.
        self.setup_autocomplete();
        self.refresh_tool_renderer_snapshots().await;
        self.setup_extension_shortcuts().await;
    }

    /// senpi `setupExtensionShortcuts`: resolve the runner's shortcuts against the effective
    /// keybindings and route a matching key to its handler with a fresh extension context.
    async fn setup_extension_shortcuts(&mut self) {
        let effective = maho_core::keybindings::KeybindingsManager::create(Some(&self.session.agent_dir())).get_effective_config();
        let (shortcuts, diagnostics) = self.session.extension_shortcuts(&effective).await;
        for diagnostic in diagnostics { self.show_status(diagnostic); }
        self.extension_shortcut_rows = shortcuts.iter().map(|(key, shortcut)| (key.clone(), shortcut.description.clone().unwrap_or_else(|| shortcut.extension_path.clone()))).collect();
        let shortcuts: Vec<(String, maho_ext_api::ExtensionShortcut)> = shortcuts.into_iter().collect();
        let session = self.session.clone();
        let ui = self.extension_ui.clone();
        self.editor.on_extension_shortcut = Some(Box::new(move |data| {
            let Some((_, shortcut)) = shortcuts.iter().find(|(key, _)| maho_tui::keys::matches_key(data, key)) else { return false; };
            let handler = shortcut.handler.clone();
            let session = session.clone();
            let ui = ui.clone();
            // Run the handler async with a fresh context; do not block input.
            tokio::spawn(async move {
                let result = match session.extension_shortcut_context().await {
                    Some(context) => handler(&context).await,
                    None => Err(maho_ext_api::ExtensionFailure::new("Extension runner is not bound")),
                };
                if let Err(error) = result {
                    maho_ext_api::ExtensionUi::notify(&*ui, &format!("Shortcut handler error: {}", error.message), maho_ext_api::NotificationType::Error);
                }
            });
            true
        }));
    }

    pub fn handle_input_at(&mut self, data: &str, now_ms: u64) {
        if let Some(data) = self.filter_terminal_input(data) { self.handle_filtered_input_at(&data, now_ms); }
    }

    fn filter_terminal_input(&self, data: &str) -> Option<String> {
        let listeners = self.extension_ui.terminal_input.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let mut data = data.to_owned();
        for listener in listeners {
            if let Some(result) = listener(&data) {
                if result.consume.unwrap_or(false) { return None; }
                if let Some(replacement) = result.data { data = replacement; }
            }
        }
        Some(data)
    }

    fn handle_filtered_input_at(&mut self, data: &str, now_ms: u64) {
        if self.async_question_widget.is_some() {
            let keys = self.keybindings();
            if self.ui_dialog.is_none() && self.rename_input.is_none() && crate::components::ask_user_answer_key::matches_ask_user_answer_key(data, &maho_core::keybindings::host_platform(), &keys) { self.expand_shown_question(None); return; }
            if keys.matches(data, "app.question.next") && self.questions.cycle() { self.refresh_async_question_widget(); return; }
            if let Some(digit) = single_digit(data) {
                let option = usize::try_from(digit).unwrap_or(1) - 1;
                if let Some(index) = self.shown_unanswered_index() && self.shown_question_has_option(index, option) {
                    self.expand_shown_question(Some(index));
                    if let Some(question) = &mut self.question { question.handle_input(data); }
                    self.settle_questions();
                    return;
                }
            }
            if data.chars().count() == 1 && data.chars().next().is_some_and(|character| !character.is_control() && !matches!(character, '/' | '!')) {
                let shown = self.questions.shown_id().map(str::to_owned);
                self.questions.set_composer_reply(shown.as_deref());
            }
        }
        if self.questions.surface == crate::question_registry::QuestionSurface::Expanded && let Some(question) = &self.question
            && ((self.keybindings().matches(data,"tui.select.cancel") && question.state.focus != crate::components::ask_user_question_state::QuestionFocus::OwnAnswer) || maho_tui::keys::matches_key(data,"ctrl+c")) {
            self.collapse_shown_question(); return;
        }
        if self.async_question_widget.is_none() && let Some(question) = &mut self.question { question.handle_input(data); self.settle_questions(); return; }
        if self.share.is_some() || self.ui_dialog.is_some() || self.login_dialog.is_some() || self.rename_input.is_some() { self.handle_editor_input(data); return; }
        if self.shortcut_overlay { self.shortcut_overlay = false; return; }
        let keys = self.keybindings();
        if self.editor.on_extension_shortcut.as_mut().is_some_and(|shortcut| shortcut(data)) { return; }
        if keys.matches(data, "app.model.select") { if let Err(error) = self.dispatch_command("/model") { self.show_status(error); } return; }
        for (action, command) in [("app.session.tree", "/tree"), ("app.session.fork", "/fork"), ("app.session.renameCurrent", "/rename"), ("app.session.resume", "/resume")] {
            if keys.matches(data, action) { if let Err(error) = self.dispatch_command(command) { self.show_status(error); } return; }
        }
        if keys.matches(data, "app.interrupt") && !(self.custom_editor.is_none() && self.editor.editor.is_showing_autocomplete()) { self.handle_escape(now_ms); return; }
        if keys.matches(data, "app.suspend") {
            // senpi `handleCtrlZ`.
            if cfg!(windows) { self.show_status("Suspend to background is not supported on Windows".into()); }
            else { self.terminal_request = Some(TerminalRequest::Suspend); }
            return;
        }
        if keys.matches(data, "app.editor.external") { self.terminal_request = Some(TerminalRequest::ExternalEditor); return; }
        if keys.matches(data, "app.session.new") { self.submissions.borrow_mut().push_back("/new".into()); return; }
        if keys.matches(data, "app.clear") {
            if self.last_clear_ms.is_some_and(|last| now_ms.saturating_sub(last) < 500) { self.shutdown_requested = true; }
            else { self.editor.editor.set_text(""); self.last_clear_ms = Some(now_ms); }
            return;
        }
        if keys.matches(data, "app.exit") && self.editor.editor.get_text().is_empty() { self.shutdown_requested = true; return; }
        if keys.matches(data, "app.thinking.cycle") {
            match self.session_host.clone() {
                Some(host) => { tokio::spawn(async move { let _ = host.cycle_thinking_level().await; }); }
                None => { if self.session.cycle_thinking_level().is_none() { self.show_status("Current model does not support thinking".into()); } }
            }
            return;
        }
        if keys.matches(data, "app.message.dequeue") { self.restore_queued_messages(false); return; }
        if keys.matches(data, "app.tools.expand") { self.set_tools_expanded(!self.tools_expanded); return; }
        if keys.matches(data, "app.thinking.toggle") {
            self.reveal.hide_thinking = !self.reveal.hide_thinking;
            let hidden = self.reveal.hide_thinking;
            if let Some(component) = &self.streaming { let content = self.reveal.resync_visibility(self.clock.elapsed().as_secs_f64() * 1000.0); component.borrow_mut().update_content(&content, Some(true)); }
            if let Err(error) = self.session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &[("hideThinkingBlock".into(), serde_json::json!(hidden))].into_iter().collect())) { self.show_status(error); }
            for component in &self.assistant_cards { component.borrow_mut().set_hide_thinking_block(self.reveal.hide_thinking); }
            self.show_status(format!("Thinking blocks: {}", if hidden { "hidden" } else { "visible" }));
            return;
        }
        if data == "?" && self.editor.editor.get_text().is_empty() { self.shortcut_overlay = true; return; }
        self.handle_editor_input(data);
    }

    /// Pop the next queued editor submission (draining any pending tree copies first), paired with
    /// the pinned steer options. `None` when nothing is queued.
    async fn next_submission(&mut self) -> Option<(String, PromptOptions)> {
        loop {
            let copy=self.tree_copies.borrow_mut().pop_front();
            let Some(copy)=copy else {break;};
            if let Some(text)=copy.filter(|text|!text.is_empty()) {
                match crate::interactive_clipboard::copy(&text).await {
                    Ok(sequence)=>{if let Some(sequence)=sequence {maho_tui::process_stdio::stdout_write(&sequence);}self.show_status("Copied selected message to clipboard".into());}
                    Err(error)=>self.show_status(error),
                }
            }else{self.show_status("Selected entry has no text to copy".into());}
        }
        let text = self.submissions.borrow_mut().pop_front();
        let text = text?;
        self.editor.editor.add_to_history(&text);
        let images = {
            let mut queued = self.submission_images.borrow_mut();
            queued.iter().position(|(submitted, _)| submitted == &text).and_then(|index| queued.remove(index)).map(|(_, images)| images)
        };
        let options = PromptOptions { images, streaming_behavior: Some(maho_ext_api::StreamingBehavior::Steer), ..Default::default() };
        Some((text, options))
    }

    /// senpi's `onSubmit` for direct callers: initiate the next queued submission and await the turn
    /// to completion. The production loop uses `dispatch_submission` instead.
    pub async fn submit_editor(&mut self) -> Result<Option<PromptDisposition>, String> {
        let Some((text, options)) = self.next_submission().await else { return Ok(None); };
        self.submit(&text, options).await.map(Some)
    }

    /// senpi's main-loop `onSubmit` for the production loop: initiate the next queued submission
    /// without awaiting the turn. `poll_turn` advances the turn on later frames so rendering, input
    /// pumping and session-event consumption keep running while the turn is in flight.
    pub async fn dispatch_submission(&mut self) -> Result<Option<PromptDisposition>, String> {
        let Some((text, options)) = self.next_submission().await else { return Ok(None); };
        self.submit_start(&text, options).await.map(Some)
    }

    /// Queue a submission for the production loop to start (senpi's `onSubmit` capture). Startup
    /// uses this to hand the initial prompt(s) to the loop instead of awaiting a turn before the
    /// first frame; order is the queue order and images are paired with their text for
    /// `next_submission`.
    pub fn enqueue_submission(&mut self, text: &str, images: Option<Vec<maho_ai::types::ImageContent>>) {
        let text = text.trim();
        if text.is_empty() { return; }
        if let Some(images) = images.filter(|images| !images.is_empty()) {
            self.submission_images.borrow_mut().push_back((text.to_owned(), images));
        }
        self.submissions.borrow_mut().push_back(text.to_owned());
    }

    /// Pair an editor marker with its in-memory image payload.
    pub fn attach_image(&mut self, image: maho_ai::types::ImageContent) {
        let id = self.editor.editor.insert_image_marker();
        self.pending_images.borrow_mut().insert(id, image);
    }

    /// senpi's `handleClipboardPaste`: attach a clipboard bitmap behind an atomic marker, else fall
    /// through to the plain-text clipboard path.
    pub async fn handle_clipboard_paste(&mut self) {
        if let Some((bytes, mime_type)) = crate::interactive_clipboard::read_image().await
            && self.attach_clipboard_image(bytes, mime_type)
        {
            return;
        }
        if let Some(text) = crate::interactive_clipboard::read_text().await {
            self.editor.editor.insert_text_at_cursor(&text);
        }
    }

    /// senpi's `attachClipboardImage`: the `blockImages` gate, the compaction drop, and the marker
    /// pair. senpi's `processImage` resize/conversion is skipped - the Rust port has no image codec
    /// (see `components/tool_execution_images.rs`) - so only an already-supported format reaches here.
    fn attach_clipboard_image(&mut self, bytes: Vec<u8>, mime_type: String) -> bool {
        if self.session.with_settings_manager(|settings| settings.get_bool("blockImages").unwrap_or(false)) {
            self.show_status("Image paste blocked by the images.blockImages setting".into());
            return false;
        }
        if self.host_is_compacting() {
            self.show_status("Image paste dropped: messages sent during compaction cannot carry images - paste again after compaction finishes".into());
            return true;
        }
        let data = { use base64::Engine as _; base64::engine::general_purpose::STANDARD.encode(&bytes) };
        self.attach_image(maho_ai::types::ImageContent { data, mime_type });
        self.show_status("Attached image from clipboard".into());
        true
    }

    pub async fn handle_runtime_input(&mut self, data: &str, now_ms: u64) -> Result<(), String> {
        let Some(data) = self.filter_terminal_input(data) else { return Ok(()); };
        let data = data.as_str();
        if self.share.is_some() || self.ui_dialog.is_some() || self.rename_input.is_some() || (self.question.is_some() && self.async_question_widget.is_none()) { self.handle_filtered_input_at(data, now_ms); return Ok(()); }
        let keys = self.keybindings();
        if self.editor.on_extension_shortcut.as_mut().is_some_and(|shortcut| shortcut(data)) { return Ok(()); }
        if keys.matches(data, "app.model.cycleForward") || keys.matches(data, "app.model.cycleBackward") {
            if let Some(name) = self.host_cycle_model(keys.matches(data, "app.model.cycleForward")).await? {
                self.show_status(format!("Switched to {name}"));
                let model = self.host_model();
                self.show_risky_main_model_warning(&model);
                self.maybe_warn_about_anthropic_subscription_auth(model);
            }
            else { self.show_status("No other models available for cycling".into()); }
            return Ok(());
        }
        if keys.matches(data,"app.message.copy") {
            // senpi `handleCopyCommand({ flashConfirmation: true })`: the fullscreen UI confirms with
            // `ui.flash("Copied!")` instead of a status line.
            let renderer = self.mounted_renderer.as_ref().and_then(std::rc::Weak::upgrade).filter(|renderer| matches!(&*renderer.borrow(), crate::tui_renderer::InteractiveTui::Fullscreen(_)));
            if let Some(renderer) = renderer && let Some(text) = self.session.get_last_assistant_text().filter(|text|!text.is_empty()) {
                let sequence = crate::interactive_clipboard::copy(&text).await?;
                if let Some(sequence) = sequence { maho_tui::process_stdio::stdout_write(&sequence); }
                renderer.borrow_mut().flash("Copied!", now_ms);
                return Ok(());
            }
            self.submit("/copy",Default::default()).await?;return Ok(());
        }
        if keys.matches(data,"app.clipboard.pasteImage") {
            // senpi `onPasteImage` -> `handleClipboardPaste` (image first, text fallback); a
            // replacement editor has no image markers, so it keeps the plain-text paste.
            if let Some(editor)=&mut self.custom_editor {
                if let Some(text)=crate::interactive_clipboard::read_text().await { editor.insert_text_at_cursor(&text); }
            } else { self.handle_clipboard_paste().await; }
            return Ok(());
        }
        if keys.matches(data, "app.history.search") {
            // senpi `app.history.search`: `session.prompt("/history")` opens the history-search extension.
            if let Err(error) = self.submit_start("/history", Default::default()).await { self.show_error(&format!("Failed to open history search: {error}")); }
            return Ok(());
        }
        if keys.matches(data, "app.message.followUp") {
            if let Err(error) = self.handle_follow_up().await { self.show_error(&error); }
            return Ok(());
        }
        self.handle_filtered_input_at(data, now_ms);
        Ok(())
    }

    /// The pending terminal hand-off, consumed by the loop that owns the terminal.
    pub fn take_terminal_request(&mut self) -> Option<TerminalRequest> { self.terminal_request.take() }

    /// senpi `getExpandedEditorText` for the active editor.
    fn expanded_editor_text(&self) -> String {
        match &self.custom_editor { Some(editor) => editor.get_expanded_text().unwrap_or_else(|| editor.get_text()), None => self.editor.editor.get_expanded_text() }
    }

    fn set_active_editor_text(&mut self, text: &str) {
        match &mut self.custom_editor { Some(editor) => editor.set_text(text), None => self.editor.editor.set_text(text) }
    }

    /// senpi `takeSubmissionImages`: the attachments behind the text's `[Image #N]` markers, in
    /// marker order; the pending payloads are cleared.
    fn take_submission_images(&mut self, text: &str) -> Vec<maho_ai::types::ImageContent> {
        let mut payloads = std::mem::take(&mut *self.pending_images.borrow_mut());
        maho_tui::image_markers::IMAGE_MARKER_REGEX.captures_iter(text)
            .filter_map(|capture| capture.get(1).and_then(|id| id.as_str().parse::<u64>().ok()))
            .filter_map(|id| payloads.remove(&id)).collect()
    }

    /// senpi `handleFollowUp` (Alt+Enter): queue a follow-up while streaming, queue it for after
    /// compaction, or behave like Enter when idle. Attachments are resolved before the editor clear.
    async fn handle_follow_up(&mut self) -> Result<(), String> {
        self.questions.set_composer_reply(None);
        let text = self.expanded_editor_text().trim().to_owned();
        if text.is_empty() { return Ok(()); }
        if self.host_is_compacting() {
            if self.is_extension_command(&text) {
                self.editor.editor.add_to_history(&text);
                self.set_active_editor_text("");
                self.submit_start(&text, Default::default()).await?;
                return Ok(());
            }
            // The compaction queue carries text only: drop attachments visibly and strip markers.
            let images = self.take_submission_images(&text);
            let dropped = images.len();
            let plural = if dropped > 1 { "s" } else { "" };
            let queued = if dropped == 0 { text.clone() } else { maho_tui::image_markers::IMAGE_MARKER_REGEX.replace_all(&text, "").trim().to_owned() };
            if queued.is_empty() {
                self.show_status(format!("Dropped {dropped} image{plural}: messages sent during compaction cannot carry images - paste again after compaction finishes"));
                return Ok(());
            }
            self.editor.editor.add_to_history(&queued);
            self.set_active_editor_text("");
            let options = PromptOptions { streaming_behavior: Some(maho_ext_api::StreamingBehavior::FollowUp), ..Default::default() };
            match self.session_host.clone() { Some(host) => host.prompt(queued, options).await?, None => { self.session.prompt(&queued, options).await?; } }
            self.drain_events();
            self.show_status(if dropped > 0 {
                format!("Queued message for after compaction; dropped {dropped} image{plural}: messages sent during compaction cannot carry images - paste again after compaction finishes")
            } else { "Queued message for after compaction".into() });
            return Ok(());
        }
        let images = self.take_submission_images(&text);
        if self.host_is_streaming() || self.pending_turn.is_some() {
            self.editor.editor.add_to_history(&text);
            self.set_active_editor_text("");
            let options = if self.is_extension_command(&text) { PromptOptions::default() } else {
                PromptOptions { images: (!images.is_empty()).then_some(images), streaming_behavior: Some(maho_ext_api::StreamingBehavior::FollowUp), ..Default::default() }
            };
            match self.session_host.clone() { Some(host) => host.prompt(text, options).await?, None => { self.session.prompt(&text, options).await?; } }
            self.drain_events();
            return Ok(());
        }
        // Not streaming: Alt+Enter acts like Enter, with the pre-resolved images paired to the text.
        if !images.is_empty() { self.submission_images.borrow_mut().push_back((text.clone(), images)); }
        self.submissions.borrow_mut().push_back(text);
        self.set_active_editor_text("");
        Ok(())
    }

    /// senpi `handleOpenExternalEditor`, run by the terminal owner while the terminal is stopped.
    pub async fn open_external_editor(&mut self) {
        let configured = self.session.with_settings_manager(|settings| settings.get_string("externalEditor")).filter(|command| !command.trim().is_empty());
        let command = configured
            .or_else(|| std::env::var("VISUAL").ok().filter(|value| !value.is_empty()))
            .or_else(|| std::env::var("EDITOR").ok().filter(|value| !value.is_empty()))
            .unwrap_or_else(|| if cfg!(windows) { "notepad".into() } else { "nano".into() });
        let content = self.expanded_editor_text();
        match crate::external_editor::edit_in_external_editor(&command, &content).await {
            Ok(crate::external_editor::ExternalEditorResult::Complete { content }) => self.set_active_editor_text(&content),
            Ok(_) => {}
            Err(error) => self.show_error(&format!("Failed to open external editor: {error}")),
        }
    }

    /// senpi `defaultEditor.onEscape` with its `compaction_start` override: abort compaction, abort
    /// the turn (also between retry attempts), abort a running `!` command, clear a `!` draft, or
    /// run the double-escape `doubleEscapeAction` (`/tree`, `/fork`, or nothing) on an empty editor.
    fn handle_escape(&mut self, now_ms: u64) {
        if self.host_is_compacting() {
            match self.session_host.clone() {
                Some(host) => { tokio::spawn(async move { let _ = host.abort_compaction().await; }); }
                None => self.session.abort_compaction(),
            }
            return;
        }
        let (retrying, bash_running) = match self.remote_state_snapshot() {
            Some(state) => (state.retry_attempt > 0.0, state.is_bash_running),
            None => (self.session.retry_attempt() > 0, self.session.is_bash_running() || self.pending_bash.is_some()),
        };
        if !self.agent_idle || self.host_is_streaming() || retrying { self.abort_and_restore_queue(); return; }
        if bash_running {
            match self.session_host.clone() {
                Some(host) => { tokio::spawn(async move { let _ = host.abort_bash().await; }); }
                None => self.session.abort_bash(),
            }
            return;
        }
        let text = self.custom_editor.as_ref().map_or_else(|| self.editor.editor.get_text(), |editor| editor.get_text());
        if text.trim_start().starts_with('!') {
            match &mut self.custom_editor { Some(editor) => editor.set_text(""), None => self.editor.editor.set_text("") }
            return;
        }
        if !text.trim().is_empty() { return; }
        let action = self.session.with_settings_manager(|settings| settings.get_string("doubleEscapeAction")).unwrap_or_else(|| "tree".into());
        if action == "none" { return; }
        if self.last_escape_ms.is_some_and(|last| now_ms.saturating_sub(last) < 500) {
            self.last_escape_ms = None;
            if let Err(error) = self.dispatch_command(if action == "tree" { "/tree" } else { "/fork" }) { self.show_error(&error); }
        } else { self.last_escape_ms = Some(now_ms); }
    }

    pub fn set_tools_expanded(&mut self, expanded: bool) {
        self.extension_ui.tools_expanded.store(expanded, std::sync::atomic::Ordering::Relaxed);
        if self.tools_expanded == expanded { return; }
        self.tools_expanded = expanded;
        // senpi `setToolsExpanded`: the active header follows the expansion.
        if self.custom_header.is_none() && let Some(expandable) = &self.built_in_expandable { expandable.borrow_mut().set_expanded(expanded); }
        for section in &self.loaded_resource_sections { section.borrow_mut().set_expanded(expanded); }
        for component in &self.tool_cards { component.borrow_mut().set_expanded(expanded); }
        for component in &self.assistant_cards { component.borrow_mut().set_expanded(expanded); }
        for update in &mut self.history_expansion { update(expanded); }
        self.show_status(format!("Tool output: {}", if expanded { "expanded" } else { "collapsed" }));
    }

    /// senpi's main-loop step: initiate the next queued submission (starting a turn or routing it
    /// into a live one), then advance the in-flight turn one step. A submission error surfaces as a
    /// status line (senpi's `showError`) instead of tearing the loop down.
    pub async fn pump_turn(&mut self) { self.pump_turn_with(std::task::Waker::noop()).await; }

    /// `pump_turn` with an explicit waker so an event-driven caller can re-poll exactly when the
    /// in-flight turn (or abort future) wakes, instead of on a fixed cadence.
    pub async fn pump_turn_with(&mut self, waker: &std::task::Waker) {
        if let Err(error) = self.dispatch_submission().await { self.show_error(&error); }
        self.poll_turn_with(waker);
    }

    /// senpi's awaited `session.prompt` for direct callers: start the turn, then wait it out.
    pub async fn submit(&mut self, text: &str, options: PromptOptions) -> Result<PromptDisposition, String> {
        let had_turn = self.pending_turn.is_some();
        let disposition = self.submit_start(text, options).await?;
        if self.pending_bash.is_some() { self.await_bash().await; }
        if !had_turn && self.pending_turn.is_some() { self.await_turn().await; }
        Ok(disposition)
    }

    /// Await the local `!` command for direct `submit` callers, streaming its output into its card.
    async fn await_bash(&mut self) {
        let Some((component, mut execution)) = self.pending_bash.take() else { return; };
        let result = loop { tokio::select! {
            result = &mut execution => break result,
            Some(event) = self.events.recv() => self.route_bash_event(&component, &event),
            Some(request) = self.ui_requests.recv() => self.handle_ui_request(request),
        }};
        while let Ok(event) = self.events.try_recv() { self.route_bash_event(&component, &event); }
        self.finish_bash(component, result);
    }

    fn route_bash_event(&mut self, component: &Rc<RefCell<crate::components::bash_execution::BashExecutionComponent>>, event: &maho_ext_api::AgentSessionEvent) {
        if let maho_ext_api::AgentSessionEvent::BashExecutionUpdate { delta, .. } = event { component.borrow_mut().append_output(delta); } else { self.handle_session_event(event); }
    }

    fn finish_bash(&mut self, component: Rc<RefCell<crate::components::bash_execution::BashExecutionComponent>>, result: Result<maho_tools::bash_executor::BashResult, String>) {
        match result {
            Ok(result) => component.borrow_mut().set_complete(result.exit_code, result.cancelled, None, result.full_output_path.map(|path| path.to_string_lossy().into_owned())),
            Err(error) => { component.borrow_mut().append_output(&error); component.borrow_mut().set_complete(Some(1), false, None, None); self.show_status(error); }
        }
        self.history_expansion.push(Box::new(move |expanded| component.borrow_mut().set_expanded(expanded)));
    }

    /// senpi `flushPendingBashComponents`: move deferred `!` cards into the chat.
    fn flush_pending_bash_components(&mut self) {
        for component in std::mem::take(&mut self.pending_bash_components) { self.chat.add_component(component); }
    }

    /// senpi `isExtensionCommand`.
    fn is_extension_command(&self, text: &str) -> bool {
        let commands = self.session.command_groups().0.into_iter().map(|command| command.name).collect::<Vec<_>>();
        is_extension_command_text(text, &commands)
    }

    /// senpi `queueCompactionSubmission`: the compaction queue carries text only, so attachments
    /// are dropped visibly and their markers stripped.
    fn queue_compaction_submission(&mut self, text: &str, images: Option<Vec<maho_ai::types::ImageContent>>) {
        let count = images.map_or(0, |images| images.len());
        if count == 0 { self.queue_compaction_message(text, 0); return; }
        let queued = maho_tui::image_markers::IMAGE_MARKER_REGEX.replace_all(text, "").trim().to_owned();
        if !queued.is_empty() { self.queue_compaction_message(&queued, count); return; }
        self.show_status(format!("Dropped {count} image{}: messages sent during compaction cannot carry images - paste again after compaction finishes", if count > 1 { "s" } else { "" }));
    }

    /// senpi `queueCompactionMessage`.
    fn queue_compaction_message(&mut self, text: &str, dropped_image_count: usize) {
        let enqueue_order = self.session.reserve_queued_input_order();
        self.compaction_queue.borrow_mut().queued.push(crate::compaction_queue_transfer::CompactionQueuedMessage { text: text.to_owned(), mode: maho_ext_api::StreamingBehavior::Steer, enqueue_order: Some(enqueue_order), pending_echo_id: None });
        self.editor.editor.add_to_history(text);
        self.editor.editor.set_text("");
        self.show_status(if dropped_image_count > 0 {
            format!("Queued message for after compaction; dropped {dropped_image_count} image{}: messages sent during compaction cannot carry images - paste again after compaction finishes", if dropped_image_count > 1 { "s" } else { "" })
        } else { "Queued message for after compaction".into() });
    }

    /// senpi `flushCompactionQueue`: transfers run one after another (senpi chains them on a tail).
    fn start_compaction_flush(&mut self, options: crate::compaction_queue_transfer::TransferOptions) {
        if self.pending_compaction_flush.is_some() { self.queued_compaction_flushes.push_back(options); return; }
        let dependencies = CompactionQueueTransfer {
            state: self.compaction_queue.clone(), session: self.session.clone(), host: self.session_host.clone(),
            extension_commands: self.session.command_groups().0.into_iter().map(|command| command.name).collect(),
            tasks: tokio::task::JoinSet::new(), events: self.events_sender.clone(), failure: None,
        };
        self.pending_compaction_flush = Some(Box::pin(async move {
            let mut dependencies = dependencies;
            crate::compaction_queue_transfer::transfer_compaction_queue(&mut dependencies, options).await;
            dependencies.tasks.detach_all();
            dependencies.failure
        }));
    }

    /// Start the turn for `text` and return, or route it into a turn that is already in flight.
    async fn submit_start(&mut self, text: &str, options: PromptOptions) -> Result<PromptDisposition, String> {
        self.refresh_tool_renderer_snapshots().await;
        if !text.trim_start().starts_with('/') && !text.trim_start().starts_with('!') && let Some(request_id) = self.questions.composer_request_id().map(str::to_owned) {
            let response = self.questions.get(&request_id).map(|entry| to_extension_response(crate::components::ask_user_async_widget::build_comment_response(&entry.request, &entry.draft, text.trim())));
            if let Some(response) = response {
                self.finish_question(&request_id, response);
                self.editor.editor.set_text("");
                return Ok(PromptDisposition::Handled);
            }
        }
        if text.trim() == "/copy" {
            if let Some(text) = self.session.get_last_assistant_text().filter(|text|!text.is_empty()) {
                match crate::interactive_clipboard::copy(&text).await {
                    Ok(sequence) => {
                        if let Some(sequence) = sequence { maho_tui::process_stdio::stdout_write(&sequence); }
                        self.show_status("Copied last agent message to clipboard".into());
                    }
                    Err(error) => { self.show_error(&error); return Ok(PromptDisposition::Handled); }
                }
            } else { self.show_error("No agent messages to copy yet."); }
            return Ok(PromptDisposition::Handled);
        }
        if text.trim() == "/keybindings" {
            let config_path = std::path::Path::new(&self.session.agent_dir()).join("keybindings.json");
            let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
            let Some(editor_command) = crate::keybindings_command::resolve_editor_command(&env) else {
                return Err(format!("Set $EDITOR or $VISUAL to edit {}.", config_path.display()));
            };
            use crate::keybindings_command::KeybindingsEditOutcome;
            let mut keybindings = maho_core::keybindings::KeybindingsManager::create(Some(&self.session.agent_dir()));
            match crate::keybindings_command::run_keybindings_edit(&config_path, &editor_command, &mut keybindings).await {
                KeybindingsEditOutcome::Reloaded => {
                    let manager = maho_core::keybindings::KeybindingsManager::create(Some(&self.session.agent_dir())).inner().clone();
                    maho_tui::keybindings::set_keybindings(manager.clone());
                    self.editor.set_keybindings(Arc::new(manager));
                    self.show_status("Keybindings reloaded".into());
                }
                KeybindingsEditOutcome::LaunchFailed { seeded } => {
                    if seeded { let _ = std::fs::remove_file(&config_path); }
                    self.show_status(format!("Could not open {} with \"{editor_command}\".", config_path.display()));
                }
                KeybindingsEditOutcome::Exited { code } => self.show_status(format!("\"{editor_command}\" exited with code {code}; keybindings were not reloaded.")),
                KeybindingsEditOutcome::Invalid { message } => self.show_status(format!("Keybindings not reloaded - {} is not valid JSON: {message}", config_path.display())),
                KeybindingsEditOutcome::IoError { message } => self.show_status(format!("Could not open {} with \"{editor_command}\": {message}", config_path.display())),
            }
            return Ok(PromptDisposition::Handled);
        }
        if text.trim().starts_with("/import-confirm ") {
            let path = get_path_command_argument(text.trim(), "/import-confirm").ok_or("Usage: /import <path.jsonl>")?;
            self.handle_import_session(path, None).await?;
            return Ok(PromptDisposition::Handled);
        }
        if text.trim().starts_with("/import-cwd ") {
            let path = get_path_command_argument(text.trim(), "/import-cwd").ok_or("Usage: /import <path.jsonl>")?;
            let cwd = self.host_cwd();
            self.handle_import_session(path, Some(cwd)).await?;
            return Ok(PromptDisposition::Handled);
        }
        if text.trim() == "/import-cancel" { self.show_status("Import cancelled".into()); return Ok(PromptDisposition::Handled); }
        if let Some(command) = text.strip_prefix('!').map(|command| command.strip_prefix('!').map_or((command.trim(), false), |command| (command.trim(), true))).filter(|(command, _)| !command.is_empty()) {
            let (command, excluded) = command;
            if self.pending_bash.is_some() || (self.session_host.is_none() && self.session.is_bash_running()) {
                self.show_warning("A bash command is already running. Press Esc to cancel it first.");
                self.editor.editor.set_text(text);
                return Ok(PromptDisposition::Handled);
            }
            let component = Rc::new(RefCell::new(crate::components::bash_execution::BashExecutionComponent::new(command, excluded, self.theme.clone())));
            component.borrow_mut().set_expanded(self.tools_expanded); self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(component.clone()));
            component.borrow_mut().set_expanded(self.tools_expanded);
            // senpi shows a `!` started while streaming in the pending area until the next submission.
            if self.host_is_streaming() { self.pending_bash_components.push(component.clone()); } else { self.chat.add_component(component.clone()); }
            if let Some(host) = self.session_host.clone() {
                let result = match host.execute_bash(command.to_owned(), excluded).await { Ok(result) => result, Err(error) => { component.borrow_mut().append_output(&error); component.borrow_mut().set_complete(Some(1), false, None, None); return Err(error); } };
                let exit_code = result.get("exitCode").and_then(serde_json::Value::as_i64).map(|code| code as i32);
                let cancelled = result.get("cancelled").and_then(serde_json::Value::as_bool).unwrap_or(false);
                let full_output_path = result.get("fullOutputPath").and_then(serde_json::Value::as_str).map(str::to_owned);
                component.borrow_mut().set_complete(exit_code, cancelled, None, full_output_path);
                self.history_expansion.push(Box::new(move |expanded| component.borrow_mut().set_expanded(expanded)));
                return Ok(PromptDisposition::Handled);
            }
            // Run without blocking the frame loop: `poll_turn` drives it and streams its output.
            let session = self.session.clone();
            let command = command.to_owned();
            self.pending_bash = Some((component, Box::pin(async move { session.execute_bash(&command, None, excluded, None, None).await })));
            return Ok(PromptDisposition::Handled);
        }
        if text.trim() == "/paste-clipboard" {
            self.handle_clipboard_paste().await;
            return Ok(PromptDisposition::Handled);
        }
        if text.trim() == "/reload" {
            // senpi `handleReloadCommand`: never reload under a live response or compaction.
            if self.host_is_streaming() { self.show_warning("Wait for the current response to finish before reloading."); return Ok(PromptDisposition::Handled); }
            if self.host_is_compacting() { self.show_warning("Wait for compaction to finish before reloading."); return Ok(PromptDisposition::Handled); }
            // Pre-check the extension veto so a blocked reload warns before anything is torn down;
            // `reload` re-checks internally. A mounted host answers the veto inside its own reload.
            if self.session_host.is_none() {
                match self.session.check_reload_veto().await {
                    Ok(true) => { self.show_warning("Reload blocked by an extension."); return Ok(PromptDisposition::Handled); }
                    Ok(false) => {}
                    Err(error) => { self.show_error(&format!("Reload failed: {error}")); return Ok(PromptDisposition::Handled); }
                }
            }
            match self.host_reload().await {
                Ok(true) => {
                    // senpi `resetExtensionUI` once the reload proceeds: the reloaded extensions
                    // re-register their autocomplete wrappers (queued UI requests, drained afterwards).
                    self.autocomplete_wrappers.clear();
                    let (padding, max_visible) = self.session.with_settings_manager(|settings| (settings.get_number("editorPaddingX").unwrap_or(0.0), settings.get_number("autocompleteMaxVisible").unwrap_or(10.0)));
                    self.editor.set_padding_x(padding as usize); self.editor.editor.set_autocomplete_max_visible(max_visible as usize);
                    self.rebuild_history();
                    // `this.keybindings.reload()` and `themeController.applyFromSettings()`.
                    // senpi reload: `keybindings.reload()` then `setupExtensionShortcuts(runner)`.
                    let manager = maho_core::keybindings::KeybindingsManager::create(Some(&self.session.agent_dir())).inner().clone();
                    maho_tui::keybindings::set_keybindings(manager.clone());
                    self.editor.set_keybindings(Arc::new(manager));
                    self.setup_extension_shortcuts().await;
                    if let Some(theme) = self.session.with_settings_manager(|settings| settings.get_string("theme")) { self.handle_ui_request(crate::interactive_extension_ui::UiRequest::SettingChanged("theme".into(), serde_json::json!(theme))); }
                    self.setup_autocomplete(); self.refresh_tool_renderer_snapshots().await; self.show_loaded_resources(false, true);
                    if let Some(error) = self.session.model_registry().get_error() { self.show_error(&format!("models.json error: {error}")); }
                    self.show_status("Reloaded keybindings, extensions, skills, prompts, themes, and context files".into());
                }
                Ok(false) => self.show_warning("Reload blocked by an extension."),
                Err(error) => self.show_error(&format!("Reload failed: {error}")),
            }
            return Ok(PromptDisposition::Handled);
        }
        if text.trim() == "/compact" || text.trim().starts_with("/compact ") {
            // senpi `handleCompactCommand`: fewer than two message entries is "no messages yet" - a
            // warning with no core call, not the core "session too small" error.
            let message_count = self.session.with_session_manager(|manager| manager.entries().iter().filter(|entry| entry.get("type").and_then(serde_json::Value::as_str) == Some("message")).count());
            if message_count < 2 { self.show_status("Nothing to compact (no messages yet)".into()); return Ok(PromptDisposition::Handled); }
            if self.session_host.is_some() {
                self.host_compact(text.trim().strip_prefix("/compact ").map(str::to_owned)).await?;
                self.rebuild_history();
                self.show_status("Compacted context".into());
                return Ok(PromptDisposition::Handled);
            }
            let session = self.session.clone();
            let compact = session.compact(text.trim().strip_prefix("/compact "));
            tokio::pin!(compact);
            let result = loop { tokio::select! {
                result = &mut compact => break result,
                Some(request) = self.ui_requests.recv() => self.handle_ui_request(request),
                Some(event) = self.events.recv() => self.handle_session_event(&event),
            }}?;
            self.rebuild_history();
            self.show_status(format!("Compacted from {} tokens", result.tokens_before));
            return Ok(PromptDisposition::Handled);
        }
        if text.trim() == "/new" {
            if self.host_new_session(None).await? {
                self.rebuild_history();
                // senpi `handleClearCommand`: the accent confirmation, not a dim status.
                self.chat.add_component(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1))));
                self.chat.add_component(Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding(self.theme.fg(crate::theme::ThemeColor::Accent, "✓ New session started"), 1, 1))));
            }
            return Ok(PromptDisposition::Handled);
        }
        if text.trim() == "/clone" {
            let leaf = self.session.with_session_manager(|manager| manager.leaf_id().map(str::to_owned));
            if let Some(leaf) = leaf {
                let result = self.host_fork(leaf, true).await?;
                if !result.cancelled { self.rebuild_history(); self.editor.editor.set_text(""); self.show_status("Cloned to new session".into()); }
            } else { self.show_status("Nothing to clone yet".into()); }
            return Ok(PromptDisposition::Handled);
        }
        if text.trim().starts_with("/resume ") {
            let path = get_path_command_argument(text.trim(), "/resume").ok_or("Missing session path")?;
            self.handle_resume_session(path, None).await?;
            return Ok(PromptDisposition::Handled);
        }
        if text.trim().starts_with("/resume-cwd ") {
            let path = get_path_command_argument(text.trim(), "/resume-cwd").ok_or("Missing session path")?;
            let cwd = self.host_cwd();
            self.handle_resume_session(path, Some(cwd)).await?;
            return Ok(PromptDisposition::Handled);
        }
        if text.trim() == "/resume-cwd-cancel" { self.show_status("Resume cancelled".into()); return Ok(PromptDisposition::Handled); }
        if let Some(id) = text.trim().strip_prefix("/fork ") {
            let result = self.host_fork(id.trim().to_owned(), false).await?;
            if !result.cancelled { self.rebuild_history(); self.editor.editor.set_text(result.editor_text.as_deref().unwrap_or("")); self.show_status("Forked to new session".into()); }
            return Ok(PromptDisposition::Handled);
        }
        if text.trim().starts_with("/tree-edit ") {
            let id = text.trim().strip_prefix("/tree-edit ").unwrap_or_default().trim().to_owned();
            self.open_tree_edit(&id);
            return Ok(PromptDisposition::Handled);
        }
        if text.trim() == "/tree-edit-apply" {
            self.apply_tree_edit().await?;
            return Ok(PromptDisposition::Handled);
        }
        if let Some(id) = text.trim().strip_prefix("/tree ") {
            let id = id.trim().to_owned();
            if self.session.with_session_manager(|manager| manager.leaf_id().is_some_and(|leaf| leaf == id.as_str())) {
                self.show_status("Already at this point".into()); return Ok(PromptDisposition::Handled);
            }
            // senpi `runTreeNavigation(entryId, { promptForSummary: true, ... })`.
            if self.tree_summary_skip_prompt() { self.run_tree_navigation(&id, false, None).await?; }
            else { self.prompt_branch_summary_choice(&id); }
            return Ok(PromptDisposition::Handled);
        }
        if let Some(id) = text.trim().strip_prefix("/tree-summary ") {
            self.prompt_branch_summary_choice(id.trim());
            return Ok(PromptDisposition::Handled);
        }
        if let Some(id) = text.trim().strip_prefix("/tree-summary-custom ") {
            self.open_branch_summary_custom_prompt(id.trim());
            return Ok(PromptDisposition::Handled);
        }
        if text.trim() == "/tree-navigate" {
            self.apply_tree_navigation().await?;
            return Ok(PromptDisposition::Handled);
        }
        if let Some(reference) = text.trim().strip_prefix("/model ") {
            let (provider, id) = reference.trim().split_once('/').ok_or("Model reference requires provider/model")?;
            let model = self.host_find_model(provider, id).ok_or_else(|| format!("Model not found: {reference}"))?;
            let requested = model.clone();
            self.host_set_model(provider, id).await?;
            if maho_ai::models::models_are_equal(Some(&self.host_model()), Some(&requested)) {
                self.show_status(format!("Switched to {}", self.host_model().name));
                self.show_risky_main_model_warning(&requested);
                self.maybe_warn_about_anthropic_subscription_auth(requested);
            }
            else { self.show_status(format!("Model switch pending: {reference}")); }
        if let Some(reference) = text.trim().strip_prefix("/model-select ") {
            // The selectors' `onSelect`: senpi calls `selectModelFromUi` with the chosen model directly.
            let Some((provider, id)) = reference.trim().split_once('/') else { return Ok(PromptDisposition::Handled); };
            match self.host_find_model(provider, id) {
                Some(model) => self.select_model_from_ui(model).await,
                None => self.show_error(&format!("Model not found: {provider}/{id}")),
            }
            return Ok(PromptDisposition::Handled);
        }
        if let Some(search_term) = text.trim().strip_prefix("/model ") {
            // senpi `handleModelCommand`: an exact match switches, a miss opens the pre-filtered selector.
            let search_term = search_term.trim().to_owned();
            match self.find_exact_model_match(&search_term).await {
                Some(model) => self.select_model_from_ui(model).await,
                None => { self.dispatch_command(&format!("/model-search {search_term}"))?; }
            }
            return Ok(PromptDisposition::Handled);
        }
        if text.trim() == "/export" || text.trim().starts_with("/export ") {
            // senpi `handleExportCommand`: `.jsonl` exports JSONL, anything else (or no path) HTML.
            let path = get_path_command_argument(text.trim(), "/export");
            let result = if path.as_deref().is_some_and(|path| path.ends_with(".jsonl")) {
                self.host_export_jsonl(path.as_deref()).await
            } else {
                let theme_name = self.theme.name.clone();
                self.host_export_html(path.as_deref(), &theme_name).await
            };
            match result {
                Ok(exported) => self.show_status(format!("Session exported to: {}", exported.or(path).unwrap_or_default())),
                Err(error) => self.show_error(&format!("Failed to export session: {error}")),
            }
            return Ok(PromptDisposition::Handled);
        }
        if text.trim() == "/share" {
            self.handle_share_command().await;
            self.editor.editor.set_text("");
            return Ok(PromptDisposition::Handled);
        }
        if let Some(name) = text.trim().strip_prefix("/rename ").or_else(|| text.trim().strip_prefix("/name ")) {
            let name = name.trim();
            if name.is_empty() { self.show_warning("Session name cannot be empty"); return Ok(PromptDisposition::Handled); }
            self.apply_session_name(name).await;
            return Ok(PromptDisposition::Handled);
        }
        if text.trim() == "/logout" { self.show_logout_selector().await; return Ok(PromptDisposition::Handled); }
        if let Some(provider_id) = text.trim().strip_prefix("/logout-provider ") { self.logout_provider(provider_id.trim()).await; return Ok(PromptDisposition::Handled); }
        if self.dispatch_command(text)? { return Ok(PromptDisposition::Handled); }
        // senpi queues non-command input during compaction for delivery after it settles.
        if self.host_is_compacting() && !self.is_extension_command(text.trim()) {
            self.queue_compaction_submission(text.trim(), options.images.clone());
            return Ok(PromptDisposition::Handled);
        }
        self.flush_pending_bash_components();
        if self.pending_turn.is_some() {
            // A turn is already in flight: hand the submission to the session's steer/follow-up
            // queue (the pinned `streamingBehavior` path) instead of replacing the running turn.
            let result = match self.session_host.clone() {
                Some(host) => host.prompt(text.to_owned(), options).await.map(|_| PromptDisposition::Started),
                None => self.session.prompt(text, options).await,
            };
            self.drain_events();
            return result;
        }
        self.start_turn(text.to_owned(), options);
        Ok(PromptDisposition::Started)
    }

    /// Initiate a provider turn without waiting for it to finish.
    fn start_turn(&mut self, text: String, options: PromptOptions) {
        let turn: TurnFuture = match self.session_host.clone() {
            Some(host) => Box::pin(async move { host.prompt(text, options).await.map(|_| PromptDisposition::Started) }),
            None => { let session = self.session.clone(); Box::pin(async move { session.prompt(&text, options).await }) }
        };
        self.pending_turn = Some(turn);
    }

    /// Advance the in-flight abort request and turn one step so a running provider keeps producing
    /// session events, then consume those events. Never blocks: a held provider leaves the turn
    /// pending and the caller keeps rendering. The abort is polled first so its signal is set before
    /// the run loop is advanced (an abort must beat the held delta). `waker` receives the turn's and
    /// abort's wakeups, so an event-driven caller re-polls exactly when there is progress; the
    /// production loop passes `Waker::noop()` and re-polls every frame.
    pub fn poll_turn_with(&mut self, waker: &std::task::Waker) {
        let mut context = std::task::Context::from_waker(waker);
        let abort_done = self.pending_abort.as_mut().is_some_and(|abort| abort.as_mut().poll(&mut context).is_ready());
        if abort_done { self.pending_abort = None; }
        let turn_result = self.pending_turn.as_mut().and_then(|turn| match turn.as_mut().poll(&mut context) {
            std::task::Poll::Pending => None,
            std::task::Poll::Ready(result) => Some(result),
        });
        if let Some(result) = turn_result {
            self.pending_turn = None;
            if let Err(error) = result { self.show_status(error); }
        }
        let bash_result = self.pending_bash.as_mut().and_then(|(_, execution)| match execution.as_mut().poll(&mut context) {
            std::task::Poll::Pending => None,
            std::task::Poll::Ready(result) => Some(result),
        });
        if let Some(result) = bash_result && let Some((component, _)) = self.pending_bash.take() {
            while let Ok(event) = self.events.try_recv() { self.route_bash_event(&component, &event); }
            self.finish_bash(component, result);
        }
        let flush_result = self.pending_compaction_flush.as_mut().and_then(|flush| match flush.as_mut().poll(&mut context) {
            std::task::Poll::Pending => None,
            std::task::Poll::Ready(failure) => Some(failure),
        });
        if let Some(failure) = flush_result {
            self.pending_compaction_flush = None;
            if let Some(failure) = failure { self.show_error(&failure); }
            if let Some(next) = self.queued_compaction_flushes.pop_front() { self.start_compaction_flush(next); }
        }
        self.drain_events();
    }

    /// One production frame step: advance the in-flight turn with a noop waker.
    pub fn poll_turn(&mut self) { self.poll_turn_with(std::task::Waker::noop()); }

    /// Whether a provider turn is currently in flight.
    pub fn has_pending_turn(&self) -> bool { self.pending_turn.is_some() }

    /// Await the in-flight turn to completion, consuming session events while it runs. Direct
    /// `submit` callers use this; the production loop uses `poll_turn` instead.
    async fn await_turn(&mut self) {
        let Some(mut turn) = self.pending_turn.take() else { return; };
        let result = loop {
            let abort_done = self.pending_abort.as_mut().is_some_and(|abort| {
                let mut context = std::task::Context::from_waker(std::task::Waker::noop());
                abort.as_mut().poll(&mut context).is_ready()
            });
            if abort_done { self.pending_abort = None; }
            tokio::select! {
                result = &mut turn => break result,
                Some(event) = self.events.recv() => { self.handle_session_event(&event); }
                Some(request) = self.ui_requests.recv() => self.handle_ui_request(request),
            }
        };
        if let Err(error) = result { self.show_status(error); }
        self.drain_events();
    }

    pub async fn abort(&self) { let _ = self.host_abort().await; }

    /// senpi `restoreQueuedMessagesToEditor`: move every queued input (native and compaction) into
    /// the editor, ahead of the current draft.
    fn restore_queued_messages_to_editor(&mut self, abort_will_follow: bool) -> usize {
        let cleared = self.host_clear_queue(abort_will_follow);
        let queued = cleared.iter().map(|message| message.text.as_str()).collect::<Vec<_>>();
        let count = queued.len();
        if count > 0 {
            let current = self.editor.editor.get_text();
            let queued = queued.join("\n\n");
            self.editor.editor.set_text(&[queued.as_str(), current.as_str()].into_iter().filter(|text| !text.trim().is_empty()).collect::<Vec<_>>().join("\n\n"));
        }
        count
    }

    pub fn restore_queued_messages(&mut self, abort_will_follow: bool) -> usize {
        let count = self.restore_queued_messages_to_editor(abort_will_follow);
        self.show_status(if count == 0 { "No queued messages to restore".into() } else { format!("Restored {count} queued message{} to editor", if count > 1 { "s" } else { "" }) });
        count
    }

    /// senpi's interrupt handler: clear the queued messages back into the editor and *request* the
    /// abort. The abort is not awaited (`void session.abort()`): the run finishes when its own loop
    /// observes the signal, and the production loop must keep polling the turn meanwhile - awaiting
    /// `wait_for_idle` here would deadlock behind a held provider.
    pub fn abort_and_restore_queue(&mut self) -> usize {
        let queued = self.host_clear_queue(true);
        self.request_abort();
        if !queued.is_empty() {
            let text = queued.iter().map(|message| message.text.as_str()).collect::<Vec<_>>().join("\n\n");
            let current = self.editor.editor.get_text();
            self.editor.editor.set_text(&[text.as_str(), current.as_str()].into_iter().filter(|text| !text.trim().is_empty()).collect::<Vec<_>>().join("\n\n"));
        }
        queued.len()
    }

    /// Queue the abort request for `poll_turn` to drive. No `tokio::spawn`, so the mode keeps its
    /// non-`Send` shape.
    fn request_abort(&mut self) {
        let abort: AbortFuture = match self.session_host.clone() {
            Some(host) => Box::pin(async move { let _ = host.abort().await; }),
            None => { let session = self.session.clone(); Box::pin(async move { session.abort().await; }) }
        };
        self.pending_abort = Some(abort);
    }

    pub async fn steer(&self, text: &str) -> Result<(), String> { self.host_steer(text).await }

    pub async fn follow_up(&self, text: &str) -> Result<(), String> { self.host_follow_up(text).await }

    // ---- senpi `editAssistantMessageFromTree` (`interactive-mode.ts:8122`) ------------------

    /// The prefill (assistant content text) and tool-call flag for the tree edit editor, or `None`
    /// when the entry is not an assistant message.
    fn tree_edit_prefill(&self, entry_id: &str) -> Option<(String, bool)> {
        let entry = self.session.with_session_manager(|manager| manager.entry(entry_id))?;
        if entry.get("type").and_then(serde_json::Value::as_str) != Some("message") { return None; }
        let message: maho_ai::types::AssistantMessage = serde_json::from_value(entry.get("message")?.clone()).ok()?;
        let drops_tool_calls = message.content.iter().any(|block| matches!(block, maho_ai::types::ContentBlock::ToolCall(_)));
        Some((maho_ai::utils::text::content_text(&message.content, ""), drops_tool_calls))
    }

    /// senpi's `editAssistantMessageFromTree` head: verify the entry, open the extension editor
    /// prefilled with the assistant text, and capture the leaf the edit is checked against.
    fn open_tree_edit(&mut self, entry_id: &str) {
        let Some((content, drops_tool_calls)) = self.tree_edit_prefill(entry_id) else {
            self.show_status("Only assistant responses can be edited here".into());
            return;
        };
        let title = if drops_tool_calls { "Edit assistant response (its tool calls will be dropped)" } else { "Edit assistant response" };
        let expected_leaf = self.session.with_session_manager(|manager| manager.leaf_id().map(str::to_owned));
        let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
        let pending = self.pending_tree_edit.clone();
        let submit_submissions = self.submissions.clone(); let submit_closed = self.ui_reply.clone();
        let cancel_submissions = self.submissions.clone(); let cancel_closed = self.ui_reply.clone();
        let entry = entry_id.to_owned();
        self.ui_dialog = Some(Box::new(crate::components::extension_editor::ExtensionEditorComponent::new(&self.theme, self.editor_host.clone(),
            Arc::new(self.keybindings()), title, Some(&content),
            Box::new(move |text| { *pending.borrow_mut() = Some((entry.clone(), text.to_owned(), expected_leaf.clone())); submit_submissions.borrow_mut().push_back("/tree-edit-apply".into()); submit_closed.borrow_mut().take(); }),
            Box::new(move || { cancel_submissions.borrow_mut().push_back("/tree".into()); cancel_closed.borrow_mut().take(); }), Default::default(), None, None)));
    }

    /// senpi's `editAssistantMessageFromTree` tail: empty re-opens the tree, an unchanged edit
    /// reports without navigating, and a real edit replaces the assistant message under the leaf
    /// token captured when the editor opened.
    async fn apply_tree_edit(&mut self) -> Result<(), String> {
        let Some((entry_id, edited, expected_leaf)) = self.pending_tree_edit.borrow_mut().take() else { return Ok(()); };
        if edited.trim().is_empty() {
            self.show_status("Assistant response cannot be empty".into());
            self.submissions.borrow_mut().push_back(format!("/tree-edit {entry_id}"));
            return Ok(());
        }
        let options = maho_core::agent_session::TreeNavigationOptions { expected_leaf_id: expected_leaf, ..Default::default() };
        let result = self.host_edit_assistant_message(&entry_id, &edited, options).await?;
        if result.unchanged == Some(true) { self.show_status("Assistant response unchanged".into()); return Ok(()); }
        if result.cancelled { self.show_status("Navigation cancelled".into()); return Ok(()); }
        if result.aborted == Some(true) { self.show_status("Branch summarization cancelled".into()); self.submissions.borrow_mut().push_back(format!("/tree-edit {entry_id}")); return Ok(()); }
        self.rebuild_history();
        self.show_status("Replaced assistant response with your edit".into());
        Ok(())
    }

    /// senpi `getBranchSummarySkipPrompt`: `branchSummary.skipPrompt`.
    fn tree_summary_skip_prompt(&self) -> bool {
        self.session.with_settings_manager(|settings| settings.get_value("branchSummary").and_then(|value| value.get("skipPrompt")).and_then(serde_json::Value::as_bool).unwrap_or(false))
    }

    /// senpi `promptBranchSummaryChoice`: the "Summarize branch?" selector. A custom prompt opens the
    /// instructions editor; cancelling either dialog re-shows the previous step.
    fn prompt_branch_summary_choice(&mut self, entry_id: &str) {
        let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
        let pending = self.pending_tree_nav.clone();
        let selected = self.ui_reply.clone(); let cancelled = self.ui_reply.clone();
        let select_submissions = self.submissions.clone(); let cancel_submissions = self.submissions.clone();
        let entry = entry_id.to_owned(); let select_entry = entry.clone(); let custom_entry = entry.clone();
        let component = crate::components::extension_selector::ExtensionSelectorComponent::new(&self.theme, Arc::new(self.keybindings()), "Summarize branch?", vec!["No summary".into(), "Summarize".into(), "Summarize with custom prompt".into()],
            Box::new(move |choice| {
                selected.borrow_mut().take();
                if choice == "Summarize with custom prompt" { select_submissions.borrow_mut().push_back(format!("/tree-summary-custom {custom_entry}")); }
                else { *pending.borrow_mut() = Some((select_entry.clone(), choice == "Summarize", None)); select_submissions.borrow_mut().push_back("/tree-navigate".into()); }
            }),
            Box::new(move || { cancelled.borrow_mut().take(); cancel_submissions.borrow_mut().push_back(format!("/tree {entry}")); }), Default::default());
        self.ui_dialog = Some(Box::new(component));
    }

    /// senpi `promptBranchSummaryChoice`'s custom-instructions editor; cancel loops back to the selector.
    fn open_branch_summary_custom_prompt(&mut self, entry_id: &str) {
        let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
        let pending = self.pending_tree_nav.clone();
        let submit_submissions = self.submissions.clone(); let submit_closed = self.ui_reply.clone();
        let cancel_submissions = self.submissions.clone(); let cancel_closed = self.ui_reply.clone();
        let entry = entry_id.to_owned(); let submit_entry = entry.clone(); let cancel_entry = entry.clone();
        self.ui_dialog = Some(Box::new(crate::components::extension_editor::ExtensionEditorComponent::new(&self.theme, self.editor_host.clone(),
            Arc::new(self.keybindings()), "Custom summarization instructions", None,
            Box::new(move |text| { *pending.borrow_mut() = Some((submit_entry.clone(), true, Some(text.to_owned()))); submit_submissions.borrow_mut().push_back("/tree-navigate".into()); submit_closed.borrow_mut().take(); }),
            Box::new(move || { cancel_submissions.borrow_mut().push_back(format!("/tree-summary {cancel_entry}")); cancel_closed.borrow_mut().take(); }), Default::default(), None, None)));
    }

    /// senpi `runTreeNavigation`'s committed tail: consume the chosen summary and navigate.
    async fn apply_tree_navigation(&mut self) -> Result<(), String> {
        let Some((entry_id, summarize, custom_instructions)) = self.pending_tree_nav.borrow_mut().take() else { return Ok(()); };
        self.run_tree_navigation(&entry_id, summarize, custom_instructions).await
    }

    /// senpi `runTreeNavigation`: stop any active response, then navigate with the summary choice.
    async fn run_tree_navigation(&mut self, entry_id: &str, summarize: bool, custom_instructions: Option<String>) -> Result<(), String> {
        if self.host_is_streaming() { self.restore_queued_messages(false); let _ = self.host_abort().await; }
        if self.host_is_compacting() { return Err("Wait for the current compaction or tree navigation to finish before navigating the session tree.".into()); }
        let options = maho_core::agent_session::TreeNavigationOptions { summarize: Some(summarize), custom_instructions, ..Default::default() };
        // senpi shows `BranchSummaryStatusIndicator` while a summarizing navigation runs.
        if summarize { self.show_branch_summary_status_indicator(); }
        let result = self.host_navigate_tree(entry_id, options).await;
        if summarize { self.clear_status_indicator(Some(crate::components::status_indicator::StatusIndicatorKind::BranchSummary)); }
        let result = result?;
        if result.aborted == Some(true) { self.show_status("Branch summarization cancelled".into()); self.submissions.borrow_mut().push_back(format!("/tree {entry_id}")); return Ok(()); }
        if result.cancelled { self.show_status("Navigation cancelled".into()); return Ok(()); }
        self.rebuild_history();
        if let Some(text) = result.editor_text { self.editor.editor.set_text(&text); }
        self.show_status("Navigated to selected point".into());
        Ok(())
    }

    fn show_status(&mut self, text: String) {
        let text = self.theme.fg(crate::theme::ThemeColor::Dim, &text);
        if let Some((count, component)) = &self.last_status && *count == self.chat.children().len() { component.borrow_mut().set_text(text); return; }
        self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1)))));
        let component = Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding(text, 1, 0)));
        self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(component.clone()));
        self.last_status = Some((self.chat.children().len(), component));
    }

    fn dispatch_command(&mut self, text: &str) -> Result<bool, String> {
        let text = text.trim();
        if text == "/login" || text.starts_with("/login ") {
            // senpi `handleLoginCommand`.
            let provider_ref = text.strip_prefix("/login ").map(str::trim).filter(|value| !value.is_empty());
            let Some(provider_ref) = provider_ref else { self.show_login_auth_type_selector(None); return Ok(true); };
            if let Some(rejection) = maho_ai::legacy_provider_ids::legacy_provider_id_rejection(provider_ref) { self.show_error(&rejection); return Ok(true); }
            let reference = provider_ref.to_lowercase();
            let matches = self.login_provider_options(None).into_iter()
                .filter(|provider| provider.id.to_lowercase() == reference || provider.name.to_lowercase() == reference).collect::<Vec<_>>();
            if matches.len() == 1 { self.start_provider_login(&matches[0]); return Ok(true); }
            if matches.len() > 1 && matches.iter().all(|provider| provider.id == matches[0].id) { self.show_login_auth_type_selector(Some(matches)); return Ok(true); }
            self.show_login_provider_selector(None, Some(provider_ref));
            return Ok(true);
        }
        if let Some(args) = text.strip_prefix("/login-select-provider ") {
            let auth_type = match args.trim() { "oauth" => crate::components::oauth_selector::AuthSelectorAuthType::OAuth, "api_key" => crate::components::oauth_selector::AuthSelectorAuthType::ApiKey, _ => return Ok(true) };
            self.show_login_provider_selector(Some(auth_type), None);
            return Ok(true);
        }
        if text == "/login-auth-type" { self.show_login_auth_type_selector(None); return Ok(true); }
        if let Some(args) = text.strip_prefix("/login-provider ") {
            let Some((provider_id, method)) = args.trim().split_once(' ') else { return Ok(true); };
            let auth_type = match method { "oauth" => crate::components::oauth_selector::AuthSelectorAuthType::OAuth, "api_key" => crate::components::oauth_selector::AuthSelectorAuthType::ApiKey, _ => return Ok(true) };
            if let Some(provider) = self.login_provider_options(Some(auth_type)).into_iter().find(|provider| provider.id == provider_id) { self.start_provider_login(&provider); }
            return Ok(true);
        }
        if text == "/changelog" {
            let source=maho_core::changelog_source::resolve_changelog_source();
            let entries=crate::interactive_changelog::entries(&source.path);
            let markdown=if entries.is_empty(){"No changelog entries found.".into()}else{entries.into_iter().rev().map(|(version,content)|if source.rewrite_links {crate::interactive_changelog::normalize_links(&content,&version)}else{content}).collect::<Vec<_>>().join("\n\n")};
            self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1)))));
            self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(crate::components::dynamic_border::DynamicBorder::new(self.theme.clone())))));
            self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding(self.theme.bold(&self.theme.fg(crate::theme::ThemeColor::Accent,"What's New")),1,0)))));
            self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1)))));
            self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(crate::components::markdown_transform::MarkdownComponent(maho_tui::components::markdown::Markdown::new(&markdown,1,1,get_markdown_theme(&self.theme),None,Default::default()))))));
            self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(crate::components::dynamic_border::DynamicBorder::new(self.theme.clone())))));
            return Ok(true);
        }
        if text == "/resume" {
            use crate::components::session_selector::SessionSelectorComponent;
            let (directory, default_directory) = self.session.with_session_manager(|manager| (manager.session_dir().to_owned(), manager.uses_default_session_dir()));
            let current_directory = directory.clone();
            let all_directory = if default_directory { std::path::Path::new(&self.session.agent_dir()).join("sessions") } else { std::path::PathBuf::from(directory) };
            let convert = |info: maho_core::session_discovery::SessionInfo| crate::components::session_selector_search::SessionInfo { path:info.path, id:info.id, cwd:info.cwd, name:info.name, parent_session_path:info.parent_session_path, modified_ms:info.modified.timestamp_millis(), message_count:info.message_count, first_message:info.first_message, all_messages_text:info.all_messages_text };
            let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
            let selected = self.ui_reply.clone(); let cancelled = selected.clone(); let exited = selected.clone(); let submissions = self.submissions.clone(); let exit_submissions = self.submissions.clone(); let host = self.editor_host.clone();
            self.ui_dialog = Some(Box::new(SessionSelectorComponent::new(&self.theme, Arc::new(self.keybindings()),
                Box::new(move |_| maho_core::session_discovery::list_sessions_from_dir(&current_directory, None, 0, None).into_iter().map(convert).collect()),
                Box::new(move |_| {
                    let mut sessions = maho_core::session_discovery::list_sessions_from_dir(&all_directory.to_string_lossy(), None, 0, None);
                    if default_directory && let Ok(directories) = std::fs::read_dir(&all_directory) {
                        for directory in directories.flatten().filter(|entry| entry.path().is_dir()) { sessions.extend(maho_core::session_discovery::list_sessions_from_dir(&directory.path().to_string_lossy(), None, 0, None)); }
                    }
                    sessions.into_iter().map(convert).collect()
                }),
                Box::new(move |path| { submissions.borrow_mut().push_back(format!("/resume \"{path}\"")); selected.borrow_mut().take(); }),
                Box::new(move || { cancelled.borrow_mut().take(); }), Box::new(move || { exit_submissions.borrow_mut().push_back("/quit".into()); exited.borrow_mut().take(); }),
                Box::new(move || host.request_render()), Some(Box::new(|path, name| { let mut manager = maho_core::session_manager::SessionManager::open(path, None, None, None); manager.append_session_info(name); })), Some(true),
                self.host_session_file().as_deref(), std::env::var("HOME").ok())));
            return Ok(true);
        }
        if text == "/settings" {
            use crate::components::settings_selector::{SettingsSelectorComponent, SettingsConfig, SettingsCallbacks, ThinkingLevel};
            let mut config = SettingsConfig { auto_compact:self.host_auto_compaction_enabled(),
                thinking_level:ThinkingLevel::from_name(self.host_thinking_level().as_str()).expect("session thinking level"),
                available_thinking_levels:self.host_available_thinking_levels().into_iter().filter_map(|level| ThinkingLevel::from_name(level.as_str())).collect(), ..Default::default() };
            let theme_registry = crate::theme::registry::ThemeRegistry::new(std::path::Path::new(&self.session.agent_dir()).join("themes"), &self.theme.name, self.theme.get_color_mode()).map_err(|error| error.to_string())?;
            config.current_theme = self.theme.name.clone(); config.available_themes = theme_registry.get_available_themes_with_paths().into_iter().map(|theme| theme.name).collect();
            self.session.with_settings_manager(|settings| {
                macro_rules! boolean { ($field:ident, $key:literal) => { config.$field = settings.get_bool($key).unwrap_or(config.$field); }; }
                macro_rules! number { ($field:ident, $key:literal) => { if let Some(value) = settings.get_number($key) { config.$field = value as _; } }; }
                macro_rules! string { ($field:ident, $key:literal) => { if let Some(value) = settings.get_string($key) { config.$field = value; } }; }
                boolean!(show_images, "showImages"); boolean!(auto_resize_images, "imageAutoResize"); boolean!(block_images, "blockImages"); boolean!(enable_skill_commands, "enableSkillCommands");
                boolean!(hide_thinking_block, "hideThinkingBlock"); boolean!(smooth_streaming, "smoothStreaming"); boolean!(show_cache_miss_notices, "showCacheMissNotices"); boolean!(collapse_changelog, "collapseChangelog");
                boolean!(enable_install_telemetry, "enableInstallTelemetry"); boolean!(show_hardware_cursor, "showHardwareCursor"); boolean!(quiet_startup, "quietStartup"); boolean!(clear_on_shrink, "clearOnShrink"); boolean!(show_terminal_progress, "showTerminalProgress"); boolean!(fullscreen_copy_on_select, "fullscreenCopyOnSelect");
                number!(image_width_cells, "imageWidthCells"); number!(http_idle_timeout_ms, "httpIdleTimeoutMs"); number!(smooth_streaming_fps, "smoothStreamingFps"); number!(editor_padding_x, "editorPaddingX"); number!(output_pad, "outputPad"); number!(autocomplete_max_visible, "autocompleteMaxVisible");
                string!(steering_mode, "steeringMode"); string!(follow_up_mode, "followUpMode"); string!(transport, "transport"); string!(double_escape_action, "doubleEscapeAction"); string!(tree_filter_mode, "treeFilterMode"); string!(fullscreen_scrollbar, "fullscreenScrollbar");
                use crate::components::settings_selector::{MermaidRenderingMode, TuiMode, FullscreenExitOutput, DefaultProjectTrust};
                config.terminal_mouse = settings.get_string("terminalMouse");
                config.mermaid_rendering_mode = MermaidRenderingMode::from_name(settings.get_mermaid_rendering_mode().as_str()).unwrap_or(config.mermaid_rendering_mode);
                config.tui_mode = settings.get_string("tuiMode").and_then(|value| TuiMode::from_name(&value)).unwrap_or(config.tui_mode);
                config.fullscreen_exit_output = settings.get_string("fullscreenExitOutput").and_then(|value| FullscreenExitOutput::from_name(&value)).unwrap_or(config.fullscreen_exit_output);
                config.default_project_trust = match settings.get_string("defaultProjectTrust").as_deref() { Some("always") => DefaultProjectTrust::Always, Some("never") => DefaultProjectTrust::Never, _ => DefaultProjectTrust::Ask };
                config.warnings.anthropic_extra_usage = settings.get_value("warnings").and_then(|value| value.get("anthropicExtraUsage")).and_then(serde_json::Value::as_bool);
            });
            let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
            let cancelled = self.ui_reply.clone();
            let mut callbacks = SettingsCallbacks { on_cancel:Some(Box::new(move || { cancelled.borrow_mut().take(); })), ..Default::default() };
            macro_rules! persist { ($field:ident, $key:literal, $ty:ty) => {{ let session = self.session.clone(); let ui = self.extension_ui.clone(); callbacks.$field = Some(Box::new(move |value: $ty| {
                let values = [($key.into(), serde_json::json!(value))].into_iter().collect();
                match session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &values)) {
                    Ok(()) => { drop(ui.sender.send(crate::interactive_extension_ui::UiRequest::SettingChanged($key.into(), serde_json::json!(value)))); }
                    Err(error) => maho_ext_api::ExtensionUi::notify(ui.as_ref(), &error, maho_ext_api::NotificationType::Error),
                }
            })); }}; }
            persist!(on_show_images_change, "showImages", bool); persist!(on_image_width_cells_change, "imageWidthCells", usize); persist!(on_auto_resize_images_change, "imageAutoResize", bool); persist!(on_block_images_change, "blockImages", bool); persist!(on_enable_skill_commands_change, "enableSkillCommands", bool);
            persist!(on_transport_change, "transport", String); persist!(on_http_idle_timeout_ms_change, "httpIdleTimeoutMs", u64); persist!(on_theme_change, "theme", String); persist!(on_hide_thinking_block_change, "hideThinkingBlock", bool);
            persist!(on_smooth_streaming_change, "smoothStreaming", bool); persist!(on_smooth_streaming_fps_change, "smoothStreamingFps", u32); persist!(on_show_cache_miss_notices_change, "showCacheMissNotices", bool); persist!(on_collapse_changelog_change, "collapseChangelog", bool); persist!(on_enable_install_telemetry_change, "enableInstallTelemetry", bool);
            persist!(on_double_escape_action_change, "doubleEscapeAction", String); persist!(on_tree_filter_mode_change, "treeFilterMode", String); persist!(on_show_hardware_cursor_change, "showHardwareCursor", bool); persist!(on_editor_padding_x_change, "editorPaddingX", usize); persist!(on_output_pad_change, "outputPad", u8); persist!(on_autocomplete_max_visible_change, "autocompleteMaxVisible", usize);
            persist!(on_quiet_startup_change, "quietStartup", bool); persist!(on_clear_on_shrink_change, "clearOnShrink", bool); persist!(on_show_terminal_progress_change, "showTerminalProgress", bool); persist!(on_terminal_mouse_change, "terminalMouse", String); persist!(on_fullscreen_scrollbar_change, "fullscreenScrollbar", String); persist!(on_fullscreen_copy_on_select_change, "fullscreenCopyOnSelect", bool);
            let session = self.session.clone(); callbacks.on_auto_compact_change = Some(Box::new(move |enabled| { session.set_auto_compaction_enabled(enabled); let values = [("compaction".into(), serde_json::json!({"enabled":enabled}))].into_iter().collect(); if let Err(error) = session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &values)) { session.emit(maho_ext_api::AgentSessionEvent::ContinuationError { error_message:error }); } }));
            let session = self.session.clone(); callbacks.on_steering_mode_change = Some(Box::new(move |mode| session.set_steering_mode(if mode == "all" { maho_agent::types::QueueMode::All } else { maho_agent::types::QueueMode::OneAtATime })));
            let session = self.session.clone(); callbacks.on_follow_up_mode_change = Some(Box::new(move |mode| session.set_follow_up_mode(if mode == "all" { maho_agent::types::QueueMode::All } else { maho_agent::types::QueueMode::OneAtATime })));
            let session = self.session.clone(); callbacks.on_thinking_level_change = Some(Box::new(move |level| session.set_thinking_level(maho_ai::types::ModelThinkingLevel::parse(level.as_str()).expect("selector thinking level"))));
            macro_rules! persist_enum { ($field:ident, $key:literal, $ty:ty, $value:expr) => {{ let session = self.session.clone(); let ui = self.extension_ui.clone(); callbacks.$field = Some(Box::new(move |value: $ty| {
                let serialized = ($value)(value); let values = [($key.into(), serialized.clone())].into_iter().collect();
                match session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &values)) {
                    Ok(()) => { drop(ui.sender.send(crate::interactive_extension_ui::UiRequest::SettingChanged($key.into(), serialized))); }
                    Err(error) => maho_ext_api::ExtensionUi::notify(ui.as_ref(), &error, maho_ext_api::NotificationType::Error),
                }
            })); }}; }
            use crate::components::settings_selector::{MermaidRenderingMode, TuiMode, FullscreenExitOutput, DefaultProjectTrust, WarningSettings};
            { let session = self.session.clone(); let ui = self.extension_ui.clone(); callbacks.on_mermaid_rendering_mode_change = Some(Box::new(move |value: MermaidRenderingMode| {
                let mode = match value { MermaidRenderingMode::Off => maho_core::settings_shapes::MermaidRenderingMode::Off, MermaidRenderingMode::Final => maho_core::settings_shapes::MermaidRenderingMode::Final, MermaidRenderingMode::Streaming => maho_core::settings_shapes::MermaidRenderingMode::Streaming };
                match session.with_settings_manager_mut(|settings| settings.set_mermaid_rendering_mode(mode)) {
                    Ok(()) => { drop(ui.sender.send(crate::interactive_extension_ui::UiRequest::SettingChanged("markdown".into(), serde_json::json!({"mermaid":value.as_str()})))); }
                    Err(error) => maho_ext_api::ExtensionUi::notify(ui.as_ref(), &error, maho_ext_api::NotificationType::Error),
                }
            })); }
            persist_enum!(on_tui_mode_change, "tuiMode", TuiMode, |value: TuiMode| serde_json::json!(value.as_str()));
            persist_enum!(on_fullscreen_exit_output_change, "fullscreenExitOutput", FullscreenExitOutput, |value: FullscreenExitOutput| serde_json::json!(value.as_str()));
            persist_enum!(on_default_project_trust_change, "defaultProjectTrust", DefaultProjectTrust, |value: DefaultProjectTrust| serde_json::json!(match value { DefaultProjectTrust::Ask => "ask", DefaultProjectTrust::Always => "always", DefaultProjectTrust::Never => "never" }));
            persist_enum!(on_warnings_change, "warnings", WarningSettings, |value: WarningSettings| serde_json::json!({"anthropicExtraUsage":value.anthropic_extra_usage}));
            self.ui_dialog = Some(Box::new(SettingsSelectorComponent::new(&self.theme, config, callbacks, self.host_model().input.contains(&maho_ai::types::InputModality::Image))));
            return Ok(true);
        }
        if text == "/trust" {
            use crate::components::trust_selector::{TrustSelectorComponent, TrustSelectorOptions};
            let cwd = self.host_cwd();
            let store = maho_core::trust_manager::ProjectTrustStore::new(&self.session.agent_dir());
            let saved_decision = store.get_entry(&cwd)?;
            let project_trusted = self.session.with_settings_manager(|settings| settings.is_project_trusted());
            let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
            let selected = self.ui_reply.clone(); let cancelled = selected.clone(); let ui = self.extension_ui.clone();
            self.ui_dialog = Some(Box::new(TrustSelectorComponent::new(&self.theme, Arc::new(self.keybindings()), TrustSelectorOptions {
                cwd, saved_decision, project_trusted,
                on_select:Box::new(move |selection| {
                    match store.set_many(&selection.updates) {
                        Ok(()) => { selected.borrow_mut().take(); maho_ext_api::ExtensionUi::notify(ui.as_ref(), &format!("Saved trust decision: {}. Restart {} for this to take effect.", if selection.trusted { "trusted" } else { "untrusted" }, maho_core::config::app_name()), maho_ext_api::NotificationType::Info); }
                        Err(error) => maho_ext_api::ExtensionUi::notify(ui.as_ref(), &error, maho_ext_api::NotificationType::Error),
                    }
                }), on_cancel:Box::new(move || { cancelled.borrow_mut().take(); }),
            })));
            return Ok(true);
        }
        if matches!(text, "/scoped-models" | "/favorite-models") {
            let favorites = text == "/favorite-models";
            let models = self.host_available_models();
            if favorites && models.is_empty() { self.show_status("No models available".into()); return Ok(true); }
            let key = if favorites { "favoriteModels" } else { "enabledModels" };
            let configured: Option<Vec<String>> = self.session.with_settings_manager(|settings| settings.get_value(key).filter(|value| !value.is_null()).cloned()).map(serde_json::from_value).transpose().map_err(|error| error.to_string())?;
            let stored = configured.clone().unwrap_or_default();
            let catalog = self.session.model_registry().get_all();
            let resolution = maho_core::model_resolver::resolve_model_scope_from_models(&stored, &catalog);
            let candidate_ids: Vec<_> = models.iter().map(|model| format!("{}/{}", model.provider, model.id)).collect();
            let entries = if favorites { self.host_favorite_models() } else { self.host_scoped_models() };
            let enabled = if !entries.is_empty() { Some(entries.iter().map(|entry| format!("{}/{}", entry.model.provider, entry.model.id)).collect()) }
                else if stored.is_empty() { if favorites || configured.is_some() { Some(Vec::new()) } else { None } }
                else { Some(resolution.pattern_resolutions.iter().flat_map(|item| if item.unresolved { vec![item.pattern.clone()] } else { item.owned_ids.clone() }).collect()) };
            let session = self.session.clone(); let available = Rc::new(RefCell::new(models.clone())); let footer_data = self.footer_data.clone();
            // senpi `updateSessionModels`: applies the scope over the (refreshable) available snapshot
            // and recomputes the footer's provider count.
            let update_session_models: Rc<dyn Fn(Option<Vec<String>>)> = { let available = available.clone(); Rc::new(move |ids: Option<Vec<String>>| {
                let available = available.borrow();
                let all_ids: Vec<_> = available.iter().map(|model| format!("{}/{}", model.provider, model.id)).collect();
                let resolved = maho_core::model_resolver::resolve_model_scope_from_models(ids.as_deref().unwrap_or(&all_ids), &available);
                let entries = resolved.scoped_models.into_iter().map(|entry| maho_core::agent_session::SessionModelEntry { model:entry.model, thinking_level:entry.thinking_level.map(|level| serde_json::from_value(serde_json::json!(level.as_str())).expect("resolved thinking level")), thinking_selection:entry.thinking_selection, service_tier:entry.service_tier.map(|tier| match tier.as_str() { "auto" => maho_ext_api::ServiceTier::Auto, "flex" => maho_ext_api::ServiceTier::Flex, "priority" => maho_ext_api::ServiceTier::Priority, _ => unreachable!("resolved service tier") }) }).collect();
                if favorites { session.set_favorite_models(entries); return; }
                else if ids.as_ref().is_some_and(|ids| available.iter().all(|model| ids.contains(&format!("{}/{}", model.provider, model.id)))) { session.set_scoped_models(Vec::new()); }
                else { session.set_scoped_models(entries); }
                // senpi `updateAvailableProviderCount`: the scoped models when narrowed, else the snapshot.
                let scoped = session.scoped_models();
                let providers: std::collections::BTreeSet<_> = if scoped.is_empty() { available.iter().map(|model| model.provider.clone()).collect() } else { scoped.into_iter().map(|entry| entry.model.provider).collect() };
                footer_data.set_available_provider_count(providers.len());
            }) };
            let selection_changed = Rc::new(std::cell::Cell::new(false)); let current_enabled = Rc::new(RefCell::new(enabled.clone()));
            let on_change = { let update = update_session_models.clone(); let changed = selection_changed.clone(); let current = current_enabled.clone(); Box::new(move |ids: Option<Vec<String>>| { changed.set(true); *current.borrow_mut() = ids.clone(); update(ids); }) };
            let refresh_stored = stored.clone(); let refresh_configured = configured.is_some(); let scoped_at_open = !entries.is_empty();
            let session = self.session.clone(); let ui = self.extension_ui.clone(); let persist_available = available.clone();
            let on_persist = Box::new(move |ids: Option<Vec<String>>| {
                let patterns = if favorites { crate::components::model_favorites::merge_favorite_patterns_for_persist(crate::components::model_favorites::FavoritePatternsForPersist { stored_patterns:&stored, pattern_resolutions:&resolution.pattern_resolutions, selected_ids:&ids, candidate_ids:&candidate_ids }) }
                    else { let candidate_ids: Vec<_> = persist_available.borrow().iter().map(|model| format!("{}/{}", model.provider, model.id)).collect(); ids.filter(|ids| !candidate_ids.iter().all(|id| ids.contains(id))) };
                let values = [(key.into(), serde_json::json!(patterns))].into_iter().collect();
                match session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &values)) {
                    Ok(()) => maho_ext_api::ExtensionUi::notify(ui.as_ref(), if favorites { "Favorite models saved to settings" } else { "Model selection saved to settings" }, maho_ext_api::NotificationType::Info),
                    Err(error) => maho_ext_api::ExtensionUi::notify(ui.as_ref(), &error, maho_ext_api::NotificationType::Error),
                }
            });
            let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
            let cancelled = self.ui_reply.clone();
            let keys = Arc::new(self.keybindings());
            if favorites {
                use crate::components::favorite_models_selector::{FavoriteModelsSelectorComponent, FavoriteModelsConfig, FavoriteModelsCallbacks};
                let selected = self.ui_reply.clone(); let submissions = self.submissions.clone(); let current = self.host_model();
                self.ui_dialog = Some(Box::new(FavoriteModelsSelectorComponent::new(&self.theme, keys,
                    FavoriteModelsConfig { all_models:models.into_iter().map(|model| crate::components::model_selector::ModelEntry { provider:model.provider, id:model.id, name:model.name }).collect(), favorite_model_ids:enabled, current_model:Some(crate::components::model_selector::ModelEntry { provider:current.provider, id:current.id, name:current.name }) },
                    FavoriteModelsCallbacks { on_change, on_persist, on_select:Box::new(move |model| { submissions.borrow_mut().push_back(format!("/model-select {}/{}", model.provider, model.id)); selected.borrow_mut().take(); }), on_cancel:Box::new(move || { cancelled.borrow_mut().take(); }) })));
            } else {
                use crate::components::scoped_models_selector::{ScopedModelsSelectorComponent, ModelsConfig, ModelsCallbacks};
                use crate::theme::ThemeColor;
                // senpi `showScopedModelsSelector`: refresh the catalogs under a 15s abort while the
                // selector is open. A mounted host owns its catalog, so only the local runtime refreshes.
                let local = self.session_host.is_none();
                let selector = ScopedModelsSelectorComponent::new(&self.theme, keys,
                    ModelsConfig { all_models:models, enabled_model_ids:enabled, refresh_status:local.then(|| "Refreshing model catalogs…".to_owned()) },
                    ModelsCallbacks { on_change, on_persist, on_cancel:Box::new(move || { cancelled.borrow_mut().take(); }) });
                let controller = maho_ai::utils::abort::AbortController::new();
                let refresh = local.then(|| {
                    let (sender, receiver) = tokio::sync::oneshot::channel();
                    let runtime = self.session.model_registry().model_runtime.clone(); let session = self.session.clone(); let task_controller = controller.clone(); let runtime_key = Arc::as_ptr(&self.session) as usize as u64;
                    tokio::spawn(async move {
                        let signal = task_controller.signal();
                        let timed_out = Arc::new(std::sync::atomic::AtomicBool::new(false));
                        let timer = { let timed_out = timed_out.clone(); tokio::spawn(async move { tokio::time::sleep(std::time::Duration::from_secs(15)).await; timed_out.store(true, std::sync::atomic::Ordering::SeqCst); task_controller.abort(None); }) };
                        let start = move |signal: maho_ai::utils::abort::AbortSignal| async move { let mut runtime = runtime; runtime.refresh(maho_ai::models::ModelsRefreshOptions { signal: Some(signal), ..Default::default() }).await };
                        let outcome = crate::model_catalog_refresh::refresh_model_catalogs(runtime_key, &signal, start).await;
                        timer.abort();
                        let _ = sender.send((outcome, timed_out.load(std::sync::atomic::Ordering::SeqCst), session.model_registry().get_available()));
                    });
                    receiver
                });
                let on_refreshed = Box::new(move |selector: &mut ScopedModelsSelectorComponent, (outcome, timed_out, models): ScopedModelsRefreshOutcome| {
                    if outcome.is_ok() {
                        *available.borrow_mut() = models.clone();
                        if !selection_changed.get() && !scoped_at_open {
                            let ids: Option<Vec<String>> = if refresh_stored.is_empty() { refresh_configured.then(Vec::new) } else { Some(maho_core::model_resolver::resolve_model_scope_from_models(&refresh_stored, &models).pattern_resolutions.iter().flat_map(|item| if item.unresolved { vec![item.pattern.clone()] } else { item.owned_ids.clone() }).collect()) };
                            *current_enabled.borrow_mut() = ids.clone();
                            selector.update_models(&models, Some(ids));
                        } else { selector.update_models(&models, None); }
                        let ids = current_enabled.borrow().clone();
                        if ids.is_some() { update_session_models(ids); }
                    }
                    let (message, color) = match outcome {
                        Ok(result) if result.aborted && timed_out => ("Model refresh timed out; showing cached models.".to_owned(), ThemeColor::Warning),
                        Ok(result) if !result.errors.is_empty() => (format!("Could not refresh {}; showing cached models.", result.errors.keys().cloned().collect::<Vec<_>>().join(", ")), ThemeColor::Warning),
                        Ok(_) => ("Model catalogs refreshed.".to_owned(), ThemeColor::Success),
                        Err(_) if timed_out => ("Model refresh timed out; showing cached models.".to_owned(), ThemeColor::Warning),
                        Err(error) => (format!("Could not refresh model catalogs: {error}"), ThemeColor::Warning),
                    };
                    selector.set_refresh_status(&message, color);
                });
                self.ui_dialog = Some(Box::new(ScopedModelsRefreshDialog { selector, refresh, controller, on_refreshed }));
            }
            return Ok(true);
        }
        if text == "/tree" {
            let (tree, leaf) = self.session.with_session_manager(|manager| {
                (manager.get_tree(), manager.leaf_id().map(str::to_owned))
            });
            if tree.is_empty() { self.show_status("No entries in session".into()); return Ok(true); }
            let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
            let selected = self.ui_reply.clone(); let cancelled = selected.clone(); let submissions = self.submissions.clone(); let session = self.session.clone();
            let filter = self.session.with_settings_manager(|settings| settings.get_value("treeFilterMode").and_then(serde_json::Value::as_str).unwrap_or("default").to_owned());
            let filter = match filter.as_str() { "no-tools" => crate::components::tree_selector::FilterMode::NoTools, "user-only" => crate::components::tree_selector::FilterMode::UserOnly, "labeled-only" => crate::components::tree_selector::FilterMode::LabeledOnly, "all" => crate::components::tree_selector::FilterMode::All, _ => crate::components::tree_selector::FilterMode::Default };
            let mut selector = crate::components::tree_selector::TreeSelectorComponent::new(&self.theme,
                Arc::new(self.keybindings()),
                tree, leaf.as_deref(), self.editor_host.terminal_rows(),
                Box::new(move |id| { submissions.borrow_mut().push_back(format!("/tree {id}")); selected.borrow_mut().take(); }), Box::new(move || { cancelled.borrow_mut().take(); }),
                Some(Box::new(move |id, label| if let Err(error_message) = session.with_session_manager_mut(|manager| manager.append_label(id, label)) { session.emit(maho_ext_api::AgentSessionEvent::ContinuationError { error_message }); })), None, Some(filter), std::env::var("HOME").ok());
            let copies=self.tree_copies.clone();
            selector.on_copy=Some(Box::new(move |text|copies.borrow_mut().push_back(text.map(str::to_owned))));
            // senpi `selector.onEditMessage`: close the selector, then open the edit flow.
            let edit_submissions=self.submissions.clone(); let edit_closed=self.ui_reply.clone();
            selector.on_edit_message=Some(Box::new(move |id| { edit_submissions.borrow_mut().push_back(format!("/tree-edit {id}")); edit_closed.borrow_mut().take(); }));
            self.ui_dialog=Some(Box::new(selector));
            return Ok(true);
        }
        if text == "/fork" {
            use crate::components::user_message_selector::{UserMessageItem, UserMessageSelectorComponent};
            let entries = self.session.with_session_manager(|manager| manager.branch(None));
            let messages: Vec<UserMessageItem> = entries.iter().filter(|entry| entry["message"]["role"] == "user").map(|entry| {
                let content = &entry["message"]["content"];
                let text = content.as_str().map(str::to_owned).unwrap_or_else(|| content.as_array().map(|parts| parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default());
                UserMessageItem { id:entry["id"].as_str().expect("entry id").into(), text, timestamp:entry["timestamp"].as_str().map(str::to_owned) }
            }).collect();
            if messages.is_empty() { self.show_status("No messages to fork from".into()); return Ok(true); }
            let initial_selected_id = messages.last().map(|message| message.id.clone());
            let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
            let selected = self.ui_reply.clone(); let cancelled = selected.clone(); let submissions = self.submissions.clone();
            self.ui_dialog = Some(Box::new(UserMessageSelectorComponent::new(messages,
                Box::new(move |id| { submissions.borrow_mut().push_back(format!("/fork {id}")); selected.borrow_mut().take(); }),
                Box::new(move || { cancelled.borrow_mut().take(); }), initial_selected_id.as_deref(), self.theme.clone())));
            if self.ui_reply.borrow().is_none() { self.ui_dialog = None; self.local_dialog_reply = None; }
            return Ok(true);
        }
        let model_search = if text == "/model" { Some(None) } else { text.strip_prefix("/model-search ").map(|term| Some(term.trim().to_owned())) };
        if let Some(initial_search) = model_search {
            use crate::components::model_selector::{ModelSelectorComponent, ModelEntry, ModelSelectorFavoriteOptions};
            let current = self.host_model();
            let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
            let selected = self.ui_reply.clone(); let cancelled = selected.clone(); let submissions = self.submissions.clone();
            let models = self.host_available_models().into_iter().map(|model| ModelEntry { provider:model.provider, id:model.id, name:model.name }).collect::<Vec<_>>();
            let stored: Vec<String> = self.session.with_settings_manager(|settings| settings.get_value("favoriteModels").cloned()).map(serde_json::from_value).transpose().map_err(|error| error.to_string())?.unwrap_or_default();
            let catalog = self.session.model_registry().get_all();
            let resolutions = maho_core::model_resolver::resolve_model_scope_from_models(&stored, &catalog).pattern_resolutions;
            let candidate_ids = models.iter().map(ModelEntry::full_id).collect::<Vec<_>>();
            let session_favorites = self.host_favorite_models().into_iter().map(|entry| format!("{}/{}", entry.model.provider, entry.model.id)).filter(|id| candidate_ids.contains(id)).collect::<Vec<_>>();
            let favorite_ids = Some(if session_favorites.is_empty() { resolutions.iter().flat_map(|resolution| resolution.owned_ids.clone()).filter(|id| candidate_ids.contains(id)).collect() } else { session_favorites });
            let session = self.session.clone(); let ui = self.extension_ui.clone();
            let favorite_callback = Box::new(move |ids: crate::components::favorite_model_ids::FavoriteModelIds, candidates: &[ModelEntry], _: &ModelEntry| {
                let candidate_ids = candidates.iter().map(ModelEntry::full_id).collect::<Vec<_>>();
                let merged = crate::components::model_favorites::merge_favorite_patterns_for_persist(crate::components::model_favorites::FavoritePatternsForPersist { stored_patterns:&stored, pattern_resolutions:&resolutions, selected_ids:&ids, candidate_ids:&candidate_ids });
                let values = [("favoriteModels".into(), serde_json::json!(merged))].into_iter().collect();
                if let Err(error) = session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &values)) { maho_ext_api::ExtensionUi::notify(ui.as_ref(), &error, maho_ext_api::NotificationType::Error); return; }
                let patterns = merged.unwrap_or_default();
                let resolved = maho_core::model_resolver::resolve_model_scope_from_models(&patterns, &catalog);
                session.set_favorite_models(resolved.scoped_models.into_iter().map(|entry| maho_core::agent_session::SessionModelEntry { model:entry.model, thinking_level:entry.thinking_level.and_then(|level| serde_json::from_value(serde_json::json!(level.as_str())).ok()), thinking_selection:entry.thinking_selection, service_tier:entry.service_tier.map(|tier| match tier.as_str() { "auto" => maho_ext_api::ServiceTier::Auto, "flex" => maho_ext_api::ServiceTier::Flex, "priority" => maho_ext_api::ServiceTier::Priority, _ => unreachable!("resolved service tier") }) }).collect());
            });
            let scoped = self.host_scoped_models().into_iter().map(|entry| crate::components::model_selector::ScopedModelItem { model:ModelEntry { provider:entry.model.provider, id:entry.model.id, name:entry.model.name }, thinking_level:entry.thinking_level.map(|level| serde_json::to_value(level).expect("thinking level").as_str().expect("string level").to_owned()) }).collect();
            let mut selector = ModelSelectorComponent::new(&self.theme, Arc::new(self.keybindings()), 0, &models,
                Some(ModelEntry { provider:current.provider, id:current.id, name:current.name }),
                scoped,
                Box::new(move |model| { submissions.borrow_mut().push_back(format!("/model-select {}/{}", model.provider, model.id)); selected.borrow_mut().take(); }),
                Box::new(move || { cancelled.borrow_mut().take(); }), initial_search.as_deref(),
                ModelSelectorFavoriteOptions { favorite_model_ids:favorite_ids, on_favorite_change:Some(favorite_callback) }, None);
            let session = self.session.clone(); let ui = self.extension_ui.clone();
            selector.set_default_model_change_handler(Box::new(move |model| {
                let values = [("defaultProvider".into(), serde_json::json!(model.provider)), ("defaultModel".into(), serde_json::json!(model.id))].into_iter().collect();
                if let Err(error) = session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &values)) { maho_ext_api::ExtensionUi::notify(ui.as_ref(), &error, maho_ext_api::NotificationType::Error); }
            }));
            self.ui_dialog = Some(Box::new(selector));
            return Ok(true);
        }
        if text == "/thinking" {
            let (reply, _receiver) = tokio::sync::oneshot::channel();
            self.local_dialog_reply = Some(_receiver);
            *self.ui_reply.borrow_mut() = Some(reply);
            let selected = self.ui_reply.clone(); let cancelled = selected.clone(); let session = self.session.clone();
            let default = self.session.with_settings_manager(|settings| settings.get_string("defaultThinkingLevel")).and_then(|value| maho_ai::types::ModelThinkingLevel::parse(&value));
            let default_session = self.session.clone(); let ui = self.extension_ui.clone();
            self.ui_dialog = Some(Box::new(crate::components::thinking_selector::ThinkingSelectorComponent::new(&self.theme,
                Arc::new(self.keybindings()),
                crate::components::thinking_selector::ThinkingSelectorOptions { current:self.host_thinking_level(), available:self.host_available_thinking_levels(), default,
                    on_select:Box::new(move |level| { session.set_session_thinking_level(level); selected.borrow_mut().take(); }), on_cancel:Box::new(move || { cancelled.borrow_mut().take(); }), on_select_as_default:Some(Box::new(move |level| {
                        let values = [("defaultThinkingLevel".into(), serde_json::json!(level.as_str()))].into_iter().collect();
                        match default_session.with_settings_manager_mut(|settings| settings.set(maho_core::settings_manager::SettingsScope::Global, &values)) {
                            Ok(()) => maho_ext_api::ExtensionUi::notify(ui.as_ref(), &format!("Default thinking level: {}", level.as_str()), maho_ext_api::NotificationType::Info),
                            Err(error) => maho_ext_api::ExtensionUi::notify(ui.as_ref(), &error, maho_ext_api::NotificationType::Error),
                        }
                    })) })));
            return Ok(true);
        }
        if matches!(text, "/quit" | "/exit") { self.shutdown_requested = true; return Ok(true); }
        if text == "/session" {
            let stats = self.host_session_stats();
            let entries = self.session.with_session_manager(|manager| manager.entries());
            let prices = |provider: &str, model: &str| self.host_find_model(provider, model).map(|model| model.cost.cache_read);
            let waste = maho_core::cache_stats::compute_cache_waste(&entries, &prices).map_err(|error| error.to_string())?;
            let breakdown = maho_core::usage_totals::get_usage_cost_breakdown(&entries);
            let theme = &self.theme;
            let label = |text: &str| theme.fg(crate::theme::ThemeColor::Dim, text);
            let count = crate::components::compaction_summary_message::format_count;
            let mut info = format!("{}\n\n", theme.bold("Session Info"));
            if let Some(name) = self.session_display_name() { info += &format!("{} {name}\n", label("Name:")); }
            info += &format!("{} {}\n{} {}\n\n{}\n{} {}\n{} {}\n{} {}\n{} {} calls, {} results\n\n{}\n", label("File:"), stats.session_file.as_deref().unwrap_or("In-memory"), label("ID:"), stats.session_id, theme.bold("Messages"), label("Total:"), stats.total_messages, label("User:"), stats.user_messages, label("Assistant:"), stats.assistant_messages, label("Tools:"), stats.tool_calls, stats.tool_results, theme.bold("Tokens"));
            let tokens = stats.tokens;
            let prompt = tokens.input + tokens.cache_read + tokens.cache_write;
            info += &format!("{} {}\n", label("Input:"), count(prompt));
            if prompt > 0 && (tokens.cache_read > 0 || tokens.cache_write > 0) {
                info += &format!("  {} {} {}\n  {} {}{}\n", label("Cached:"), count(tokens.cache_read), label(&format!("({:.1}%)", tokens.cache_read as f64 / prompt as f64 * 100.0)), label("Uncached:"), count(tokens.input + tokens.cache_write), if tokens.cache_write > 0 { format!(" {}", label(&format!("({} written to cache)", count(tokens.cache_write)))) } else { String::new() });
            }
            info += &format!("{} {}\n{} {}\n", label("Output:"), count(tokens.output), label("Total:"), count(tokens.total));
            if stats.cost > 0.0 || waste.missed_tokens > 0.0 {
                info += &format!("\n{}\n{} ${:.3}", theme.bold("Cost"), label("Total:"), stats.cost);
                if breakdown.len() > 1 { for entry in breakdown { info += &format!("\n  {} ${:.3} {}", label(&format!("{}:", entry.key)), entry.cost, label(&format!("({} tokens)", crate::components::footer::format_tokens(entry.tokens as f64)))); } }
                if waste.missed_tokens > 0.0 {
                    let detail = format!("{} tokens, {} miss{}", count(waste.missed_tokens as u64), waste.miss_count, if waste.miss_count == 1 { "" } else { "es" });
                    info += &if waste.missed_cost >= 0.0001 { format!("\n{} ${:.3} {}", label("Cache Re-billed:"), waste.missed_cost, label(&format!("({detail})"))) } else { format!("\n{} {detail}", label("Cache Re-billed:")) };
                }
            }
            // senpi `handleSessionCommand`: a plain Text block, never the dim merging status line.
            self.chat.add_component(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1))));
            self.chat.add_component(Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding(info, 1, 0))));
            return Ok(true);
        }
        if matches!(text, "/rename" | "/name") {
            let accepted = self.rename_result.clone();
            let cancelled = self.rename_result.clone();
            self.rename_input = Some(crate::components::extension_input::ExtensionInputComponent::new(&self.theme, "Rename session", Box::new(move |text| *accepted.borrow_mut() = Some(Some(text.into()))), Box::new(move || *cancelled.borrow_mut() = Some(None)), crate::components::extension_input::ExtensionInputOptions { initial_value: self.host_session_name(), ..Default::default() }));
            return Ok(true);
        }
        if let Some(value) = text.strip_prefix("/thinking ") {
            let level = maho_ai::types::ModelThinkingLevel::parse(value.trim()).ok_or_else(|| format!("Invalid thinking level: {}", value.trim()))?;
            if !self.host_available_thinking_levels().contains(&level) { return Err(format!("Thinking level {} is not supported by the current model", value.trim())); }
            self.fire_set_session_thinking_level(level.as_str());
            self.show_status(format!("Thinking level: {}", level.as_str()));
            return Ok(true);
        }
        if text == "/hotkeys" {
            let keys=self.keybindings();
            let key_display_text=|action:&str|crate::components::keybinding_hints::format_key_text(&keys.get_keys(action).join("/"),true);
            let cursor_up=key_display_text("tui.editor.cursorUp");
            let cursor_down=key_display_text("tui.editor.cursorDown");
            let cursor_left=key_display_text("tui.editor.cursorLeft");
            let cursor_right=key_display_text("tui.editor.cursorRight");
            let cursor_word_left=key_display_text("tui.editor.cursorWordLeft");
            let cursor_word_right=key_display_text("tui.editor.cursorWordRight");
            let cursor_line_start=key_display_text("tui.editor.cursorLineStart");
            let cursor_line_end=key_display_text("tui.editor.cursorLineEnd");
            let jump_forward=key_display_text("tui.editor.jumpForward");
            let jump_backward=key_display_text("tui.editor.jumpBackward");
            let page_up=key_display_text("tui.editor.pageUp");
            let page_down=key_display_text("tui.editor.pageDown");
            let submit=key_display_text("tui.input.submit");
            let new_line=key_display_text("tui.input.newLine");
            let delete_word_backward=key_display_text("tui.editor.deleteWordBackward");
            let delete_word_forward=key_display_text("tui.editor.deleteWordForward");
            let delete_to_line_start=key_display_text("tui.editor.deleteToLineStart");
            let delete_to_line_end=key_display_text("tui.editor.deleteToLineEnd");
            let yank=key_display_text("tui.editor.yank");
            let yank_pop=key_display_text("tui.editor.yankPop");
            let undo=key_display_text("tui.editor.undo");
            let tab=key_display_text("tui.input.tab");
            let interrupt=key_display_text("app.interrupt");
            let clear=key_display_text("app.clear");
            let exit=key_display_text("app.exit");
            let suspend=key_display_text("app.suspend");
            let cycle_thinking_level=key_display_text("app.thinking.cycle");
            let cycle_model_forward=key_display_text("app.model.cycleForward");
            let select_model=key_display_text("app.model.select");
            let expand_tools=key_display_text("app.tools.expand");
            let toggle_thinking=key_display_text("app.thinking.toggle");
            let external_editor=key_display_text("app.editor.external");
            let cycle_model_backward=key_display_text("app.model.cycleBackward");
            let copy_message=key_display_text("app.message.copy");
            let follow_up=key_display_text("app.message.followUp");
            let dequeue=key_display_text("app.message.dequeue");
            let answer_question=key_display_text("app.question.answer");
            let next_question=key_display_text("app.question.next");
            let paste_image=key_display_text("app.clipboard.pasteImage");
            let windows_note=if cfg!(target_os="windows") {" (Ctrl+Enter on Windows Terminal)"}else{""};
            let mut markdown=format!(r#"
**Navigation**
| Key | Action |
|-----|--------|
| `{cursor_up}` / `{cursor_down}` / `{cursor_left}` / `{cursor_right}` | Move cursor / browse history |
| `{cursor_word_left}` / `{cursor_word_right}` | Move by word |
| `{cursor_line_start}` | Start of line |
| `{cursor_line_end}` | End of line |
| `{jump_forward}` | Jump forward to character |
| `{jump_backward}` | Jump backward to character |
| `{page_up}` / `{page_down}` | Scroll by page |

**Editing**
| Key | Action |
|-----|--------|
| `{submit}` | Send message |
| `{new_line}` | New line{windows_note} |
| `{delete_word_backward}` | Delete word backwards |
| `{delete_word_forward}` | Delete word forwards |
| `{delete_to_line_start}` | Delete to start of line |
| `{delete_to_line_end}` | Delete to end of line |
| `{yank}` | Paste the most-recently-deleted text |
| `{yank_pop}` | Cycle through the deleted text after pasting |
| `{undo}` | Undo |

**Other**
| Key | Action |
|-----|--------|
| `{tab}` | Path completion / accept autocomplete |
| `{interrupt}` | Cancel autocomplete / abort streaming |
| `{clear}` | Clear editor (first) / exit (second) |
| `{exit}` | Exit (when editor is empty) |
| `{suspend}` | Suspend to background |
| `{cycle_thinking_level}` | Cycle thinking level |
| `{cycle_model_forward}` / `{cycle_model_backward}` | Cycle models |
| `{select_model}` | Open model selector |
| `{expand_tools}` | Toggle tool output expansion |
| `{toggle_thinking}` | Toggle thinking block visibility |
| `{external_editor}` | Edit message in external editor |
| `{copy_message}` | Copy last assistant message |
| `{follow_up}` | Queue follow-up message |
| `{dequeue}` | Restore queued messages |
| `{answer_question}` | Open the pending question before dequeue (also: empty Enter; /answer lists requests) |
| `{next_question}` | Cycle pending questions from an empty composer |
| `{paste_image}` | Paste image or text from clipboard |
| `/` | Slash commands |
| `!` | Run bash command |
| `!!` | Run bash command (excluded from context) |
"#);
            // senpi: append the extension-registered shortcuts.
            if !self.extension_shortcut_rows.is_empty() {
                markdown.push_str("\n**Extensions**\n| Key | Action |\n|-----|--------|\n");
                for (key, description) in &self.extension_shortcut_rows {
                    markdown.push_str(&format!("| `{}` | {description} |\n", crate::components::keybinding_hints::format_key_text(key, true)));
                }
            }
            self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1)))));
            self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(crate::components::dynamic_border::DynamicBorder::new(self.theme.clone())))));
            self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding(self.theme.bold(&self.theme.fg(crate::theme::ThemeColor::Accent,"Keyboard Shortcuts")),1,0)))));
            self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1)))));
            self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(crate::components::markdown_transform::MarkdownComponent(maho_tui::components::markdown::Markdown::new(markdown.trim(), 1, 1, get_markdown_theme(&self.theme), None, Default::default()))))));
            self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(crate::components::dynamic_border::DynamicBorder::new(self.theme.clone())))));
            return Ok(true);
        }
        if text == "/debug" { self.handle_debug_command(); return Ok(true); }
        if text == "/arminsayshi" { self.handle_armin_says_hi(); return Ok(true); }
        if text == "/dementedelves" { self.handle_demented_delves(); return Ok(true); }
        if text == "/answer" || text.starts_with("/answer ") {
            let argument = text.strip_prefix("/answer").map(str::trim).unwrap_or_default();
            self.answer_command(argument);
            return Ok(true);
        }
        if text == "/import" || text.starts_with("/import ") {
            let path = get_path_command_argument(text, "/import").ok_or("Usage: /import <path.jsonl>")?;
            let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
            let selected = self.ui_reply.clone(); let cancelled = selected.clone(); let submissions = self.submissions.clone(); let cancel_submissions = self.submissions.clone();
            let confirm_path = path.clone();
            let title = format!("Import session\nReplace current session with {path}?");
            let component = crate::components::extension_selector::ExtensionSelectorComponent::new(&self.theme, Arc::new(self.keybindings()), &title, vec!["Yes".into(), "No".into()],
                Box::new(move |choice| { submissions.borrow_mut().push_back(if choice == "Yes" { format!("/import-confirm \"{confirm_path}\"") } else { "/import-cancel".to_owned() }); selected.borrow_mut().take(); }),
                Box::new(move || { cancel_submissions.borrow_mut().push_back("/import-cancel".to_owned()); cancelled.borrow_mut().take(); }), Default::default());
            self.ui_dialog = Some(Box::new(component));
            return Ok(true);
        }
        if let Some(name) = text.split_whitespace().next().and_then(|word| word.strip_prefix('/'))
            && maho_core::slash_commands::builtin_slash_commands().iter().any(|command| command.name == name)
        {
            return Err(format!("/{name} is not yet integrated into the native interactive runtime"));
        }
        Ok(false)
    }

    /// senpi `handleDebugCommand`: dump the current render, visible widths and the
    /// session messages to the debug log so a broken frame can be inspected.
    fn handle_debug_command(&mut self) {
        let (columns, rows) = self.terminal_dimensions.get();
        let width = usize::from(columns);
        let lines = self.render(width);
        let messages = self.host_messages().iter().map(|message| serde_json::to_string(message).unwrap_or_default()).collect::<Vec<_>>();
        let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let path = self.debug_log_path.clone().unwrap_or_else(maho_core::config::get_debug_log_path);
        let data = format_debug_log(&timestamp, width, usize::from(rows), &lines, &messages);
        if let Some(parent) = std::path::Path::new(&path).parent() { let _ = std::fs::create_dir_all(parent); }
        match std::fs::write(&path, data) {
            Ok(()) => self.show_notice_box(crate::components::earendil_announcement::NoticeSpec { title: "✓ Debug log written".into(), tone: Some(crate::theme::ThemeColor::Success), why: path, extra: Vec::new(), expanded_line: None }),
            Err(error) => self.show_status(format!("Failed to write debug log: {error}")),
        }
    }

    /// senpi `handleArminSaysHi`.
    fn handle_armin_says_hi(&mut self) {
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |elapsed| u64::from(elapsed.subsec_nanos()) ^ elapsed.as_secs());
        let component = Rc::new(RefCell::new(crate::components::armin::ArminComponent::new(crate::components::armin::effect_for_index(seed as usize), seed, self.theme.clone())));
        self.chat.add_component(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1))));
        self.chat.add_component(component.clone());
        self.armin_animations.push((component, self.clock.elapsed().as_secs_f64() * 1000.0));
        self.editor_host.request_render();
    }

    /// senpi `handleDementedDelves`.
    fn handle_demented_delves(&mut self) {
        self.chat.add_component(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1))));
        self.chat.add_component(Rc::new(RefCell::new(crate::components::earendil_announcement::EarendilAnnouncementComponent::new(&self.theme))));
        self.editor_host.request_render();
    }

    pub fn drain_events(&mut self) {
        self.drain_ui_requests();
        // senpi resolves debounced/asynchronous autocomplete requests on its event loop.
        self.editor.editor.poll_autocomplete();
        if let Some(editor) = self.custom_editor.as_mut() { editor.poll_autocomplete(); }
        self.drain_remote_history();
        self.drain_remote_models();
        self.drain_remote_stats();
        while let Ok(event) = self.events.try_recv() {
            self.handle_session_event(&event);
        }
        self.drain_auth_ui_events();
        self.drain_share_result();
    }

    /// senpi `handleShareCommand`: check `gh auth status`, export the session to a temp HTML file,
    /// then create a secret gist behind a cancellable bordered loader that replaces the editor.
    async fn handle_share_command(&mut self) {
        match tokio::process::Command::new("gh").args(["auth", "status"]).output().await {
            Ok(output) if output.status.success() => {}
            Ok(_) => { self.show_error("GitHub CLI is not logged in. Run 'gh auth login' first."); return; }
            Err(_) => { self.show_error("GitHub CLI (gh) is not installed. Install it from https://cli.github.com/"); return; }
        }
        let tmp_file = std::env::temp_dir().join("session.html");
        let theme_name = self.theme.name.clone();
        if let Err(error) = self.host_export_html(Some(tmp_file.to_string_lossy().as_ref()), &theme_name).await {
            self.show_error(&format!("Failed to export session: {error}"));
            return;
        }
        let now_ms = u64::try_from(self.clock.elapsed().as_millis()).unwrap_or(u64::MAX);
        let hint = crate::components::keybinding_hints::key_hint("tui.select.cancel", "cancel", &self.theme);
        let loader = crate::components::bordered_loader::BorderedLoader::new(self.theme.clone(), "Creating gist...", true, &hint, now_ms);
        let (cancel, cancelled) = tokio::sync::oneshot::channel::<()>();
        let (reply, result) = tokio::sync::oneshot::channel();
        let file = tmp_file.clone();
        tokio::spawn(async move {
            // Dropping the cancel sender (abort or restore) drops the child, which kills it.
            let mut command = tokio::process::Command::new("gh");
            command.args(["gist", "create", "--public=false"]).arg(&file).kill_on_drop(true);
            tokio::select! {
                output = command.output() => { let _ = reply.send(output); }
                _ = cancelled => {}
            }
        });
        self.share = Some(ShareInFlight { loader, result, _cancel: cancel, tmp_file });
    }

    /// senpi `restoreEditor`: dispose the loader, give the editor back, remove the temp export.
    fn restore_editor_after_share(&mut self) {
        if let Some(mut share) = self.share.take() {
            share.loader.dispose();
            // senpi ignores temp-file cleanup errors.
            let _ = std::fs::remove_file(&share.tmp_file);
        }
    }

    /// Settle a finished gist upload: report the share viewer URL or the gh failure.
    fn drain_share_result(&mut self) {
        let Some(share) = self.share.as_mut() else { return; };
        let output = match share.result.try_recv() {
            Ok(output) => output,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Err(std::io::Error::other("gist process ended without a result")),
        };
        self.restore_editor_after_share();
        match output {
            Err(error) => self.show_error(&format!("Failed to create gist: {error}")),
            Ok(output) if !output.status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                self.show_error(&format!("Failed to create gist: {}", if stderr.is_empty() { "Unknown error" } else { stderr.as_str() }));
            }
            Ok(output) => {
                let gist_url = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                match gist_url.rsplit('/').next().filter(|id| !id.is_empty()) {
                    Some(gist_id) => {
                        let preview_url = maho_core::config::get_share_viewer_url(gist_id);
                        self.show_status(format!("Share URL: {preview_url}\nGist: {gist_url}"));
                    }
                    None => self.show_error("Failed to parse gist ID from gh output"),
                }
            }
        }
    }: one row per provider and supported auth type, by name.
    fn login_provider_options(&self, auth_type: Option<crate::components::oauth_selector::AuthSelectorAuthType>) -> Vec<crate::components::oauth_selector::AuthSelectorProvider> {
        use crate::components::oauth_selector::AuthSelectorAuthType;
        let registry = self.session.model_registry();
        let mut options = maho_core::auth_providers::build_login_provider_infos(registry, None).into_iter().filter_map(|info| {
            let kind = match info.auth_type { maho_core::auth_storage::CredentialKind::Oauth => AuthSelectorAuthType::OAuth, maho_core::auth_storage::CredentialKind::ApiKey => AuthSelectorAuthType::ApiKey };
            if auth_type.is_some_and(|wanted| wanted != kind) { return None; }
            let status = registry.model_runtime.get_provider_auth_status(&info.id);
            let status = status.configured.then(|| maho_ai::auth::types::AuthCheck {
                auth_type: if registry.model_runtime.is_using_oauth(&info.id) { maho_ai::auth::types::AuthType::OAuth } else { maho_ai::auth::types::AuthType::ApiKey },
                source: status.label.or(status.source),
            });
            Some(crate::components::oauth_selector::AuthSelectorProvider { id: info.id, name: info.name, auth_type: kind, method_name: None, status })
        }).collect::<Vec<_>>();
        options.sort_by(|left, right| left.name.cmp(&right.name));
        options
    }

    /// senpi `getLogoutProviderOptions`: the stored credentials, listed under a 15 s abort.
    async fn get_logout_provider_options(&self) -> Result<Vec<crate::components::oauth_selector::AuthSelectorProvider>, String> {
        use crate::components::oauth_selector::{AuthSelectorAuthType, AuthSelectorProvider};
        let runtime = self.session.model_registry().model_runtime.clone();
        let abort = maho_ai::utils::abort::AbortController::new();
        let options = maho_ai::auth::types::AuthOperationOptions { signal: Some(abort.signal()) };
        let credentials = match tokio::time::timeout(std::time::Duration::from_secs(15), runtime.list_credentials(Some(options))).await {
            Ok(result) => result.map_err(|error| error.to_string())?,
            Err(_) => { abort.abort(None); return Err("The operation was aborted due to timeout".to_owned()); }
        };
        let registry = self.session.model_registry();
        let mut options = credentials.into_iter().map(|credential| {
            let auth_type = match credential.credential_type { maho_ai::auth::types::CredentialType::OAuth => AuthSelectorAuthType::OAuth, maho_ai::auth::types::CredentialType::ApiKey => AuthSelectorAuthType::ApiKey };
            AuthSelectorProvider {
                name: registry.get_provider_display_name(&credential.provider_id), id: credential.provider_id, auth_type, method_name: None,
                status: Some(maho_ai::auth::types::AuthCheck { auth_type: maho_ai::auth::types::AuthType::from(auth_type), source: Some("stored credential".to_owned()) }),
            }
        }).collect::<Vec<_>>();
        options.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(options)
    }

    /// senpi `showOAuthSelector("logout")`.
    async fn show_logout_selector(&mut self) {
        let options = match self.get_logout_provider_options().await {
            Ok(options) => options,
            Err(error) => { self.show_error(&format!("Could not read stored credentials: {error}")); return; }
        };
        if options.is_empty() { self.show_status("No stored credentials to remove. /logout only removes credentials saved by /login; environment variables and models.json config are unchanged.".into()); return; }
        let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
        let selected = self.ui_reply.clone(); let cancelled = self.ui_reply.clone(); let submissions = self.submissions.clone();
        self.ui_dialog = Some(Box::new(crate::components::oauth_selector::OAuthSelectorComponent::new(&self.theme, crate::components::oauth_selector::AuthSelectorMode::Logout, options,
            Box::new(move |id, _| { submissions.borrow_mut().push_back(format!("/logout-provider {id}")); selected.borrow_mut().take(); }),
            Box::new(move || { cancelled.borrow_mut().take(); }), None)));
    }

    /// senpi's logout selector callback: remove the chosen provider's stored credential.
    async fn logout_provider(&mut self, provider_id: &str) {
        let options = match self.get_logout_provider_options().await {
            Ok(options) => options,
            Err(error) => { self.show_error(&format!("Could not read stored credentials: {error}")); return; }
        };
        let Some(provider) = options.into_iter().find(|provider| provider.id == provider_id) else { return; };
        let runtime = self.session.model_registry().model_runtime.clone();
        let sender = self.auth_ui_sender.clone();
        let auth_type = provider.auth_type;
        tokio::spawn(async move {
            // senpi bounds logout with `AbortSignal.timeout(15_000)`.
            let abort = maho_ai::utils::abort::AbortController::new();
            let options = maho_ai::auth::types::AuthOperationOptions { signal: Some(abort.signal()) };
            let result = match tokio::time::timeout(std::time::Duration::from_secs(15), runtime.logout(&provider.id, options)).await {
                Ok(result) => result.map_err(|error| error.to_string()),
                Err(_) => { abort.abort(None); Err("The operation was aborted due to timeout".to_owned()) }
            };
            let _ = sender.send(AuthUiEvent::LogoutFinished { provider_name: provider.name, auth_type, result });
        });
    }

    /// senpi `showLoginAuthTypeSelector`.
    fn show_login_auth_type_selector(&mut self, provider_options: Option<Vec<crate::components::oauth_selector::AuthSelectorProvider>>) {
        use crate::components::oauth_selector::AuthSelectorAuthType;
        const SUBSCRIPTION: &str = "Sign in with an account";
        const API_KEY: &str = "Sign in with an API key";
        let available = provider_options.as_ref().map_or_else(|| vec![AuthSelectorAuthType::OAuth, AuthSelectorAuthType::ApiKey], |providers| providers.iter().map(|provider| provider.auth_type).collect());
        let mut labels = Vec::new();
        if available.contains(&AuthSelectorAuthType::OAuth) { labels.push(SUBSCRIPTION.to_owned()); }
        if available.contains(&AuthSelectorAuthType::ApiKey) { labels.push(API_KEY.to_owned()); }
        if labels.is_empty() { self.show_status("No login methods available.".into()); return; }
        if let Some(providers) = &provider_options && labels.len() == 1 && let Some(provider) = providers.first().cloned() { self.start_provider_login(&provider); return; }
        let title = provider_options.as_ref().and_then(|providers| providers.first()).map_or_else(|| "Select authentication method:".to_owned(), |provider| format!("Select authentication method for {}:", provider.name));
        let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
        let selected = self.ui_reply.clone(); let cancelled = self.ui_reply.clone(); let submissions = self.submissions.clone();
        let provider_id = provider_options.as_ref().and_then(|providers| providers.first()).map(|provider| provider.id.clone());
        self.ui_dialog = Some(Box::new(crate::components::extension_selector::ExtensionSelectorComponent::new(&self.theme, Arc::new(self.keybindings()), &title, labels,
            Box::new(move |label| {
                let method = if label == SUBSCRIPTION { "oauth" } else { "api_key" };
                submissions.borrow_mut().push_back(match &provider_id { Some(id) => format!("/login-provider {id} {method}"), None => format!("/login-select-provider {method}") });
                selected.borrow_mut().take();
            }),
            Box::new(move || { cancelled.borrow_mut().take(); }), Default::default())));
    }

    /// senpi `showLoginProviderSelector`.
    fn show_login_provider_selector(&mut self, auth_type: Option<crate::components::oauth_selector::AuthSelectorAuthType>, initial_search: Option<&str>) {
        use crate::components::oauth_selector::AuthSelectorAuthType;
        let options = self.login_provider_options(auth_type);
        if options.is_empty() {
            self.show_status(match auth_type { Some(AuthSelectorAuthType::OAuth) => "No subscription providers available.", Some(AuthSelectorAuthType::ApiKey) => "No API key providers available.", None => "No login providers available." }.into());
            return;
        }
        let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
        let selected = self.ui_reply.clone(); let cancelled = self.ui_reply.clone(); let submissions = self.submissions.clone(); let back = self.submissions.clone();
        self.ui_dialog = Some(Box::new(crate::components::oauth_selector::OAuthSelectorComponent::new(&self.theme, crate::components::oauth_selector::AuthSelectorMode::Login, options,
            Box::new(move |id, kind| { submissions.borrow_mut().push_back(format!("/login-provider {id} {}", match kind { AuthSelectorAuthType::OAuth => "oauth", AuthSelectorAuthType::ApiKey => "api_key" })); selected.borrow_mut().take(); }),
            // Cancelling a typed-provider list returns to the method selector, as pinned.
            Box::new(move || { if auth_type.is_some() { back.borrow_mut().push_back("/login-auth-type".into()); } cancelled.borrow_mut().take(); }),
            initial_search)));
    }

    /// senpi `startProviderLogin` -> `showLoginDialog` / `showApiKeyLoginDialog`: mount the dialog
    /// and run `modelRuntime.login` on the runtime, answering its prompts through the UI channel.
    fn start_provider_login(&mut self, provider: &crate::components::oauth_selector::AuthSelectorProvider) {
        use crate::components::oauth_selector::AuthSelectorAuthType;
        let method = match provider.auth_type { AuthSelectorAuthType::OAuth => crate::login_outcome::LoginMethod::Oauth, AuthSelectorAuthType::ApiKey => crate::login_outcome::LoginMethod::ApiKey };
        let mut dialog = crate::components::login_dialog::LoginDialogComponent::new(&self.theme, &provider.id, Box::new(|_, _| {}), Some(&provider.name), None);
        if provider.id == "amazon-bedrock" && provider.auth_type == AuthSelectorAuthType::ApiKey {
            dialog.show_details(&[
                self.theme.fg(crate::theme::ThemeColor::Text, "You can also use an AWS profile, IAM keys, or role-based credentials."),
                self.theme.fg(crate::theme::ThemeColor::Muted, "See:"),
                self.theme.fg(crate::theme::ThemeColor::Accent, &format!("  {}", std::path::Path::new(&maho_core::config::get_docs_path()).join("providers.md").display())),
            ]);
        }
        maho_tui::tui::Focusable::set_focused(&mut dialog, true);
        self.login_dialog = Some(dialog);
        let abort = maho_ai::utils::abort::AbortController::new();
        let interaction = Arc::new(InteractiveAuthInteraction { sender: self.auth_ui_sender.clone(), signal: abort.signal() });
        self.auth_abort = Some(abort);
        let runtime = self.session.model_registry().model_runtime.clone();
        let sender = self.auth_ui_sender.clone();
        let (provider_id, provider_name, auth_type) = (provider.id.clone(), provider.name.clone(), maho_ai::auth::types::AuthType::from(provider.auth_type));
        let event_provider_id = provider_id.clone();
        let session = self.session.clone();
        let previous_model = session.model();
        let session_id = session.session_id();
        let host = self.session_host.clone();
        tokio::spawn(async move {
            let result = runtime.login(&provider_id, auth_type, interaction).await.map(|_| ()).map_err(|error| {
                if error.message == crate::login_outcome::LOGIN_CANCELLED_NOTICE || error.message.contains("aborted") { crate::login_outcome::LoginFailure::Cancelled }
                else { crate::login_outcome::LoginFailure::Failed { message: error.message } }
            });
            let mut selection_error = None;
            if result.is_ok() && previous_model.provider == "unknown" && previous_model.id == "unknown"
                && session.model() == previous_model && session.session_id() == session_id {
                let models = runtime.get_available(Some(&provider_id)).await;
                let default_id = maho_core::model_resolver::DEFAULT_MODEL_PER_PROVIDER.iter().find(|(id, _)| *id == provider_id).map(|(_, model)| *model);
                if let Some(model) = models.iter().find(|model| Some(model.id.as_str()) == default_id)
                    .or_else(|| if provider_id == "radius" { models.first() } else { None }) {
                    let selected = match host { Some(host) => host.set_model(provider_id.clone(), model.id.clone()).await, None => session.set_model(model.clone()).await.map(|_| ()) };
                    if let Err(error) = selected { selection_error = Some(format!("Saved credentials for {provider_name}, but selecting its default model failed: {error}. Use /model to select a model.")); }
                }
            }
            let _ = sender.send(AuthUiEvent::LoginFinished { provider_id: event_provider_id, provider_name, method, result, selection_error });
        });
    }

    /// senpi `addContinuityNotice`: a muted single-line notice for degraded session continuity.
    fn add_continuity_notice(&mut self, message: &maho_ai::types::AssistantMessage) {
        let Ok(value) = serde_json::to_value(message) else { return; };
        let Some(notice) = self.continuity_notices.notice_for(&value, &self.theme) else { return; };
        self.chat.add_component(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1))));
        self.chat.add_component(Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding(notice, 1, 0))));
    }

    /// senpi `showNoticeBox`.
    fn show_notice_box(&mut self, mut spec: crate::components::earendil_announcement::NoticeSpec) {
        use crate::extension_error_format::sanitize_tui_error_message;
        spec.title = sanitize_tui_error_message(&spec.title);
        spec.why = sanitize_tui_error_message(&spec.why);
        for line in &mut spec.extra { line.text = sanitize_tui_error_message(&line.text); }
        spec.expanded_line = spec.expanded_line.as_deref().map(sanitize_tui_error_message);
        self.chat.add_component(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1))));
        self.chat.add_component(crate::components::earendil_announcement::build_notice_box(&spec, self.tools_expanded, &self.theme));
        self.editor_host.request_render();
    }


    /// senpi `showRiskyMainModelWarning`.
    fn show_risky_main_model_warning(&mut self, model: &maho_ai::model::Model) {
        if !crate::risky_main_model_warning::is_risky_main_model(model) { return; }
        self.show_notice_box(crate::components::earendil_announcement::NoticeSpec {
            title: "Risky model warning".into(), tone: Some(crate::theme::ThemeColor::Error),
            why: crate::risky_main_model_warning::RISKY_MAIN_MODEL_WARNING.into(), extra: Vec::new(), expanded_line: None,
        });
    }

    /// senpi `maybeWarnAboutAnthropicSubscriptionAuth`: the auth lookup runs off the UI loop (senpi's
    /// `void` promise) and reports back through the auth UI channel; lookup failures are ignored.
    fn maybe_warn_about_anthropic_subscription_auth(&mut self, model: maho_ai::model::Model) {
        let disabled = self.session.with_settings_manager(|settings| settings.get_value("warnings").and_then(|value| value.get("anthropicExtraUsage")).and_then(serde_json::Value::as_bool)) == Some(false);
        if disabled || self.anthropic_subscription_warning_shown || model.provider != "anthropic" { return; }
        let runtime = self.session.model_registry().model_runtime.clone();
        let sender = self.auth_ui_sender.clone();
        tokio::spawn(async move {
            let subscription = match runtime.check_auth("anthropic", None).await {
                Ok(Some(check)) if check.auth_type == maho_ai::auth::types::AuthType::OAuth => true,
                Ok(_) => matches!(runtime.get_auth(&model.provider).await, Ok(Some(resolution)) if is_anthropic_subscription_auth_key(resolution.auth.api_key.as_deref())),
                Err(_) => false,
            };
            if subscription { let _ = sender.send(AuthUiEvent::AnthropicSubscriptionAuth); }
        });
    }

    /// senpi `init`'s post-startup model warnings (`showRiskyMainModelWarning` +
    /// `maybeWarnAboutAnthropicSubscriptionAuth` for the session model).
    pub fn show_startup_model_warnings(&mut self) {
        let model = self.host_model();
        self.show_risky_main_model_warning(&model);
        self.maybe_warn_about_anthropic_subscription_auth(model);
    /// senpi `showWarning`.
    pub fn show_warning(&mut self, message: &str) {
        self.chat.add_component(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1))));
        self.chat.add_component(Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding(self.theme.fg(crate::theme::ThemeColor::Warning, &format!("Warning: {message}")), 1, 0))));
        self.editor_host.request_render();
    }

    /// senpi `showNewVersionNotification`: the "Update Available" notice box.
    pub fn show_new_version_notification(&mut self, new_version: &str, action: &str, changelog_url: &str) {
        use crate::components::earendil_announcement::{build_notice_box, NoticeLine, NoticeSpec};
        let link = if maho_tui::terminal_image::get_capabilities().hyperlinks { maho_tui::terminal_image::hyperlink(changelog_url, changelog_url) } else { changelog_url.to_owned() };
        let spec = NoticeSpec {
            title: "Update Available".into(), tone: Some(crate::theme::ThemeColor::Warning),
            why: crate::extension_error_format::sanitize_tui_error_message(&format!("New version {new_version} is available.")),
            extra: vec![NoticeLine { text: action.to_owned(), tone: None }, NoticeLine { text: format!("Changelog: {link}"), tone: Some(crate::theme::ThemeColor::Accent) }],
            expanded_line: None,
        };
        self.chat.add_component(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1))));
        self.chat.add_component(build_notice_box(&spec, self.tools_expanded, &self.theme));
    }

    fn show_error(&mut self, message: &str) {
        let text = self.theme.fg(crate::theme::ThemeColor::Error, &format!("Error: {}", crate::extension_error_format::sanitize_tui_error_message(message)));
        self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1)))));
        self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding(text, 1, 0)))));
    }

    /// Bridge the running login/logout tasks into the UI: mount prompts, forward answers, report outcomes.
    fn drain_auth_ui_events(&mut self) {
        let selected = self.pending_auth_select.as_ref().and_then(|(answer, _)| answer.borrow_mut().take());
        if let Some(value) = selected && let Some((_, reply)) = self.pending_auth_select.take() {
            let _ = reply.send(value);
        }
        if let Some((receiver, _)) = self.pending_auth_prompt.as_mut() {
            match receiver.try_recv() {
                Ok(value) => if let Some((_, reply)) = self.pending_auth_prompt.take() { let _ = reply.send(Ok(value)); },
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => if let Some((_, reply)) = self.pending_auth_prompt.take() { let _ = reply.send(Err(crate::login_outcome::LOGIN_CANCELLED_NOTICE.into())); },
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
            }
        }
        while let Ok(event) = self.auth_ui_events.try_recv() {
            self.handle_auth_ui_event(event);
        }
    }

    fn handle_auth_ui_event(&mut self, event: AuthUiEvent) {
            match event {
                AuthUiEvent::Prompt(prompt, reply) => self.show_auth_prompt(prompt, reply),
                AuthUiEvent::Notify(event) => self.notify_auth_dialog(event),
                AuthUiEvent::LoginFinished { provider_id, provider_name, method, result, selection_error } => {
                    let outcome = match &result {
                        Ok(()) => AuthLifecycleOutcome::Success,
                        Err(crate::login_outcome::LoginFailure::Cancelled | crate::login_outcome::LoginFailure::Abort { .. }) => AuthLifecycleOutcome::Cancelled,
                        Err(crate::login_outcome::LoginFailure::CredentialSynchronization { message } | crate::login_outcome::LoginFailure::Failed { message }) => {
                            if message == crate::login_outcome::LOGIN_CANCELLED_NOTICE { AuthLifecycleOutcome::Cancelled } else { AuthLifecycleOutcome::Error(message.clone()) }
                        }
                    };
                    let _ = self.auth_lifecycle_events.send(AuthLifecycleEvent::LoginCompleted { provider_name: provider_name.clone(), method, outcome: outcome.clone() });
                    self.auth_abort = None; self.login_dialog = None; self.pending_auth_prompt = None; self.pending_auth_select = None;
                    match result {
                        Ok(()) => {
                             self.complete_provider_authentication(provider_id, provider_name, method, selection_error);
                        }
                        Err(failure) => {
                            let notice = crate::login_outcome::describe_login_failure(Some(&failure), &provider_name, method);
                            match notice.level { crate::login_outcome::NoticeLevel::Status => self.show_status(notice.message), crate::login_outcome::NoticeLevel::Error => self.show_error(&notice.message) }
                        }
                    }
                }
                AuthUiEvent::LogoutFinished { provider_name, auth_type, result } => {
                    let _ = self.auth_lifecycle_events.send(AuthLifecycleEvent::LogoutCompleted { provider_name: provider_name.clone(), auth_type, outcome: result.clone() });
                    match result {
                    Ok(()) => {
                        // senpi `updateAvailableProviderCount` after a successful logout.
                        self.footer_data.set_available_provider_count(self.host_available_models().iter().map(|model| &model.provider).collect::<std::collections::BTreeSet<_>>().len());
                        if let Some(footer) = &mut self.footer { footer.invalidate(); }
                        self.show_status(match auth_type {
                        crate::components::oauth_selector::AuthSelectorAuthType::OAuth => format!("Logged out of {provider_name}"),
                        crate::components::oauth_selector::AuthSelectorAuthType::ApiKey => format!("Removed stored API key for {provider_name}. Environment variables and models.json config are unchanged."),
                    });
                    }
                    Err(error) => self.show_error(&format!("Logout failed: {error}")),
                    }
                },
                AuthUiEvent::AnthropicSubscriptionAuth => {
                    if !self.anthropic_subscription_warning_shown {
                        self.anthropic_subscription_warning_shown = true;
                        self.show_warning(ANTHROPIC_SUBSCRIPTION_AUTH_WARNING);
                    }
                }
                AuthUiEvent::LoginCatalogFinished { provider_name, action, result, selection_error, selected_model } => {
                    match &result {
                        Ok(()) => self.show_status(format!("{action}. Credentials saved to {}", maho_core::config::get_auth_path())),
                        Err(error) if error == "aborted" => self.show_status(format!("{action}, but its model catalog refresh timed out; using cached models.")),
                        Err(_) => self.show_status(format!("{action}, but its model catalog could not be refreshed; using cached models.")),
                    }
                    if let Some(error) = &selection_error { self.show_error(error); }
                    match &selected_model {
                        Some(model) => { self.show_risky_main_model_warning(model); self.maybe_warn_about_anthropic_subscription_auth(model.clone()); }
                        None if selection_error.is_none() => { let model = self.host_model(); self.maybe_warn_about_anthropic_subscription_auth(model); }
                        None => {}
                    }
                    self.footer_data.set_available_provider_count(self.host_available_models().iter().map(|model| &model.provider).collect::<std::collections::BTreeSet<_>>().len());
                    if let Some(footer) = &mut self.footer { footer.invalidate(); }
                    let _ = self.auth_lifecycle_events.send(AuthLifecycleEvent::LoginCatalogCompleted { provider_name, outcome: result, selection_error });
                }
            }
    }

    /// senpi `showAuthPrompt`: select prompts use a selector, manual codes the manual input, the rest the dialog prompt.
    fn show_auth_prompt(&mut self, prompt: maho_ai::auth::types::AuthPrompt, reply: tokio::sync::oneshot::Sender<Result<String, String>>) {
        use maho_ai::auth::types::AuthPromptKind;
        if prompt.signal.as_ref().is_some_and(maho_ai::utils::abort::AbortSignal::aborted) { let _ = reply.send(Err(crate::login_outcome::LOGIN_CANCELLED_NOTICE.into())); return; }
        let _ = self.auth_lifecycle_events.send(AuthLifecycleEvent::PromptReady);
        match prompt.kind {
            AuthPromptKind::Select { message, options } => {
                let labels = options.iter().map(|option| option.label.clone()).collect::<Vec<_>>();
                let answer: AuthSelectReply = Rc::new(RefCell::new(None));
                let selected = answer.clone(); let cancelled = answer.clone();
                // Keep the selector mounted until it answers, like every other local dialog.
                let (close, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(close);
                let select_closed = self.ui_reply.clone(); let cancel_closed = self.ui_reply.clone();
                self.ui_dialog = Some(Box::new(crate::components::extension_selector::ExtensionSelectorComponent::new(&self.theme, Arc::new(self.keybindings()), &message, labels,
                    Box::new(move |label| { *selected.borrow_mut() = Some(options.iter().find(|option| option.label == label).map(|option| option.id.clone()).ok_or_else(|| crate::login_outcome::LOGIN_CANCELLED_NOTICE.to_owned())); select_closed.borrow_mut().take(); }),
                    Box::new(move || { *cancelled.borrow_mut() = Some(Err(crate::login_outcome::LOGIN_CANCELLED_NOTICE.into())); cancel_closed.borrow_mut().take(); }), Default::default())));
                self.pending_auth_select = Some((answer, reply));
            }
            kind => {
                let Some(dialog) = self.login_dialog.as_mut() else { let _ = reply.send(Err(crate::login_outcome::LOGIN_CANCELLED_NOTICE.into())); return; };
                let receiver = match kind {
                    AuthPromptKind::ManualCode { message, .. } => dialog.show_manual_input(&message),
                    AuthPromptKind::Text { message, placeholder } | AuthPromptKind::Secret { message, placeholder } => dialog.show_prompt(&message, placeholder.as_deref()),
                    AuthPromptKind::Select { .. } => unreachable!("select prompts are handled above"),
                };
                self.pending_auth_prompt = Some((receiver, reply));
            }
        }
    }

    /// senpi `notifyAuthDialog`.
    fn notify_auth_dialog(&mut self, event: maho_ai::auth::types::AuthEvent) {
        if let Some(dialog) = &mut self.login_dialog {
            use maho_ai::auth::types::AuthEvent;
            match event {
                AuthEvent::AuthUrl { url, instructions } => dialog.show_auth(&url, instructions.as_deref()),
                AuthEvent::DeviceCode { user_code, verification_uri, .. } => {
                    dialog.show_device_code(&crate::components::login_dialog::OAuthDeviceCodeInfo { user_code, verification_uri });
                    dialog.show_waiting("Waiting for authentication...");
                }
                AuthEvent::Info { message, links } => dialog.show_info(&message, &links.unwrap_or_default(), false),
                AuthEvent::Progress { message } => dialog.show_progress(&message),
            }
        }
    }

    /// senpi `showStatusIndicator`: replace the active indicator. Retry and branch-summary embed in
    /// the editor border at render time; compaction keeps its own status row.
    fn show_status_indicator(&mut self, indicator: crate::components::status_indicator::StatusIndicator) {
        if let Some(previous) = self.active_status_indicator.take() { previous.borrow_mut().dispose(); }
        self.idle_status = None;
        self.active_status_indicator = Some(Rc::new(RefCell::new(indicator)));
    }

    /// senpi `clearStatusIndicator(kind)`: clear only when the active indicator matches `kind`.
    fn clear_status_indicator(&mut self, kind: Option<crate::components::status_indicator::StatusIndicatorKind>) {
        use crate::components::status_indicator::{IdleStatus, StatusIndicatorKind};
        let Some(active) = self.active_status_indicator.clone() else { return; };
        let active_kind = active.borrow().kind;
        if kind.is_some_and(|kind| kind != active_kind) { return; }
        // senpi reserves the cleared row's height under `clearOnShrink` in regular mode; only the
        // standalone compaction row occupies dock rows here (the others live in the editor border).
        let (_, clear_on_shrink, _) = self.terminal_settings();
        let reserved = (clear_on_shrink && active_kind == StatusIndicatorKind::Compaction).then(|| {
            let (columns, rows) = self.terminal_dimensions.get();
            let rendered = active.borrow_mut().render(usize::from(columns)).len();
            IdleStatus { height: usize::from(rows).min(rendered.max(1)) }
        });
        active.borrow_mut().dispose();
        self.active_status_indicator = None;
        self.idle_status = reserved;
    }

    fn status_indicator_now(&self) -> u64 { self.clock.elapsed().as_millis() as u64 }

    fn show_retry_status_indicator(&mut self, attempt: u32, max_attempts: u32, delay_ms: u64, error_message: &str) {
        let trouble = crate::provider_error_presentation::is_network_provider_error(Some(error_message), false);
        let indicator = crate::components::status_indicator::StatusIndicator::retry(attempt as usize, max_attempts as usize, delay_ms, trouble, &crate::components::keybinding_hints::key_text("app.interrupt"), self.theme.clone(), self.status_indicator_now());
        self.show_status_indicator(indicator);
    }

    fn show_compaction_status_indicator(&mut self, reason: maho_ext_api::CompactionReason) {
        use crate::components::status_indicator::CompactionStatusReason;
        let reason = match reason {
            maho_ext_api::CompactionReason::Manual => CompactionStatusReason::Manual,
            maho_ext_api::CompactionReason::Threshold => CompactionStatusReason::Threshold,
            maho_ext_api::CompactionReason::Overflow => CompactionStatusReason::Overflow,
            maho_ext_api::CompactionReason::PrePrompt => CompactionStatusReason::PrePrompt,
            maho_ext_api::CompactionReason::Branch => CompactionStatusReason::Branch,
            maho_ext_api::CompactionReason::Extension => CompactionStatusReason::Extension,
        };
        let indicator = crate::components::status_indicator::StatusIndicator::compaction(reason, &crate::components::keybinding_hints::key_text("app.interrupt"), self.theme.clone(), self.status_indicator_now());
        self.show_status_indicator(indicator);
    }

    fn show_branch_summary_status_indicator(&mut self) {
        let indicator = crate::components::status_indicator::StatusIndicator::branch_summary(&crate::components::keybinding_hints::key_text("app.interrupt"), self.theme.clone(), self.status_indicator_now());
        self.show_status_indicator(indicator);
    }

    pub fn handle_session_event(&mut self, event: &maho_ext_api::AgentSessionEvent) {
        use maho_ext_api::AgentSessionEvent;
        use crate::components::status_indicator::StatusIndicatorKind;
        match event {
            AgentSessionEvent::Agent(event) => {
                // senpi `message_update`: visible assistant text ends a pending retry indicator.
                if let AgentEvent::MessageUpdate { message, .. } = event
                    && message.as_assistant().is_some_and(|assistant| assistant.content.iter().any(|block| matches!(block, maho_ai::types::ContentBlock::Text(text) if !text.text.trim().is_empty()))) {
                    self.clear_status_indicator(Some(StatusIndicatorKind::Retry));
                }
                self.handle_event(event);
            }
            AgentSessionEvent::AgentEnd { messages, .. } => self.handle_event(&AgentEvent::AgentEnd { messages: messages.clone() }),
            AgentSessionEvent::ContinuationError { error_message } => self.show_status(error_message.clone()),
            AgentSessionEvent::ModelChangePending { notice, .. } | AgentSessionEvent::ResumeCompactionRequired { notice, .. } | AgentSessionEvent::ResumeContextReduced { notice, .. } => self.show_status(notice.clone()),
            AgentSessionEvent::ModelChangeRejected { detail, .. } => self.show_status(detail.clone()),
            AgentSessionEvent::ThinkingLevelChanged { level, .. } => self.show_status(format!("Thinking level: {}", serde_json::to_value(level).expect("level").as_str().expect("string"))),
            AgentSessionEvent::AutoRetryStart { attempt, max_attempts, error_message, .. } => self.show_status(format!("Retrying ({attempt}/{max_attempts}): {error_message}")),
            AgentSessionEvent::AutoRetryEnd { final_error:Some(error), .. } => self.show_status(error.clone()),
            AgentSessionEvent::CompactionStart { .. } => self.show_status("Compacting context...".into()),
            AgentSessionEvent::CompactionEnd { error_message:Some(error), .. } => self.show_status(error.clone()),
            AgentSessionEvent::AgentIdle | AgentSessionEvent::AgentSettled | AgentSessionEvent::SessionAbort => { self.agent_idle = true; self.working_started_ms = None; }
            AgentSessionEvent::ThinkingLevelChanged { level, .. } => self.show_status(format!("Thinking level: {}", serde_json::to_value(level).expect("level").as_str().expect("string"))),
            AgentSessionEvent::AutoRetryStart { attempt, max_attempts, delay_ms, error_message } => self.show_retry_status_indicator(*attempt, *max_attempts, *delay_ms, error_message),
            AgentSessionEvent::AutoRetryEnd { success, attempt, final_error } => {
                self.clear_status_indicator(Some(StatusIndicatorKind::Retry));
                if !*success && final_error.as_deref() != Some("Retry cancelled") {
                    self.show_error(&format!("Retry failed after {attempt} attempts: {}", final_error.as_deref().filter(|error| !error.is_empty()).unwrap_or("Unknown error")));
                }
            }
            AgentSessionEvent::SummarizationRetryScheduled { attempt, max_attempts, delay_ms, error_message } => { self.show_error(error_message); self.show_retry_status_indicator(*attempt, *max_attempts, *delay_ms, error_message); }
            AgentSessionEvent::SummarizationRetryAttemptStart { source, reason } => {
                self.clear_status_indicator(Some(StatusIndicatorKind::Retry));
                if source == "branchSummary" { self.show_branch_summary_status_indicator(); }
                else { self.show_compaction_status_indicator(reason.unwrap_or(maho_ext_api::CompactionReason::Manual)); }
            }
            AgentSessionEvent::SummarizationRetryFinished => self.clear_status_indicator(Some(StatusIndicatorKind::Retry)),
            AgentSessionEvent::CompactionStart { reason, .. } => self.show_compaction_status_indicator(*reason),
            AgentSessionEvent::CompactionProgress { delta, text, .. } => {
                if let Some(indicator) = self.active_status_indicator.clone().filter(|indicator| indicator.borrow().kind == StatusIndicatorKind::Compaction) {
                    let next = text.clone().or_else(|| delta.clone()).unwrap_or_default();
                    if !next.is_empty() {
                        let tail = next.char_indices().rev().nth(3_999).map_or(next.as_str(), |(index, _)| &next[index..]);
                        let preview = if tail.len() < next.len() { format!("...{tail}") } else { next.clone() };
                        indicator.borrow_mut().set_progress_text(&maho_tui::terminal_text::sanitize_terminal_label(&preview), self.status_indicator_now());
                    }
                }
            }
            AgentSessionEvent::CompactionEnd { result, will_retry, accepted, error_message, .. } => {
                self.clear_status_indicator(Some(StatusIndicatorKind::Compaction));
                if let Some(error) = error_message { self.show_status(error.clone()); }
                // senpi: every terminal compaction_end resolves the compaction queue.
                if *accepted == Some(true) || result.is_some() {
                    self.start_compaction_flush(crate::compaction_queue_transfer::TransferOptions { will_retry: *will_retry, defer_admission: false });
                } else if *will_retry {
                    let held = self.compaction_queue.borrow().queued.len();
                    if held > 0 { self.show_status(format!("{held} queued message{} will send with the next turn (compaction will retry)", if held == 1 { "" } else { "s" })); }
                    self.start_compaction_flush(crate::compaction_queue_transfer::TransferOptions { will_retry: true, defer_admission: true });
                } else {
                    let restored = self.restore_queued_messages_to_editor(false);
                    if restored > 0 { self.show_status(format!("{restored} queued message{} restored to the editor (compaction did not complete)", if restored == 1 { "" } else { "s" })); }
                }
            }
            AgentSessionEvent::BashExecutionUpdate { delta, .. } => { if let Some((component, _)) = &self.pending_bash { component.borrow_mut().append_output(delta); } }
            AgentSessionEvent::AgentIdle | AgentSessionEvent::AgentSettled => { self.agent_idle = true; self.working_started_ms = None; }
            AgentSessionEvent::SessionAbort => { self.agent_idle = true; self.working_started_ms = None; self.clear_status_indicator(Some(StatusIndicatorKind::Retry)); }
            _ => {}
        }
    }

    fn settle_questions(&mut self) {
        self.handle_question_actions();
        let expanded_result = self.question_result.borrow_mut().take().filter(|_| self.questions.surface == crate::question_registry::QuestionSurface::Expanded);
        if let Some(response) = expanded_result {
            match self.questions.shown_id().map(str::to_owned) {
                Some(id) if response.status != maho_ext_api::QuestionStatus::Cancelled => self.finish_question(&id, response),
                _ => self.collapse_shown_question(),
            }
        }
        let sent = self.question_reply.borrow().is_none();
        let closed = self.question_reply.borrow().as_ref().is_some_and(tokio::sync::oneshot::Sender::is_closed);
        if closed || (self.blocking_question && sent) {
            self.question_reply.borrow_mut().take();
            if self.blocking_question { self.blocking_question = false; self.question = None; }
        }
        let abandoned: Vec<String> = self.questions.order().iter().filter(|id| self.questions.get(id).is_some_and(|entry| !entry.replies.is_empty() && entry.replies.iter().all(tokio::sync::oneshot::Sender::is_closed))).cloned().collect();
        for id in abandoned {
            let response = self.cancelled_question_response(&id);
            self.finish_question(&id, response);
        }
        if self.questions.is_empty() && self.question_reply.borrow().is_none() && !self.queued_questions.is_empty() {
            while let Some(request) = self.queued_questions.pop_front() {
                self.handle_ui_request(request);
                if self.question_reply.borrow().is_some() || !self.questions.is_empty() { break; }
            }
        }
    }

    fn drain_ui_requests(&mut self) {
        while let Ok(request) = self.ui_requests.try_recv() { self.handle_ui_request(request); }
        self.settle_questions();
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let mut index = 0;
        while index < self.custom_ui_builds.len() {
            match self.custom_ui_builds[index].as_mut().poll(&mut context) {
                std::task::Poll::Pending => index += 1,
                std::task::Poll::Ready(result) => {
                    drop(self.custom_ui_builds.remove(index));
                    match result {
                        Ok(mut component) => {
                            if self.custom_ui_result.borrow().is_some() { component.dispose(); }
                            else { self.ui_dialog = Some(component); }
                        }
                        Err(error) => { if let Some(reply) = self.custom_ui_reply.take() { drop(reply.send(Err(error))); } }
                    }
                }
            }
        }
        let closed = self.ui_reply.borrow().as_ref().is_some_and(tokio::sync::oneshot::Sender::is_closed);
        if closed { self.ui_reply.borrow_mut().take(); if let Some(mut dialog) = self.ui_dialog.take() { dialog.dispose(); } }
        let result = self.custom_ui_result.borrow_mut().take();
        if let Some(result) = result {
            if let Some(handle) = self.custom_overlay.take() { handle.hide(); }
            self.custom_ui_builds.clear();
            if let Some(mut component) = self.ui_dialog.take() { component.dispose(); }
            if let Some(reply) = self.custom_ui_reply.take() { drop(reply.send(Ok(result))); }
        }
    }

    pub fn handle_ui_request(&mut self, request: crate::interactive_extension_ui::UiRequest) {
        use crate::interactive_extension_ui::UiRequest;
        match request {
            UiRequest::WidgetFrame => self.editor_host.request_render(),
            UiRequest::WidgetFactory(key, factory, options) => {
                let host = crate::interactive_ui_host::InteractiveUiHost(self.editor_host.clone(),self.terminal_dimensions.clone());
                let theme = maho_ext_api::ExtensionUi::theme(self.extension_ui.as_ref());
                let component = factory.map(|factory| factory(&host, &theme));
                if let Some(index) = self.widgets.iter().position(|(name,_,_)| name == &key) { let (_,mut previous,_) = self.widgets.remove(index); previous.dispose(); }
                if let Some(component) = component { self.widgets.push((key,component,options.placement)); }
            }
            UiRequest::HeaderFactory(factory) => {
                let host = crate::interactive_ui_host::InteractiveUiHost(self.editor_host.clone(),self.terminal_dimensions.clone());
                let header = factory.map(|factory| -> HeaderSlot { let component: Box<dyn Component> = factory(&host, &maho_ext_api::ExtensionUi::theme(self.extension_ui.as_ref())); Rc::new(RefCell::new(crate::interactive_ui_host::BoxedComponent(component))) });
                self.set_extension_header(header);
            }
            UiRequest::FooterFactory(factory) => {
                let host = crate::interactive_ui_host::InteractiveUiHost(self.editor_host.clone(),self.terminal_dimensions.clone());
                self.footer_data.set_available_provider_count(self.host_available_models().iter().map(|model|&model.provider).collect::<std::collections::BTreeSet<_>>().len());
                let data = crate::interactive_ui_host::InteractiveFooterData { provider:self.footer_data.clone(), ui:self.extension_ui.clone() };
                self.footer = factory.map(|factory| factory(&host,&maho_ext_api::ExtensionUi::theme(self.extension_ui.as_ref()),&data));
            }
            UiRequest::CustomFactory(factory, options, reply) => {
                if options.overlay && self.mounted_renderer.is_none() { drop(reply.send(Err(maho_ext_api::ExtensionFailure::new("Custom overlay requires the mounted terminal renderer")))); }
                else {
                    let host = crate::interactive_ui_host::InteractiveUiHost(self.editor_host.clone(),self.terminal_dimensions.clone());
                    let theme = maho_ext_api::ExtensionUi::theme(self.extension_ui.as_ref());
                    let keys = self.keybindings(); let result = self.custom_ui_result.clone();
                    self.custom_ui_reply = Some(reply);
                    let renderer = self.mounted_renderer.as_ref().and_then(std::rc::Weak::upgrade);
                    self.custom_ui_builds.push(Box::pin(async move {
                        let component = factory(&host,&theme,&keys,Rc::new(move |value| { *result.borrow_mut() = Some(value); })).await?;
                        if options.overlay && let Some(renderer) = renderer {
                            let overlay_options = options.overlay_options.map(|options| match options { maho_ext_api::ExtensionOverlayOptions::Static(factory) | maho_ext_api::ExtensionOverlayOptions::Dynamic(factory) => factory() });
                            let component = Rc::new(RefCell::new(crate::interactive_ui_host::BoxedComponent(component)));
                            let handle = renderer.borrow_mut().base_mut().show_overlay(component.clone(),overlay_options);
                            if let Some(callback) = options.on_handle { callback(handle); }
                            Ok(Box::new(crate::interactive_ui_host::OverlayComponent { component, renderer:Rc::downgrade(&renderer) }) as Box<dyn Component>)
                        } else { Ok(component) }
                    }));
                }
            }
            UiRequest::WorkingIndicator(options) => self.working_indicator = options,
            UiRequest::Autocomplete(factory) => {
                self.autocomplete_wrappers.push(factory);
                self.setup_autocomplete();
            }
            UiRequest::EditorFactory(factory) => {
                let mut replacement = factory.map(|factory| factory(self.editor_host.clone(), editor_theme(&self.theme), &self.keybindings()));
                match (&mut self.custom_editor, &mut replacement) {
                    (Some(source), Some(target)) => { crate::editor_paste_transfer::transfer_editor_content(source.as_ref(), target.as_mut()); }
                    (Some(source), None) => { crate::editor_paste_transfer::transfer_editor_content(source.as_ref(), &mut self.editor); }
                    (None, Some(target)) => { crate::editor_paste_transfer::transfer_editor_content(&self.editor, target.as_mut()); }
                    (None, None) => {}
                }
                // senpi `setCustomEditor`: route the new editor's image-marker changes and undo
                // payload snapshots into the pending images, and copy the default border color.
                if let Some(target) = &mut replacement {
                    if target.get_image_marker_state().is_some() {
                        let images = self.pending_images.clone();
                        target.set_on_image_markers_changed(Some(Box::new(move |order| {
                            let previous = std::mem::take(&mut *images.borrow_mut());
                            images.borrow_mut().extend(order.iter().enumerate().filter_map(|(index, id)| {
                                previous.get(id).cloned().map(|image| (u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1), image))
                            }));
                        })));
                        let images = self.pending_images.clone();
                        target.set_snapshot_attachment_state(Some(Box::new(move || Some(Rc::new(images.borrow().clone())))));
                        let images = self.pending_images.clone();
                        target.set_restore_attachment_state(Some(Box::new(move |snapshot| {
                            if let Some(snapshot) = snapshot.downcast_ref::<BTreeMap<u64, maho_ai::types::ImageContent>>() { *images.borrow_mut() = snapshot.clone(); }
                        })));
                    }
                    if target.border_color().is_some() { target.set_border_color(self.editor.editor.border_color.clone()); }
                }
                if let Some(mut editor) = self.custom_editor.take() { editor.dispose(); }
                self.custom_editor = replacement;
            }
            UiRequest::SettingChanged(key, value) => {
                match key.as_str() {
                    "editorPaddingX" => self.editor.set_padding_x(value.as_u64().expect("editor padding") as usize),
                    "autocompleteMaxVisible" => self.editor.editor.set_autocomplete_max_visible(value.as_u64().expect("autocomplete limit") as usize),
                    "enableSkillCommands" => self.setup_autocomplete(),
                    "showImages" => { for component in &self.tool_cards { component.borrow_mut().set_show_images(value.as_bool().expect("show images")); } },
                    "imageWidthCells" => { for component in &self.tool_cards { component.borrow_mut().set_image_width_cells(u32::try_from(value.as_u64().expect("image width")).expect("selector image width")); } },
                    "hideThinkingBlock" => { self.reveal.hide_thinking = value.as_bool().expect("thinking visibility"); for component in &self.assistant_cards { component.borrow_mut().set_hide_thinking_block(self.reveal.hide_thinking); } if let Some(component) = &self.streaming { let content = self.reveal.resync_visibility(self.clock.elapsed().as_secs_f64() * 1000.0); component.borrow_mut().update_content(&content, Some(true)); } },
                    "smoothStreaming" => { let smooth = value.as_bool().expect("smooth streaming"); self.reveal.smooth = smooth; self.tool_args_reveal.smooth = smooth; self.tool_reveal.smooth = smooth; self.apply_smooth_streaming_render_fps(); self.tick(self.clock.elapsed().as_secs_f64() * 1000.0); },
                    "smoothStreamingFps" => { let fps = value.as_f64().expect("streaming fps"); self.reveal.fps = fps; self.tool_args_reveal.fps = fps; self.tool_reveal.fps = fps; self.apply_smooth_streaming_render_fps(); },
                    "theme" => {
                        let setting = value.as_str().expect("theme setting");
                        let name = crate::theme::resolve_theme_setting(Some(setting), self.terminal_theme).unwrap_or("dark").to_owned();
                        self.apply_theme_name(&name);
                        // senpi `setThemeSetting` -> `applyFromSettings`: an auto setting re-detects and follows.
                        self.request_terminal_theme_detection(Some(setting.to_owned()));
                    }
                    "outputPad" => { for component in &self.assistant_cards { component.borrow_mut().set_output_pad(value.as_u64().expect("output padding") as usize); } if self.agent_idle { self.rebuild_history(); } },
                    "showCacheMissNotices" if self.agent_idle => self.rebuild_history(),
                    "fullscreenCopyOnSelect" => {
                        let enabled = value.as_bool().unwrap_or(true);
                        if let Some(renderer) = self.mounted_renderer.as_ref().and_then(std::rc::Weak::upgrade) { renderer.borrow_mut().set_copy_on_select(enabled); }
                    }
                    _ => self.chat.invalidate(),
                }
            }
            UiRequest::Question { request, options, reply } => {
                if reply.is_closed() { return; }
                use crate::components::ask_user_question_state as state;
                let convert = |request: maho_ext_api::QuestionRequest| state::QuestionRequest { request_id:request.request_id, wait_for_answer:request.wait_for_answer, timeout_ms:request.timeout_ms,
                    questions:request.questions.into_iter().map(|q| state::Question { id:q.id, header:q.header, question:q.question, multi_select:q.multi_select, options:q.options.into_iter().map(|o| state::QuestionOption { label:o.label, description:o.description }).collect() }).collect() };
                if options.deliver == maho_ext_api::QuestionDelivery::UserMessage || !request.wait_for_answer {
                    let request = convert(request);
                    self.show_async_question(request, options, reply);
                    return;
                }
                if self.question_reply.borrow().is_some() {
                    self.queued_questions.push_back(UiRequest::Question { request, options, reply }); return;
                }
                let request = convert(request);
                *self.question_reply.borrow_mut() = Some(reply); let answer = self.question_reply.clone();
                self.blocking_question = true;
                let mut component_options = crate::components::ask_user_question::AskUserQuestionOptions::new(self.theme.clone());
                component_options.now_ms = self.clock.elapsed().as_millis() as u64;
                component_options.timeout_ms = options.dialog.timeout_ms;
                component_options.get_deadline_at_ms = crate::question_registry::question_deadline(&options, component_options.now_ms).map(|deadline| Box::new(move || deadline()) as Box<dyn Fn() -> u64>);
                component_options.initial_draft = options.initial_draft.clone().map(crate::question_registry::question_draft);
                component_options.on_progress = options.on_progress.map(|callback| Box::new(move |draft: &state::QuestionDraft| callback(maho_ext_api::QuestionDraft { comment:draft.comment.clone(), answers:Some(draft.answers.iter().map(|(id, answer)| (id.clone(), maho_ext_api::QuestionAnswer { selected:answer.selected.clone(), text:answer.text.clone() })).collect()) })) as crate::components::ask_user_question::ProgressCallback);
                self.question = Some(crate::components::ask_user_question::AskUserQuestionComponent::new(request, Box::new(move |response| {
                    let status = match response.status { state::QuestionStatus::Answered => maho_ext_api::QuestionStatus::Answered, state::QuestionStatus::CommentSubmitted => maho_ext_api::QuestionStatus::CommentSubmitted, state::QuestionStatus::Cancelled => maho_ext_api::QuestionStatus::Cancelled, state::QuestionStatus::TimedOut => maho_ext_api::QuestionStatus::TimedOut };
                    let response = maho_ext_api::QuestionResponse { status, comment:response.comment, unanswered:response.unanswered, auto_resolved_after_ms:response.auto_resolved_after_ms,
                        answers:response.answers.into_iter().map(|(id, value)| (id, maho_ext_api::QuestionAnswer { selected:value.selected, text:value.text })).collect() };
                    if let Some(reply) = answer.borrow_mut().take() { drop(reply.send(response)); }
                }), component_options));
            }
            UiRequest::ToolsExpanded(expanded) => self.set_tools_expanded(expanded),
            UiRequest::HiddenThinkingLabel(label) => { self.hidden_thinking_label = label.unwrap_or_else(|| "Thinking...".into()); for card in &self.assistant_cards { card.borrow_mut().set_hidden_thinking_label(&self.hidden_thinking_label); } }
            UiRequest::Editor { title, prefill, reply } => {
                *self.ui_reply.borrow_mut() = Some(reply);
                let selected = self.ui_reply.clone(); let cancelled = selected.clone();
                self.ui_dialog = Some(Box::new(crate::components::extension_editor::ExtensionEditorComponent::new(&self.theme, self.editor_host.clone(),
                    Arc::new(self.keybindings()), &title, prefill.as_deref(),
                    Box::new(move |text| { if let Some(reply) = selected.borrow_mut().take() { drop(reply.send(Some(text.into()))); } }),
                    Box::new(move || { if let Some(reply) = cancelled.borrow_mut().take() { drop(reply.send(None)); } }), Default::default(), None, None)));
            }
            UiRequest::WorkingMessage(message) => self.working_message = message,
            UiRequest::WorkingVisible(visible) => self.working_visible = visible,
            UiRequest::Notify(text, _) => self.show_status(text),
            UiRequest::Title(title) => self.terminal_title = Some(title),
            UiRequest::EditorText(text) => self.editor.editor.set_text(&text),
            UiRequest::Paste(text) => self.editor.editor.insert_text_at_cursor(&text),
            UiRequest::Header(factory) => {
                let header = factory.map(|factory| -> HeaderSlot { let component: Box<dyn Component> = factory(&maho_ext_api::ExtensionUi::theme(self.extension_ui.as_ref())); Rc::new(RefCell::new(crate::interactive_ui_host::BoxedComponent(component))) });
                self.set_extension_header(header);
            },
            UiRequest::Footer(factory) => self.footer = factory.map(|factory| factory(&maho_ext_api::ExtensionUi::theme(self.extension_ui.as_ref()))),
            UiRequest::Widget(key, content, options) => {
                let index = self.widgets.iter().position(|(name, _, _)| name == &key);
                if let Some(content) = content {
                    let component: Box<dyn Component> = match content {
                        maho_ext_api::WidgetContent::Lines(lines) => {
                            let mut container = Container::new();
                            for line in lines.iter().take(10) { container.add_child(Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding(line.clone(), 1, 0)))); }
                            if lines.len() > 10 { container.add_child(Rc::new(RefCell::new(maho_tui::components::text::Text::with_padding(self.theme.fg(crate::theme::ThemeColor::Muted, "... (widget truncated)"), 1, 0)))); }
                            Box::new(container)
                        }
                        maho_ext_api::WidgetContent::Component(factory) => factory(&maho_ext_api::ExtensionUi::theme(self.extension_ui.as_ref())),
                    };
                    if let Some(index) = index { self.widgets[index].1.dispose(); self.widgets[index] = (key, component, options.placement); }
                    else { self.widgets.push((key, component, options.placement)); }
                } else if let Some(index) = index { let (_, mut component, _) = self.widgets.remove(index); component.dispose(); }
            }
            UiRequest::Select { title, options, reply } => {
                *self.ui_reply.borrow_mut() = Some(reply);
                let selected = self.ui_reply.clone(); let cancelled = self.ui_reply.clone();
                let keys = Arc::new(self.keybindings());
                self.ui_dialog = Some(Box::new(crate::components::extension_selector::ExtensionSelectorComponent::new(&self.theme, keys, &title, options,
                    Box::new(move |text| { if let Some(reply) = selected.borrow_mut().take() { drop(reply.send(Some(text.into()))); } }),
                    Box::new(move || { if let Some(reply) = cancelled.borrow_mut().take() { drop(reply.send(None)); } }), Default::default())));
            }
            UiRequest::Input { title, reply } => {
                *self.ui_reply.borrow_mut() = Some(reply);
                let selected = self.ui_reply.clone(); let cancelled = self.ui_reply.clone();
                self.ui_dialog = Some(Box::new(crate::components::extension_input::ExtensionInputComponent::new(&self.theme, &title,
                    Box::new(move |text| { if let Some(reply) = selected.borrow_mut().take() { drop(reply.send(Some(text.into()))); } }),
                    Box::new(move || { if let Some(reply) = cancelled.borrow_mut().take() { drop(reply.send(None)); } }), Default::default())));
            }
        }
        *self.extension_ui.editor_text.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = self.editor.editor.get_text();
    }

    /// senpi's `showAsyncQuestion`: every async request stays pending at once, and a duplicate
    /// request id shares the existing question's completion instead of showing a second card.
    fn show_async_question(&mut self, request: crate::components::ask_user_question_state::QuestionRequest, options: maho_ext_api::QuestionOptions, reply: tokio::sync::oneshot::Sender<maho_ext_api::QuestionResponse>) {
        use crate::components::ask_user_question_state as state;
        let request_id = request.request_id.clone();
        let timeout_ms = options.dialog.timeout_ms;
        let now_ms = self.clock.elapsed().as_millis() as u64;
        let get_deadline_at_ms = crate::question_registry::question_deadline(&options, now_ms);
        let draft = options.initial_draft.clone().map(crate::question_registry::question_draft).unwrap_or_default();
        let on_progress = options.on_progress.map(|callback| std::rc::Rc::new(std::cell::RefCell::new(Box::new(move |draft: &state::QuestionDraft| callback(maho_ext_api::QuestionDraft { comment: draft.comment.clone(), answers: Some(draft.answers.iter().map(|(id, answer)| (id.clone(), maho_ext_api::QuestionAnswer { selected: answer.selected.clone(), text: answer.text.clone() })).collect()) })) as Box<dyn FnMut(&state::QuestionDraft)>)));
        let entry = crate::question_registry::PendingQuestion {
            timeout_ms: timeout_ms.unwrap_or(request.timeout_ms),
            asked_at_ms: now_ms,
            get_deadline_at_ms,
            on_progress,
            request,
            draft,
            replies: Vec::new(),
        };
        let reattaching = self.questions.get(&request_id).is_some_and(|existing| existing.replies.iter().all(tokio::sync::oneshot::Sender::is_closed));
        if reattaching && self.questions.shown_id() == Some(request_id.as_str()) && self.questions.surface == crate::question_registry::QuestionSurface::Expanded { self.collapse_shown_question(); }
        if let Some(existing) = self.questions.get_mut(&request_id) {
            if reattaching { *existing = entry; }
        } else { self.questions.show(entry); }
        if let Some(existing) = self.questions.get_mut(&request_id) { existing.replies.push(reply); }
        self.refresh_async_question_widget();
    }

    /// senpi's `refreshAsyncWidget`: the collapsed widget shows the shown question, and nothing
    /// while the expanded component owns the surface.
    fn refresh_async_question_widget(&mut self) {
        if self.questions.surface == crate::question_registry::QuestionSurface::Expanded { self.async_question_widget = None; return; }
        let now_ms = self.clock.elapsed().as_millis() as u64;
        let pending = self.questions.shown().map(|shown| (shown.request.clone(), shown.draft.clone(), if shown.timeout_ms == 0 { 0 } else { shown.deadline_at_ms().saturating_sub(now_ms).max(1) }, shown.get_deadline_at_ms.clone()));
        let Some((request, draft, timeout_ms, deadline)) = pending else { self.async_question_widget = None; return; };
        let actions = self.question_actions.clone();
        let next_actions = actions.clone(); let expand_actions = actions.clone(); let own_answer_actions = actions.clone(); let expire_actions = actions.clone();
        let expire_id = request.request_id.clone();
        let pending_count = self.questions.len();
        self.async_question_widget = Some(crate::components::ask_user_async_widget::AskUserAsyncWidget::new(crate::components::ask_user_async_widget::AskUserAsyncWidgetOptions {
            request, draft, timeout_ms, now_ms,
            get_deadline_at_ms: deadline.map(|deadline| Box::new(move || deadline()) as Box<dyn Fn() -> u64>), pending_count, theme: self.theme.clone(), env: std::env::vars().collect(), mouse_capture_active: false,
            on_option_click: Some(Box::new(move |index| actions.borrow_mut().push_back(QuestionAction::ClickOption(index)))),
            on_own_answer_click: Some(Box::new(move || own_answer_actions.borrow_mut().push_back(QuestionAction::OwnAnswer))),
            on_expand_click: Some(Box::new(move || expand_actions.borrow_mut().push_back(QuestionAction::Expand))),
            on_next_question: Some(Box::new(move || next_actions.borrow_mut().push_back(QuestionAction::Next))),
            on_expire: Box::new(move || expire_actions.borrow_mut().push_back(QuestionAction::Expire(expire_id.clone()))),
        }));
    }

    fn cycle_question(&mut self) { if self.questions.cycle() { self.refresh_async_question_widget(); } }

    /// senpi's `expandPendingQuestion`: mount the full component for the shown request. Esc collapses
    /// back to the widget and keeps the question pending; any other response settles it.
    fn expand_shown_question(&mut self, initial_question_index: Option<usize>) {
        if self.questions.surface == crate::question_registry::QuestionSurface::Expanded { return; }
        let now_ms = self.clock.elapsed().as_millis() as u64;
        let pending = self.questions.shown().map(|shown| (shown.request.clone(), shown.draft.clone(), if shown.timeout_ms == 0 { 0 } else { shown.deadline_at_ms().saturating_sub(now_ms).max(1) }, shown.get_deadline_at_ms.clone()));
        let Some((request, draft, timeout_ms, deadline)) = pending else { return; };
        let result = self.question_result.clone();
        let progress = self.questions.shown().and_then(|shown| shown.on_progress.clone());
        let mut options = crate::components::ask_user_question::AskUserQuestionOptions::new(self.theme.clone());
        options.now_ms = now_ms;
        options.timeout_ms = Some(timeout_ms);
        options.get_deadline_at_ms = deadline.map(|deadline| Box::new(move || deadline()) as Box<dyn Fn() -> u64>);
        options.initial_draft = Some(draft);
        options.initial_question_index = initial_question_index;
        let actions = self.question_actions.clone();
        let request_id = request.request_id.clone();
        options.on_progress = Some(Box::new(move |draft| {
            actions.borrow_mut().push_back(QuestionAction::Progress(request_id.clone(), draft.clone()));
            if let Some(progress) = &progress { let mut callback = progress.borrow_mut(); (**callback)(draft); }
        }));
        self.questions.surface = crate::question_registry::QuestionSurface::Expanded;
        self.blocking_question = false;
        self.async_question_widget = None;
        self.question = Some(crate::components::ask_user_question::AskUserQuestionComponent::new(request, Box::new(move |response| { *result.borrow_mut() = Some(to_extension_response(response)); }), options));
    }

    /// senpi's `hideQuestionOverlay`.
    fn collapse_shown_question(&mut self) {
        self.handle_question_actions();
        self.question = None;
        self.question_result.borrow_mut().take();
        self.questions.surface = crate::question_registry::QuestionSurface::Collapsed;
        self.refresh_async_question_widget();
    }

    /// senpi's `AsyncQuestionState.finish`: resolve every completion and hand the surface on.
    fn finish_question(&mut self, request_id: &str, response: maho_ext_api::QuestionResponse) {
        let was_shown = self.questions.shown_id() == Some(request_id);
        if let Some(mut entry) = self.questions.finish(request_id) {
            for reply in entry.replies.drain(..) { let _ = reply.send(response.clone()); }
        }
        if was_shown { self.question = None; self.blocking_question = false; }
        self.refresh_async_question_widget();
    }

    fn handle_question_actions(&mut self) {
        loop {
            let action = self.question_actions.borrow_mut().pop_front();
            let Some(action) = action else { break; };
            match action {
                QuestionAction::Progress(request_id, draft) => { if let Some(entry) = self.questions.get_mut(&request_id) { entry.draft = draft; } }
                QuestionAction::ExpandRequest(request_id) => { self.ui_dialog = None; self.local_dialog_reply = None; self.questions.surface = crate::question_registry::QuestionSurface::Collapsed; if self.questions.set_shown(&request_id) { self.expand_shown_question(None); } }
                QuestionAction::CloseList => { self.questions.surface = crate::question_registry::QuestionSurface::Collapsed; }
                QuestionAction::Expand => self.expand_shown_question(None),
                QuestionAction::Next => self.cycle_question(),
                QuestionAction::ClickOption(index) => self.click_shown_question_option(index),
                QuestionAction::OwnAnswer => { self.expand_shown_question(None); if let Some(question) = &mut self.question { question.open_own_answer(None); } }
                QuestionAction::Expire(request_id) => { let response = self.timed_out_question_response(&request_id); self.finish_question(&request_id, response); }
            }
        }
    }

    fn shown_unanswered_index(&self) -> Option<usize> {
        let shown = self.questions.shown()?;
        let unanswered = shown.unanswered();
        shown.request.questions.iter().position(|question| unanswered.first() == Some(&question.id))
    }

    fn shown_question_has_option(&self, question_index: usize, option_index: usize) -> bool {
        self.questions.shown().and_then(|shown| shown.request.questions.get(question_index)).is_some_and(|question| question.options.get(option_index).is_some())
    }

    fn click_shown_question_option(&mut self, option: usize) {
        let index = self.shown_unanswered_index();
        self.expand_shown_question(index);
        if let Some(question) = &mut self.question { question.click_option(option, true); }
    }

    fn cancelled_question_response(&self, request_id: &str) -> maho_ext_api::QuestionResponse {
        self.questions.get(request_id).map_or_else(question_empty_response, |entry| to_extension_response(crate::components::ask_user_async_widget::build_cancelled_response(&entry.request, &entry.draft)))
    }

    fn timed_out_question_response(&self, request_id: &str) -> maho_ext_api::QuestionResponse {
        let elapsed = self.clock.elapsed().as_millis() as u64;
        self.questions.get(request_id).map_or_else(|| maho_ext_api::QuestionResponse { status: maho_ext_api::QuestionStatus::TimedOut, answers: Default::default(), comment: None, unanswered: Vec::new(), auto_resolved_after_ms: None }, |entry| to_extension_response(crate::components::ask_user_async_widget::build_timed_out_response(&entry.request, &entry.draft, elapsed.saturating_sub(entry.asked_at_ms))))
    }

    fn question_frame(&self, request_id: &str, response: &maho_ext_api::QuestionResponse) -> Option<String> {
        let entry = self.questions.get(request_id)?;
        let questions: Vec<maho_ext_api::Question> = entry.request.questions.iter().map(|question| maho_ext_api::Question { id: question.id.clone(), header: question.header.clone(), question: question.question.clone(), multi_select: question.multi_select, options: question.options.iter().map(|option| maho_ext_api::QuestionOption { label: option.label.clone(), description: option.description.clone() }).collect() }).collect();
        Some(maho_ext_ask_user::format::format_user_message(response, request_id, &questions))
    }

    /// senpi's `handleAnswerCommand`.
    fn answer_command(&mut self, argument: &str) {
        let Some(shown) = self.questions.shown() else { self.show_status("No question is pending.".into()); return; };
        let request_id = shown.request.request_id.clone();
        if argument == "skip" {
            let response = self.cancelled_question_response(&request_id);
            let frame = self.question_frame(&request_id, &response);
            self.finish_question(&request_id, response);
            self.show_status("The user dismissed the question.".into());
            if let Some(frame) = frame {
                let streaming = self.host_is_streaming();
                let session = self.session.clone();
                tokio::spawn(async move {
                    let options = PromptOptions { streaming_behavior: Some(if streaming { maho_ext_api::StreamingBehavior::Steer } else { maho_ext_api::StreamingBehavior::FollowUp }), ..Default::default() };
                    let _ = session.prompt(&frame, options).await;
                });
            }
            return;
        }
        if !argument.is_empty() {
            let id = argument.bytes().next().filter(|first| matches!(first, b'1'..=b'9')).filter(|_| argument.bytes().all(|byte| byte.is_ascii_digit())).and_then(|_| argument.parse::<usize>().ok()).and_then(|number| self.questions.by_number(number)).map(str::to_owned);
            match id {
                Some(id) => { if self.questions.surface == crate::question_registry::QuestionSurface::Expanded { self.collapse_shown_question(); } self.questions.set_shown(&id); self.expand_shown_question(None); }
                None => self.show_status("Choose a pending question number or /answer skip.".into()),
            }
            return;
        }
        if self.questions.len() == 1 { self.expand_shown_question(None); return; }
        self.show_answer_list();
    }

    /// senpi's `/answer` SelectList over `pendingOrder`.
    fn show_answer_list(&mut self) {
        if self.questions.surface == crate::question_registry::QuestionSurface::Expanded { self.collapse_shown_question(); }
        let now = self.clock.elapsed().as_millis() as u64;
        let labels: Vec<String> = self.questions.order().iter().enumerate().filter_map(|(index, id)| self.questions.get(id).map(|entry| format!("{}. {}", index + 1, crate::question_registry::answer_list_label(entry, now)))).collect();
        if labels.is_empty() { self.show_status("No question is pending.".into()); return; }
        self.questions.surface = crate::question_registry::QuestionSurface::List;
        let (reply, receiver) = tokio::sync::oneshot::channel(); self.local_dialog_reply = Some(receiver); *self.ui_reply.borrow_mut() = Some(reply);
        let selected = self.ui_reply.clone(); let cancelled = self.ui_reply.clone(); let actions = self.question_actions.clone(); let ids = self.questions.order().to_vec();
        let cancel_actions = actions.clone();
        let component = crate::components::extension_selector::ExtensionSelectorComponent::new(&self.theme, Arc::new(self.keybindings()), "Pending questions", labels,
            Box::new(move |choice| { if let Some(id) = choice.split('.').next().and_then(|prefix| prefix.trim().parse::<usize>().ok()).and_then(|number| number.checked_sub(1)).and_then(|index| ids.get(index)) { actions.borrow_mut().push_back(QuestionAction::ExpandRequest(id.clone())); } selected.borrow_mut().take(); }),
            Box::new(move || { cancelled.borrow_mut().take(); cancel_actions.borrow_mut().push_back(QuestionAction::CloseList); }), Default::default());
        self.ui_dialog = Some(Box::new(component));
    }

    pub fn footer_snapshot(&self) -> crate::components::footer::FooterSnapshot {
        let stats = self.host_session_stats();
        let model = self.host_model();
        let usage = self.host_context_usage();
        let latest_cache_hit_rate = match self.remote_state_snapshot() {
            Some(state) => state.usage_totals.as_ref().and_then(|totals| totals.get("latestCacheHitRate")).and_then(serde_json::Value::as_f64),
            None => self.session.with_session_manager(|manager| manager.usage_totals().latest_cache_hit_rate),
        };
        crate::components::footer::FooterSnapshot {
            cwd: self.host_cwd(), home: std::env::var("HOME").ok(), session_name: self.host_session_name(),
            cache_read: stats.tokens.cache_read as f64, cache_write: stats.tokens.cache_write as f64, cost: stats.cost, latest_cache_hit_rate,
            context_window: model.context_window as f64, context_percent: usage.and_then(|usage| usage.percent),
            context_tokens: usage.and_then(|usage| usage.tokens).map(|tokens| tokens as f64),
            model_id: Some(model.id), provider: Some(model.provider), reasoning: model.reasoning,
            thinking_level: Some(self.host_thinking_level().as_str().into()), ..Default::default()
        }
    }

    pub fn handle_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::AgentStart => { self.agent_idle = false; self.pending_tools.clear(); self.tool_partial_json.clear(); self.tool_args_reveal.stop(); self.working_started_ms = Some(self.clock.elapsed().as_secs_f64() * 1000.0); }
            AgentEvent::AgentEnd { .. } => { self.agent_idle = true; self.pending_tools.clear(); self.tool_partial_json.clear(); self.tool_args_reveal.stop(); self.tool_reveal.stop(); self.working_started_ms = None; }
            AgentEvent::MessageStart { message } => {
                if message.role() == "user" {
                    let value = serde_json::to_value(message).expect("serializable agent message");
                    let text = value["content"].as_str().map(str::to_owned).unwrap_or_else(|| value["content"].as_array().map(|parts| parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default());
                    if let Some(block) = maho_core::skill_invocation::parse_skill_block(&text) {
                        let mut component = crate::components::skill_invocation_message::SkillInvocationMessageComponent::new(block.skills.into_iter().map(|skill| crate::components::skill_invocation_message::InvokedSkill { name:skill.name, content:skill.content }).collect(), self.theme.clone(), get_markdown_theme(&self.theme), crate::components::keybinding_hints::key_display_text("app.tools.expand"));
                        component.set_expanded(self.tools_expanded);
                        let component = Rc::new(RefCell::new(component)); self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(component.clone()));
                        self.history_expansion.push(Box::new(move |expanded| component.borrow_mut().set_expanded(expanded)));
                        if let Some(text) = block.user_message { self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(maho_tui::components::spacer::Spacer::new(1))))); self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(UserMessageComponent::new(text, self.theme.clone(), get_markdown_theme(&self.theme), self.output_pad(), self.markdown_transformers.clone()))))); }
                        return;
                    }
                    let component = Rc::new(RefCell::new(UserMessageComponent::new(text.clone(), self.theme.clone(), get_markdown_theme(&self.theme), self.output_pad(), self.markdown_transformers.clone())));
                    if let Some(frame) = crate::components::ask_user_answer_chip::parse_ask_user_answer_frame(&text) {
                        let entries = self.session.with_session_manager(|manager| manager.entries());
                        let headers = crate::components::ask_user_answer_chip::get_ask_user_answer_headers(&entries, &frame.request_id);
                        self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(Rc::new(RefCell::new(crate::components::ask_user_answer_chip::AskUserAnswerChip::new(&frame, &headers, component, self.theme.clone())))));
                    } else { self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Other(component)); }
                } else if message.role() == "assistant" {
                    self.assistant_segments.clear();
                    self.reveal.begin(serde_json::to_value(message).expect("assistant"), self.clock.elapsed().as_secs_f64() * 1000.0);
                    let component = Rc::new(RefCell::new(AssistantMessageComponent::new(None, false, get_markdown_theme(&self.theme), "Thinking…", self.output_pad(), self.markdown_transformers.clone(), self.theme.clone())));
                    self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Assistant(component.clone()));
                    self.assistant_cards.push(component.clone());
                    component.borrow_mut().set_hidden_thinking_label(&self.hidden_thinking_label);
                    self.streaming = Some(component);
                } else if message.role() == "custom" { self.add_history_message(message); }
            }
            AgentEvent::MessageUpdate { message, .. } | AgentEvent::MessageEnd { message } if message.role() == "assistant" => {
                if let Some(assistant) = message.as_assistant() {
                    let final_message = matches!(event, AgentEvent::MessageEnd { .. });
                    if let AgentEvent::MessageUpdate { assistant_message_event: maho_ai::types::AssistantMessageEvent::ToolcallDelta { content_index, delta, .. }, .. } = event
                        && let Some(maho_ai::types::ContentBlock::ToolCall(call)) = assistant.content.get(*content_index) {
                        self.tool_partial_json.entry(call.id.clone()).or_default().push_str(delta);
                    }
                    let mut start = 0;
                    for (index, content) in assistant.content.iter().enumerate() {
                        if let maho_ai::types::ContentBlock::ToolCall(call) = content {
                            self.update_assistant_segment(assistant, start, index, final_message);
                            let args = serde_json::Value::Object(call.arguments.clone());
                            let component = self.tool_component(&call.name, &call.id, args.clone());
                            if final_message { self.tool_args_reveal.finish(&call.id); self.tool_partial_json.remove(&call.id); component.borrow_mut().update_args(args); }
                            else if let Some(partial) = self.tool_partial_json.get(&call.id) {
                                let (handled, value) = self.tool_args_reveal.update(&call.id, Rc::as_ptr(&component) as usize, partial, self.clock.elapsed().as_secs_f64() * 1000.0);
                                if let Some(value) = value { component.borrow_mut().update_args(value); }
                                else if !handled { component.borrow_mut().update_args(args); }
                            } else { component.borrow_mut().update_args(args); }
                            if matches!(event, AgentEvent::MessageEnd { .. }) { component.borrow_mut().set_args_complete(); }
                            start = index + 1;
                        }
                    }
                    self.update_assistant_segment(assistant, start, assistant.content.len(), final_message);
                    if matches!(event, AgentEvent::MessageEnd { .. }) && matches!(assistant.stop_reason, maho_ai::types::StopReason::Aborted | maho_ai::types::StopReason::Error) {
                        for component in self.pending_tools.values() {
                            component.borrow_mut().update_result(ToolExecutionResult { content: vec![maho_tools::definition::ToolContent::text(assistant.error_message.as_deref().unwrap_or("Error"))], details: None, is_error: true }, false);
                        }
                        self.pending_tools.clear();
                    }
                }
                if matches!(event, AgentEvent::MessageEnd { .. }) {
                    self.reveal.stop(); self.streaming = None; self.assistant_segments.clear();
                    if let Some(assistant) = message.as_assistant() { self.add_continuity_notice(assistant); }
                }
            }
            AgentEvent::ToolExecutionStart { tool_call_id, tool_name, args } => {
                self.tool_args_reveal.finish(tool_call_id); self.tool_partial_json.remove(tool_call_id);
                let component = self.tool_component(tool_name, tool_call_id, args.clone());
                let mut component = component.borrow_mut();
                component.update_args(args.clone());
                component.mark_execution_started();
            }
            AgentEvent::ToolExecutionUpdate { tool_call_id, partial_result, .. } => {
                if let Some(component) = self.pending_tools.get(tool_call_id) {
                    let (handled, value) = self.tool_reveal.update(tool_call_id, Rc::as_ptr(component) as usize, partial_result.clone(), self.clock.elapsed().as_secs_f64() * 1000.0);
                    if let Some(value) = value { component.borrow_mut().update_result(Self::tool_result(&value, false), true); }
                    else if !handled { component.borrow_mut().update_result(Self::tool_result(partial_result, false), true); }
                }
            }
            AgentEvent::ToolExecutionEnd { tool_call_id, tool_name, result, is_error } => {
                self.tool_reveal.finish(tool_call_id, self.clock.elapsed().as_secs_f64() * 1000.0);
                let component = self.tool_component(tool_name, tool_call_id, serde_json::json!({}));
                component.borrow_mut().update_result(Self::tool_result(result, *is_error), false);
                self.pending_tools.remove(tool_call_id);
            }
            _ => {}
        }
    }

    fn update_assistant_segment(&mut self, message: &maho_ai::types::AssistantMessage, start: usize, end: usize, final_message: bool) {
        if start == end && start != 0 { return; }
        let output_pad = self.output_pad();
        let component = if start == 0 { self.streaming.clone() } else {
            Some(self.assistant_segments.entry(start).or_insert_with(|| {
                let component = Rc::new(RefCell::new(AssistantMessageComponent::new(None, false, get_markdown_theme(&self.theme), "Thinking…", output_pad, self.markdown_transformers.clone(), self.theme.clone())));
                component.borrow_mut().set_hidden_thinking_label(&self.hidden_thinking_label);
                self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Assistant(component.clone())); self.assistant_cards.push(component.clone()); component
            }).clone())
        };
        if let Some(component) = component {
            let mut segment = message.clone(); segment.content = message.content[start..end].to_vec();
            if end < message.content.len() { segment.error_message = None; segment.stop_reason = maho_ai::types::StopReason::ToolUse; }
            let value = serde_json::to_value(segment).expect("assistant segment");
            let value = if start == 0 && !final_message { self.reveal.set_target(value, self.clock.elapsed().as_secs_f64() * 1000.0) } else { value };
            component.borrow_mut().update_content(&value, Some(!final_message));
            component.borrow_mut().set_hide_thinking_block(self.reveal.hide_thinking);
            component.borrow_mut().set_expanded(self.tools_expanded);
        }
    }

    pub fn tick(&mut self, now_ms: f64) {
        self.footer_data.set_cwd(&self.host_cwd());
        self.footer_data.set_available_provider_count(self.host_available_models().iter().map(|model|&model.provider).collect::<std::collections::BTreeSet<_>>().len());
        self.footer_data.refresh_branch();
        for (id, value) in self.tool_args_reveal.tick(now_ms) { if let Some(component) = self.pending_tools.get(&id) { component.borrow_mut().update_args(value); } }
        if let Some(share) = &mut self.share { share.loader.tick(now_ms.max(0.0) as u64); }
        if let Some(widget) = &mut self.async_question_widget { widget.tick(now_ms.max(0.0) as u64); }
        else if let Some(question) = &mut self.question { question.tick(now_ms.max(0.0) as u64); }
        self.settle_questions();
        for component in &self.tool_cards { let mut component = component.borrow_mut(); component.set_now_ms(now_ms); component.tick(now_ms.max(0.0) as u64); }
        if let Some(value) = self.reveal.tick(now_ms) && let Some(component) = &self.streaming { component.borrow_mut().update_content(&value, Some(true)); }
        for (id, value) in self.tool_reveal.tick(now_ms) { if let Some(component) = self.pending_tools.get(&id) { component.borrow_mut().update_result(Self::tool_result(&value, false), true); } }
        // senpi `ArminComponent.startAnimation`: 30 fps (60 for glitch) until the effect completes.
        self.armin_animations.retain_mut(|(component, last_ms)| {
            let mut component = component.borrow_mut();
            let interval = if component.effect() == crate::components::armin::Effect::Glitch { 1000.0 / 60.0 } else { 1000.0 / 30.0 };
            while now_ms - *last_ms >= interval {
                *last_ms += interval;
                if component.tick() { return false; }
            }
            true
        });
        self.chat.warm_next_chunk();
    }

    pub fn working_frame(&self, now_ms: f64) -> Option<String> {
        if !self.working_visible { return None; }
        let elapsed = (now_ms - self.working_started_ms?).max(0.0);
        if let Some(options) = &self.working_indicator && let Some(frames) = &options.frames && !frames.is_empty() {
            let index = (elapsed / options.interval_ms.unwrap_or(80).max(1) as f64) as usize % frames.len();
            return Some(frames[index].clone());
        }
        let base = |text: &str| self.theme.fg(crate::theme::ThemeColor::Dim, text);
        let glow = |text: &str| self.theme.fg(crate::theme::ThemeColor::Text, text);
        let highlight = |text: &str| self.theme.bold(&glow(text));
        Some(crate::working_status::format_working_status_message_frame(self.working_message.as_deref().unwrap_or("Working"), elapsed / 1000.0,
            &crate::components::keybinding_hints::key_display_text("app.interrupt"), elapsed,
            &crate::working_status::WorkingStatusTextFrameStyle { base:&base, glow:&glow, highlight:&highlight, shimmer:None }, &base))
    }

    fn tool_component(&mut self, name: &str, id: &str, args: serde_json::Value) -> Rc<RefCell<ToolExecutionComponent>> {
        let name = self.session.resolve_tool_call_name(name);
        let renderers = self.tool_renderers(&name);
        let options = self.session.with_settings_manager(|settings| ToolExecutionOptions { show_images:settings.get_bool("showImages"), image_width_cells:settings.get_number("imageWidthCells").map(|width| width as u32) });
        self.pending_tools.get(id).cloned().unwrap_or_else(|| {
            let component = Rc::new(RefCell::new(ToolExecutionComponent::new(&name, id, args, options, renderers, &self.host_cwd(), ToolExecutionPresentation::Classic, None, self.theme.clone())));
            self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Tool(component.clone()));
            component.borrow_mut().set_expanded(self.tools_expanded);
            self.tool_cards.push(component.clone());
            self.pending_tools.insert(id.into(), component.clone());
            component
        })
    }

    /// senpi's `getRegisteredToolDefinition`: the session's registered definition for the resolved
    /// tool name, else the ask-user pair, else the built-in renderers.
    fn tool_renderers(&self, tool_name: &str) -> Option<Rc<RefCell<dyn crate::tools::renderers::ToolRenderers>>> {
        let tool_name = self.session.resolve_tool_call_name(tool_name);
        self.native_tool_renderer_snapshots.get(&tool_name).cloned()
            .or_else(|| self.tool_renderer_snapshots.get(&tool_name).cloned())
            .or_else(|| crate::tools::renderers::card_renderers(&tool_name))
    }

    /// Refresh the per-tool registered-renderer snapshots from the session's live runner. Run at async
    /// ingress (extension binding, submission, replacement) because the synchronous card path cannot
    /// await the session accessor; rebuilding the map drops a removed registration.
    /// Refresh the per-tool registered-renderer snapshots from the session's live runner. Uses the
    /// session's full registered-tool snapshot (`tool_renderers_snapshot`, over `get_all_registered_tools`
    /// with the runner's live registration/replacement precedence), not the active set, so a tool that
    /// is registered but currently inactive still resolves its registered renderers on replay, and a
    /// removed or replaced registration is dropped by rebuilding the map. Run at async ingress
    /// (extension binding, submission, reload, host replacement) because the synchronous card path
    /// cannot await the accessor.
    pub async fn refresh_tool_renderer_snapshots(&mut self) {
        self.tool_renderer_snapshots = self
            .session
            .tool_renderers_snapshot()
            .await
            .into_iter()
            .map(|(name, renderers)| {
                (name, Rc::new(RefCell::new(crate::tools::renderers::registered::RegisteredToolRenderers::new(renderers))) as Rc<RefCell<dyn crate::tools::renderers::ToolRenderers>>)
            })
            .collect();
    }

    fn tool_result(value: &serde_json::Value, is_error: bool) -> ToolExecutionResult {
        ToolExecutionResult { content: serde_json::from_value(value["content"].clone()).expect("typed tool result content"), details: value.get("details").cloned(), is_error }
    }
}

impl crate::replay_assistant_tools::ReplayToolHost for InteractiveMode {
    fn expanded(&self) -> bool { self.tools_expanded }
    fn add_message(&mut self, message: maho_ai::types::AssistantMessage) {
        let component = Rc::new(RefCell::new(AssistantMessageComponent::new(Some(serde_json::to_value(message).expect("assistant")), self.reveal.hide_thinking, get_markdown_theme(&self.theme), "Thinking…", self.output_pad(), self.markdown_transformers.clone(), self.theme.clone())));
        component.borrow_mut().set_expanded(self.tools_expanded);
        component.borrow_mut().set_hidden_thinking_label(&self.hidden_thinking_label);
        self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Assistant(component.clone())); self.assistant_cards.push(component);
    }
    fn add_child(&mut self, component: Rc<RefCell<ToolExecutionComponent>>) { self.chat.add_child(crate::components::exploration_transcript_container::TranscriptChild::Tool(component.clone())); self.tool_cards.push(component); }
    fn create_tool(&mut self, name: &str, id: &str, args: &serde_json::Map<String, serde_json::Value>) -> ToolExecutionComponent {
        let name = self.session.resolve_tool_call_name(name);
        let renderers = self.tool_renderers(&name);
        let options = self.session.with_settings_manager(|settings| ToolExecutionOptions { show_images:settings.get_bool("showImages"), image_width_cells:settings.get_number("imageWidthCells").map(|width| width as u32) });
        ToolExecutionComponent::new(&name, id, serde_json::Value::Object(args.clone()), options, renderers, &self.host_cwd(), ToolExecutionPresentation::Classic, None, self.theme.clone())
    }
    fn add_pending(&mut self, id: &str, component: Rc<RefCell<ToolExecutionComponent>>) { self.pending_tools.insert(id.into(), component); }
}

/// senpi `getTipsHistory`: the global `tipsHistory` object as a `tipId -> timestamp` map.
fn tips_history(settings: &maho_core::settings_manager::SettingsManager) -> std::collections::HashMap<String, u64> {
    settings.get_value("tipsHistory").and_then(serde_json::Value::as_object).map(|map| map.iter().filter_map(|(key, value)| value.as_u64().map(|value| (key.clone(), value))).collect()).unwrap_or_default()
}

/// Parse the wire `RpcSessionModelEntry[]` (lane 36's `session_model_entry_json`) into typed
/// session model entries. Used only when a shared host is mounted.
fn session_model_entries(value: &serde_json::Value) -> Vec<maho_core::agent_session::SessionModelEntry> {
    value
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    Some(maho_core::agent_session::SessionModelEntry {
                        model: serde_json::from_value(entry.get("model")?.clone()).ok()?,
                        thinking_level: entry.get("thinkingLevel").and_then(|level| serde_json::from_value(level.clone()).ok()),
                        thinking_selection: entry.get("thinkingSelection").and_then(|selection| serde_json::from_value(selection.clone()).ok()),
                        service_tier: entry.get("serviceTier").and_then(serde_json::Value::as_str).and_then(service_tier_from_wire),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn service_tier_from_wire(value: &str) -> Option<maho_ext_api::ServiceTier> {
    Some(match value {
        "auto" => maho_ext_api::ServiceTier::Auto,
        "flex" => maho_ext_api::ServiceTier::Flex,
        "priority" => maho_ext_api::ServiceTier::Priority,
        _ => return None,
    })
}

/// Parse the wire `ContextUsage` (`{tokens, contextWindow, percent}`) into the typed record.
fn context_usage_from_wire(value: &serde_json::Value) -> Option<maho_core::agent_session::ContextUsage> {
    Some(maho_core::agent_session::ContextUsage {
        tokens: value.get("tokens").and_then(serde_json::Value::as_u64),
        context_window: value.get("contextWindow").and_then(serde_json::Value::as_u64)?,
        percent: value.get("percent").and_then(serde_json::Value::as_f64),
    })
}

fn ordered_inputs_from_remote_state(state: Option<&crate::interactive_host_runtime::RemoteSessionState>) -> Vec<maho_core::agent_session::QueuedInput> {
    state
        .map(|state| {
            state.ordered.iter().filter_map(|entry| {
                let mode = match entry.get("mode")?.as_str()? { "steer" => maho_ext_api::StreamingBehavior::Steer, "followUp" => maho_ext_api::StreamingBehavior::FollowUp, _ => return None };
                Some(maho_core::agent_session::QueuedInput { text: entry.get("text")?.as_str()?.to_owned(), mode, enqueue_order: entry.get("enqueueOrder")?.as_u64()? })
            }).collect()
        })
        .unwrap_or_default()
}

/// Parse the wire `get_session_stats` record (lane 36's `get_session_stats`) into the typed stats.
fn session_stats_from_wire(value: &serde_json::Value) -> maho_core::agent_session::SessionStats {
    let count = |key: &str| value.get(key).and_then(serde_json::Value::as_u64).unwrap_or(0) as usize;
    let tokens = |key: &str| value.get("tokens").and_then(|tokens| tokens.get(key)).and_then(serde_json::Value::as_u64).unwrap_or(0);
    maho_core::agent_session::SessionStats {
        session_file: value.get("sessionFile").and_then(serde_json::Value::as_str).map(str::to_owned),
        session_id: value.get("sessionId").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned(),
        user_messages: count("userMessages"),
        assistant_messages: count("assistantMessages"),
        tool_calls: count("toolCalls"),
        tool_results: count("toolResults"),
        total_messages: count("totalMessages"),
        tokens: maho_core::agent_session::SessionStatsTokens { input: tokens("input"), output: tokens("output"), cache_read: tokens("cacheRead"), cache_write: tokens("cacheWrite"), total: tokens("total") },
        cost: value.get("cost").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
        context_usage: value.get("contextUsage").and_then(context_usage_from_wire),
    }
}

pub fn get_path_command_argument(text: &str, command: &str) -> Option<String> {
    let args = text.strip_prefix(command)?.strip_prefix(' ')?.trim_start();
    let first = args.chars().next()?;
    let path = if matches!(first, '\'' | '"') { let end = args[1..].find(first)? + 1; &args[1..end] }
        else { args.split_whitespace().next()? };
    if path == "~" { std::env::var("HOME").ok() }
    else if let Some(relative) = path.strip_prefix("~/") { std::env::var("HOME").ok().map(|home| format!("{home}/{relative}")) }
    else { Some(path.into()) }
}

/// A single `1`-`9` keystroke, which senpi's question chords treat as an option choice.
fn single_digit(data: &str) -> Option<u32> {
    let mut characters = data.chars();
    let digit = characters.next()?.to_digit(10)?;
    (characters.next().is_none() && (1..=9).contains(&digit)).then_some(digit)
}

fn question_empty_response() -> maho_ext_api::QuestionResponse {
    maho_ext_api::QuestionResponse { status: maho_ext_api::QuestionStatus::Cancelled, answers: Default::default(), comment: None, unanswered: Vec::new(), auto_resolved_after_ms: None }
}

fn to_extension_response(response: crate::components::ask_user_question_state::QuestionResponse) -> maho_ext_api::QuestionResponse {
    use crate::components::ask_user_question_state as state;
    let status = match response.status { state::QuestionStatus::Answered => maho_ext_api::QuestionStatus::Answered, state::QuestionStatus::CommentSubmitted => maho_ext_api::QuestionStatus::CommentSubmitted, state::QuestionStatus::Cancelled => maho_ext_api::QuestionStatus::Cancelled, state::QuestionStatus::TimedOut => maho_ext_api::QuestionStatus::TimedOut };
    maho_ext_api::QuestionResponse { status, comment: response.comment, unanswered: response.unanswered, auto_resolved_after_ms: response.auto_resolved_after_ms,
        answers: response.answers.into_iter().map(|(id, value)| (id, maho_ext_api::QuestionAnswer { selected: value.selected, text: value.text })).collect() }
}

impl Component for InteractiveMode {
    fn render(&mut self, width: usize) -> Vec<String> {
        let (mut document, dock) = self.render_split(width);
        document.extend(dock);
        document
    }
    fn handle_input(&mut self, data: &str) {
        let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system clock").as_millis();
        self.handle_input_at(data, u64::try_from(now_ms).expect("timestamp"));
    }
    fn has_input_handler(&self) -> bool { true }
    fn focusable_get(&self) -> Option<bool> { Some(maho_tui::tui::Focusable::focused(&self.editor)) }
    fn focusable_set(&mut self, focused: bool) { maho_tui::tui::Focusable::set_focused(&mut self.editor, focused); }
    fn invalidate(&mut self) { self.chat.invalidate(); self.editor.invalidate(); }
    fn dispose(&mut self) {
        for (_, widget, _) in &mut self.widgets { widget.dispose(); }
        self.widgets.clear();
        self.chat.dispose();
        self.native_tool_renderer_snapshots.clear();
        self.tool_renderer_snapshots.clear();
        self.session_host_subscription = None;
    }
}

impl InteractiveMode {
    /// The fullscreen render root's two halves, as separate line vectors: the transcript document
    /// and the fixed input dock. senpi's fullscreen chat viewport (`chat-viewport.ts`) places the
    /// document in a follow-end scroll view and pins the dock; the flat `render()` concatenation is
    /// only correct for the main-screen path, where the whole list scrolls together.
    pub fn render_split(&mut self, width: usize) -> (Vec<String>, Vec<String>) {
        self.drain_events();
        self.tick(self.clock.elapsed().as_secs_f64() * 1000.0);
        (self.render_document(width), self.render_dock(width))
    }
    pub(crate) fn render_document(&mut self,width:usize)->Vec<String> {
        let mut lines = self.header_container.render(width);
        lines.extend(self.loaded_resources_container.render(width));
        lines.extend(self.chat.render(width));
        lines
    }
    pub(crate) fn render_dock(&mut self,width:usize)->Vec<String> {
        let mut lines=Vec::new();
        let now = self.clock.elapsed().as_secs_f64() * 1000.0;
        // senpi `activeStatusIndicator`: a retry/branch-summary indicator owns the editor border;
        // compaction keeps a standalone status row; otherwise the working frame is embedded.
        let active = self.active_status_indicator.clone();
        if let Some(active) = &active { active.borrow_mut().tick(now.max(0.0) as u64); }
        let compaction = active.clone().filter(|indicator| indicator.borrow().kind == crate::components::status_indicator::StatusIndicatorKind::Compaction);
        let embedded = active.filter(|indicator| indicator.borrow().kind != crate::components::status_indicator::StatusIndicatorKind::Compaction);
        let indicator = embedded.or_else(|| self.working_frame(now).map(|frame| {
            let mut indicator = crate::components::status_indicator::StatusIndicator::working(&frame, self.theme.clone(), self.working_started_ms.unwrap_or(now).max(0.0) as u64);
            indicator.tick(now.max(0.0) as u64);
            Rc::new(RefCell::new(indicator))
        }));
        self.editor.set_working_status_indicator(indicator);
        lines.push(String::new());
        for component in &self.pending_bash_components { lines.extend(component.borrow_mut().render(width)); }
        if let Some(compaction) = compaction { lines.extend(compaction.borrow_mut().render(width)); }
        else if let Some(idle) = &mut self.idle_status { lines.extend(idle.render(width)); }
        for (_, widget, placement) in &mut self.widgets { if *placement == maho_ext_api::WidgetPlacement::AboveEditor { lines.extend(widget.render(width)); } }
        if self.shortcut_overlay { lines.extend(crate::components::shortcut_overlay::ShortcutOverlay::new(&self.theme).render(width)); }
        if let Some(widget) = &mut self.async_question_widget { lines.extend(widget.render(width)); }
        lines.extend(if self.async_question_widget.is_none() && let Some(question) = &mut self.question { question.render(width) } else if let Some(share) = &mut self.share { share.loader.render(width) } else if let Some(dialog) = &mut self.ui_dialog { dialog.render(width) } else if let Some(dialog) = &mut self.login_dialog { dialog.render(width) } else if let Some(input) = &mut self.rename_input { input.render(width) } else if let Some(editor) = &mut self.custom_editor { editor.render(width) } else { self.render_prompt_editor(width) });
        for (_, widget, placement) in &mut self.widgets { if *placement == maho_ext_api::WidgetPlacement::BelowEditor { lines.extend(widget.render(width)); } }
        let mut footer = crate::components::footer::FooterComponent::new(self.footer_snapshot());
        footer.set_auto_compact_enabled(self.host_auto_compaction_enabled());
        footer.snapshot.extension_statuses = self.extension_ui.statuses.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        lines.extend(if let Some(custom) = &mut self.footer { custom.render(width) } else { footer.render(width, &self.theme).expect("footer layout") });
        lines
    }

    /// senpi's `updateEditorBorderColor`: the chrome border, the bash-mode border while the draft
    /// starts with `!`, otherwise the current thinking level's border color.
    fn update_editor_border_color(&mut self) {
        let is_bash_mode = self.editor.editor.get_text().trim_start().starts_with('!');
        let level = self.host_thinking_level();
        if self.editor_border_key == Some((level, is_bash_mode)) { return; }
        self.editor_border_key = Some((level, is_bash_mode));
        let theme = self.theme.clone();
        self.editor.editor.border_color = if let Some(chrome) = &self.chrome {
            chrome.get_editor_border_color(&crate::grok::chrome::EditorBorderContext { is_bash_mode, thinking_level: level.as_str().to_owned() })
        } else if is_bash_mode {
            Rc::new(move |text: &str| theme.get_bash_mode_border_color(text))
        } else {
            Rc::new(move |text: &str| theme.get_thinking_border_color(level, text))
        };
    }

    fn render_prompt_editor(&mut self, width: usize) -> Vec<String> {
        self.update_editor_border_color();
        let mut lines = self.editor.render(width);
        if crate::components::neon_border::has_ulw_keyword(&self.editor.editor.get_text()) {
            crate::components::neon_border::apply_neon_border(&mut lines, self.clock.elapsed().as_secs_f64() * 1000.0, self.theme.get_color_mode());
        }
        lines
    }
}

impl InteractiveMode {
    fn handle_editor_input(&mut self, data: &str) {
        let submit = self.keybindings().matches(data, "tui.input.submit");
        if let Some(share) = &mut self.share {
            share.loader.handle_input(data);
            if share.loader.aborted() {
                self.restore_editor_after_share();
                self.show_status("Share cancelled".into());
            }
            return;
        }
        if let Some(dialog) = &mut self.ui_dialog {
            dialog.handle_input(data);
            if self.ui_reply.borrow().is_none() && self.custom_ui_reply.is_none() { self.ui_dialog = None; self.local_dialog_reply = None; }
            self.drain_auth_ui_events();
            return;
        }
        if let Some(dialog) = &mut self.login_dialog {
            dialog.handle_input(data);
            // senpi aborts the dialog signal on cancel; the login task then reports the neutral outcome.
            if dialog.is_cancelled() && let Some(abort) = self.auth_abort.take() {
                abort.abort(Some(maho_ai::utils::abort::AbortReason::new("AbortError", crate::login_outcome::LOGIN_CANCELLED_NOTICE)));
            }
            self.drain_auth_ui_events();
            return;
        }
        if let Some(input) = &mut self.rename_input {
            input.handle_input(data);
            let result = self.rename_result.borrow_mut().take();
            if let Some(result) = result {
                self.rename_input = None;
                if let Some(name) = result {
                    let name = name.trim();
                    if name.is_empty() { self.show_warning("Session name cannot be empty"); }
                    else { self.fire_set_session_name(name); self.show_status(format!("Session name set: {}", self.host_session_name().unwrap_or_else(|| name.into()))); }
                }
            }
        } else if let Some(editor) = &mut self.custom_editor {
            if submit {
                let text = editor.get_expanded_text().unwrap_or_else(|| editor.get_text());
                if !text.trim().is_empty() { self.submissions.borrow_mut().push_back(text); editor.set_text(""); }
                else { self.question_actions.borrow_mut().push_back(QuestionAction::Expand); }
            } else { editor.handle_input(data); }
        } else { self.editor.handle_input(data); }
        let text = self.custom_editor.as_ref().map_or_else(|| self.editor.editor.get_text(), |editor| editor.get_text());
        *self.extension_ui.editor_text.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = text;
    }
}

/// senpi `showScopedModelsSelector`'s refresh outcome: the shared refresh result, whether the 15s
/// timeout fired, and the available snapshot read after the refresh.
type ScopedModelsRefreshOutcome = (Result<maho_ai::models::ModelsRefreshResult, maho_ai::utils::abort::AbortReason>, bool, Vec<maho_ai::model::Model>);

/// The scoped-models selector plus its in-flight catalog refresh. The refresh result is applied on
/// the next frame; closing the dialog drops the receiver and aborts the refresh (senpi `dispose`).
struct ScopedModelsRefreshDialog {
    selector: crate::components::scoped_models_selector::ScopedModelsSelectorComponent,
    refresh: Option<tokio::sync::oneshot::Receiver<ScopedModelsRefreshOutcome>>,
    controller: maho_ai::utils::abort::AbortController,
    on_refreshed: Box<dyn FnMut(&mut crate::components::scoped_models_selector::ScopedModelsSelectorComponent, ScopedModelsRefreshOutcome)>,
}

impl ScopedModelsRefreshDialog {
    fn apply_refresh(&mut self) {
        let outcome = self.refresh.as_mut().and_then(|receiver| receiver.try_recv().ok());
        if let Some(outcome) = outcome { self.refresh = None; (self.on_refreshed)(&mut self.selector, outcome); }
    }
}

impl Component for ScopedModelsRefreshDialog {
    fn render(&mut self, width: usize) -> Vec<String> { self.apply_refresh(); self.selector.render(width) }
    fn handle_input(&mut self, data: &str) { self.apply_refresh(); self.selector.handle_input(data); }
    fn has_input_handler(&self) -> bool { true }
    fn invalidate(&mut self) { self.selector.invalidate(); }
    fn dispose(&mut self) { self.refresh = None; self.controller.abort(None); self.selector.dispose(); }
    fn focusable_get(&self) -> Option<bool> { self.selector.focusable_get() }
    fn focusable_set(&mut self, focused: bool) { self.selector.focusable_set(focused); }
}

impl Drop for ScopedModelsRefreshDialog {
    fn drop(&mut self) { self.controller.abort(None); }
}

/// senpi `handleDebugCommand`'s report body: the terminal size, every rendered
/// line with its visible width, then the session messages as JSONL.
pub(crate) fn format_debug_log(timestamp: &str, width: usize, height: usize, lines: &[String], messages: &[String]) -> String {
    let mut data = String::new();
    data.push_str(&format!("Debug output at {timestamp}\nTerminal: {width}x{height}\nTotal lines: {}\n\n", lines.len()));
    data.push_str("=== All rendered lines with visible widths ===\n");
    for (index, line) in lines.iter().enumerate() {
        data.push_str(&format!("[{index}] (w={}) {}\n", maho_tui::utils::visible_width(line), serde_json::to_string(line).unwrap_or_default()));
    }
    data.push_str("\n=== Agent messages (JSONL) ===\n");
    for message in messages { data.push_str(message); data.push('\n'); }
    data.push('\n');
    data
}

fn quote_if_needed(value: &str) -> String {
    if !value.is_empty() && value.chars().all(|character| character.is_ascii_alphanumeric() || "_-./~:@".contains(character)) { return value.to_owned(); }
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// senpi `formatResumeCommand`: the command line that reopens this session, when it is on disk.
/// `is_tty` is senpi's `process.stdout.isTTY`.
pub fn format_resume_command(manager: &maho_core::session_manager::SessionManager, is_tty: bool) -> Option<String> {
    if !is_tty || !manager.is_persisted() { return None; }
    let session_file = manager.session_file()?;
    if !std::path::Path::new(session_file).exists() { return None; }
    let mut args = vec![maho_core::config::app_command()];
    if !manager.uses_default_session_dir() { args.push("--session-dir".into()); args.push(quote_if_needed(manager.session_dir())); }
    args.push("--session".into());
    args.push(manager.session_id().to_owned());
    Some(args.join(" "))
}

/// senpi `checkTmuxKeyboardSetup`: query the tmux options (2 s each) and build the warning.
pub async fn check_tmux_keyboard_setup() -> Option<String> {
    std::env::var_os("TMUX")?;
    async fn run_tmux(args: &[&str]) -> Option<String> {
        let output = tokio::process::Command::new("tmux").args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true).output();
        let output = tokio::time::timeout(std::time::Duration::from_secs(2), output).await.ok()?.ok()?;
        output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
    let (extended_keys, extended_keys_format, client_termname, allow_passthrough, focus_events, version) = tokio::join!(
        run_tmux(&["show", "-gv", "extended-keys"]),
        run_tmux(&["show", "-gv", "extended-keys-format"]),
        run_tmux(&["display-message", "-p", "#{client_termname}"]),
        run_tmux(&["show", "-gv", "allow-passthrough"]),
        run_tmux(&["show", "-gv", "focus-events"]),
        run_tmux(&["display-message", "-p", "#{version}"]),
    );
    // Could not query tmux (timeout, sandbox, ...): do not warn.
    let extended_keys = extended_keys?;
    crate::tmux_setup::build_tmux_setup_warning(&crate::tmux_setup::TmuxSetupCheck {
        extended_keys: Some(extended_keys.as_str()),
        extended_keys_format: extended_keys_format.as_deref(),
        images_enabled: maho_tui::terminal_image::get_capabilities().tmux_passthrough,
        outer_kitty_capable: maho_tui::terminal_capabilities::outer_kitty_graphics_mode(client_termname.as_deref().unwrap_or("")).is_some(),
        allow_passthrough: allow_passthrough.as_deref(),
        focus_events: focus_events.as_deref(),
        version: version.as_deref(),
    })
}

/// senpi `getMissingSessionCwdIssue` for a session file not yet opened: the header's recorded cwd
/// when it is set and no longer exists, read without opening (and possibly migrating) the file.
pub fn missing_session_cwd_issue(path: &str, fallback_cwd: &str) -> Option<maho_core::session_cwd::SessionCwdIssue> {
    if !std::path::Path::new(path).exists() { return None; }
    let entries = maho_core::session_manager::load_entries_from_file(path);
    let session_cwd = entries.first().and_then(|header| header.get("cwd")).and_then(serde_json::Value::as_str)?.to_owned();
    if session_cwd.is_empty() || std::path::Path::new(&session_cwd).exists() { return None; }
    Some(maho_core::session_cwd::SessionCwdIssue { session_file: Some(path.to_owned()), session_cwd, fallback_cwd: fallback_cwd.to_owned() })
}

/// senpi `importFromJsonl` destination planning: imports are stored beside the
/// current session, reusing the source path when it already lives there and
/// otherwise taking the first free `<stem>[-N].<ext>` name so nothing is clobbered.
pub fn prepare_import_destination(resolved: &std::path::Path, session_dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let file_name = resolved.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{} is not a file path", resolved.display())))?;
    let destination = session_dir.join(file_name);
    let already_stored = match (std::fs::canonicalize(&destination), std::fs::canonicalize(resolved)) {
        (Ok(stored), Ok(source)) => stored == source,
        _ => destination == resolved,
    };
    if already_stored { return Ok(destination); }
    let stem = destination.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let extension = destination.extension().map(|extension| extension.to_string_lossy().into_owned());
    let mut candidate = destination;
    let mut suffix = 1u32;
    while candidate.exists() {
        let name = match &extension { Some(extension) => format!("{stem}-{suffix}.{extension}"), None => format!("{stem}-{suffix}") };
        candidate = session_dir.join(name);
        suffix += 1;
    }
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expandable_text_follows_its_render_closures() {
        let mut expandable = ExpandableText::new(Box::new(|| "collapsed".into()), Box::new(|| "expanded".into()), false, 0, 0);
        assert!(!expandable.is_expanded());
        assert_eq!(expandable.render(80).join("\n").trim(), "collapsed");
        expandable.set_expanded(true);
        assert!(expandable.is_expanded());
        assert_eq!(expandable.render(80).join("\n").trim(), "expanded");
        expandable.set_expanded(false);
        assert_eq!(expandable.render(80).join("\n").trim(), "collapsed");
    }

    #[test]
    fn expandable_text_starts_expanded_when_asked() {
        let mut expandable = ExpandableText::new(Box::new(|| "collapsed".into()), Box::new(|| "expanded".into()), true, 1, 0);
        assert!(expandable.is_expanded());
        assert_eq!(expandable.render(80).join("\n").trim(), "expanded");
    }

    #[test]
    fn debug_log_reports_terminal_size_lines_and_messages() {
        let data = format_debug_log("2026-10-03T00:00:00.000Z", 80, 24, &["hi".into(), "wide".into()], &[r#"{"role":"user"}"#.into()]);
        assert!(data.starts_with("Debug output at 2026-10-03T00:00:00.000Z\nTerminal: 80x24\nTotal lines: 2\n\n"));
        assert!(data.contains("[0] (w=2) \"hi\""));
        assert!(data.contains("[1] (w=4) \"wide\""));
        assert!(data.contains("=== Agent messages (JSONL) ===\n{\"role\":\"user\"}\n"));
    }

    #[test]
    fn debug_log_measures_visible_width_and_escapes_ansi() {
        let data = format_debug_log("t", 10, 10, &["\u{1b}[31mred\u{1b}[0m".into()], &[]);
        assert!(data.contains("(w=3)"));
        assert!(data.contains("\\u001b[31mred"));
    }

    #[test]
    fn resume_command_quotes_only_unsafe_paths() {
        assert_eq!(quote_if_needed("/tmp/sessions-1"), "/tmp/sessions-1");
        assert_eq!(quote_if_needed("/tmp/my sessions"), "'/tmp/my sessions'");
        assert_eq!(quote_if_needed("it's"), "'it'\\''s'");
        assert_eq!(quote_if_needed(""), "''");
    }

    #[test]
    fn resume_command_requires_a_tty_and_a_persisted_session() {
        let manager = maho_core::session_manager::SessionManager::in_memory("/tmp", None, None);
        assert_eq!(format_resume_command(&manager, true), None);
        assert_eq!(format_resume_command(&manager, false), None);
    }

    #[test]
    fn compaction_queue_identifies_extension_commands_by_name() {
        let commands = vec!["deploy".to_owned()];
        assert!(is_extension_command_text("/deploy now", &commands));
        assert!(!is_extension_command_text("/deployx", &commands));
        assert!(!is_extension_command_text("deploy", &commands));
    }

    #[test]
    fn missing_session_cwd_issue_reports_only_a_vanished_recorded_cwd() {
        let directory = tempfile::tempdir().expect("directory");
        let fallback = directory.path().to_string_lossy().into_owned();
        let gone = directory.path().join("gone").to_string_lossy().into_owned();
        let missing = directory.path().join("missing.jsonl");
        std::fs::write(&missing, format!("{}\n", serde_json::json!({"type":"session","id":"s","cwd":gone}))).expect("missing-cwd session");
        let missing = missing.to_string_lossy().into_owned();
        assert_eq!(missing_session_cwd_issue(&missing, &fallback), Some(maho_core::session_cwd::SessionCwdIssue { session_file: Some(missing.clone()), session_cwd: gone, fallback_cwd: fallback.clone() }));
        let present = directory.path().join("present.jsonl");
        std::fs::write(&present, format!("{}\n", serde_json::json!({"type":"session","id":"s","cwd":fallback}))).expect("present-cwd session");
        assert_eq!(missing_session_cwd_issue(&present.to_string_lossy(), &fallback), None);
        assert_eq!(missing_session_cwd_issue(&directory.path().join("absent.jsonl").to_string_lossy(), &fallback), None);
    }

    #[test]
    fn import_destination_reuses_the_stored_path_and_uniquifies_collisions() {
        let directory = tempfile::tempdir().expect("directory");
        let session_dir = directory.path().join("sessions");
        std::fs::create_dir_all(&session_dir).expect("session dir");
        let stored = session_dir.join("session.jsonl");
        std::fs::write(&stored, "stored").expect("stored session");
        assert_eq!(prepare_import_destination(&stored, &session_dir).expect("stored destination"), stored);
        let source = directory.path().join("session.jsonl");
        std::fs::write(&source, "source").expect("source session");
        assert_eq!(prepare_import_destination(&source, &session_dir).expect("collision destination"), session_dir.join("session-1.jsonl"));
        let free = directory.path().join("other.jsonl");
        std::fs::write(&free, "free").expect("free session");
        assert_eq!(prepare_import_destination(&free, &session_dir).expect("free destination"), session_dir.join("other.jsonl"));
    }
}
