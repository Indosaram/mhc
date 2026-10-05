//! Shared seams for the memory slash-command suite.
//!
//! Port of `components/memory/commands/types.ts` at pin 77f3067f1. Handlers read
//! only the structural slice of the extension context the suite needs and return
//! the rendered text; every user-visible line is ALSO pushed through the notify
//! seam so read-only output never enters model context.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use maho_ext_api::{ExtensionActions, ExtensionUi, ExtensionUiDialogOptions, ModelRegistry, UiFuture};
use memory_core::{
    git::exec::GitExec,
    identity::layout::MemoryIdentityPaths,
    reflection::ReflectionEvent,
};

use crate::context::MemoryIdentityContext;
use crate::facts_wiring::FactsExtractorWork;
use crate::prompt::{MemoryPromptHandler, PromptContextResolver};

/// Actionable text returned whenever a session has no bound memory identity.
pub const NOT_BOUND_ERROR: &str =
    "memory is not bound to this session; start a session with memory enabled and retry";

/// Notify severity mirrored from senpi's `"info" | "warning" | "error"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyLevel {
    Info,
    Warning,
    Error,
}

impl NotifyLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

impl From<NotifyLevel> for maho_ext_api::NotificationType {
    fn from(level: NotifyLevel) -> Self {
        match level {
            NotifyLevel::Info => Self::Info,
            NotifyLevel::Warning => Self::Warning,
            NotifyLevel::Error => Self::Error,
        }
    }
}

/// Bound identity for a session: the agent id plus its memory paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryCommandIdentity {
    pub identity: String,
    pub identity_paths: MemoryIdentityPaths,
}

impl From<&MemoryIdentityContext> for MemoryCommandIdentity {
    fn from(context: &MemoryIdentityContext) -> Self {
        Self {
            identity: context.identity.clone(),
            identity_paths: context.identity_paths.clone(),
        }
    }
}

impl From<MemoryIdentityContext> for MemoryCommandIdentity {
    fn from(context: MemoryIdentityContext) -> Self {
        Self::from(&context)
    }
}

/// UI surface the suite writes through; the host adapter forwards to `ExtensionUi`.
pub trait MemoryCommandUi: Send + Sync {
    fn notify(&self, message: &str, level: NotifyLevel);
    /// True when the UI can run an interactive confirmation dialog.
    fn supports_confirm(&self) -> bool {
        false
    }
    fn confirm<'a>(&'a self, title: &'a str, message: &'a str) -> UiFuture<'a, bool> {
        let _ = (title, message);
        Box::pin(async { false })
    }
    fn select<'a>(&'a self, title: &'a str, options: &'a [String]) -> UiFuture<'a, Option<String>> {
        let _ = (title, options);
        Box::pin(async { None })
    }
}

/// Adapter from senpi's live UI to the command UI seam.
pub struct ExtensionUiAdapter(pub Arc<dyn ExtensionUi>);

impl MemoryCommandUi for ExtensionUiAdapter {
    fn notify(&self, message: &str, level: NotifyLevel) {
        self.0.notify(message, level.into());
    }

    fn supports_confirm(&self) -> bool {
        true
    }

    fn confirm<'a>(&'a self, title: &'a str, message: &'a str) -> UiFuture<'a, bool> {
        self.0
            .confirm(title, message, ExtensionUiDialogOptions::default())
    }

    fn select<'a>(&'a self, title: &'a str, options: &'a [String]) -> UiFuture<'a, Option<String>> {
        self.0
            .select(title, options, ExtensionUiDialogOptions::default())
    }
}

/// Structural slice of senpi's extension context the memory commands read.
#[derive(Clone)]
pub struct CommandContext {
    pub ui: Arc<dyn MemoryCommandUi>,
    pub has_ui: bool,
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub session_id: Option<String>,
    pub model_registry: Option<Arc<dyn ModelRegistry>>,
    pub wait_for_idle: Option<Arc<dyn Fn() -> UiFuture<'static, ()> + Send + Sync>>,
}

impl CommandContext {
    /// True when a dialog-capable UI is present (senpi `hasUI`).
    pub fn is_interactive(&self) -> bool {
        self.has_ui && self.ui.supports_confirm()
    }
}

/// Rendered command output plus the level it was notified at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandResponse {
    pub text: String,
    pub level: NotifyLevel,
}

impl CommandResponse {
    pub fn is_error(&self) -> bool {
        self.level == NotifyLevel::Error
    }
}

/// Notify (when a UI is present) and return the same text for headless consumers.
pub fn respond(ctx: &CommandContext, text: impl Into<String>, level: NotifyLevel) -> CommandResponse {
    let text = text.into();
    ctx.ui.notify(&text, level);
    CommandResponse { text, level }
}

/// Build the command context slice from the live extension context.
pub fn command_context_from(context: &maho_ext_api::ExtensionContext) -> CommandContext {
    CommandContext {
        ui: Arc::new(ExtensionUiAdapter(context.ui.clone())),
        has_ui: context.has_ui,
        cwd: context.cwd.clone(),
        agent_dir: context.agent_dir.clone(),
        session_id: Some(context.session_manager.session_id().to_owned()),
        model_registry: Some(context.model_registry.clone()),
        wait_for_idle: Some(context.wait_for_idle_fn.clone()),
    }
}

/// Map a handler response onto the host's command result: errors surface as failures.
pub fn finish(response: CommandResponse) -> Result<(), maho_ext_api::ExtensionFailure> {
    if response.is_error() {
        Err(maho_ext_api::ExtensionFailure::new(response.text))
    } else {
        Ok(())
    }
}

/// Bound identity required by most handlers.
pub fn identity_for(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
) -> Option<MemoryCommandIdentity> {
    let session_id = ctx.session_id.as_deref()?;
    (deps.resolve_context)(session_id).map(|context| MemoryCommandIdentity::from(&context))
}

/// `/memfs init` fallback: bound identity first, then the config-resolved identity.
pub fn identity_for_init(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
) -> Option<MemoryCommandIdentity> {
    identity_for(deps, ctx).or_else(|| {
        deps.resolve_identity
            .as_ref()
            .and_then(|resolve| resolve())
            .map(|context| MemoryCommandIdentity::from(&context))
    })
}

/// Bound identity, or the actionable error text when the session is unbound.
pub fn require_identity(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
) -> Result<MemoryCommandIdentity, String> {
    identity_for(deps, ctx).ok_or_else(|| NOT_BOUND_ERROR.to_owned())
}

/// The reflection runner the command layer calls: `(status, run_id)`.
pub type Reflect = Arc<dyn Fn(&str, ReflectionEvent) -> Result<(String, String), String> + Send + Sync>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ManualReflectionRequest {
    pub focus: Option<String>,
    pub recent_n: Option<usize>,
    pub conversation_ids: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReflectionDisposition {
    Reserved,
    Pending,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflectionRequestReceipt {
    pub disposition: ReflectionDisposition,
    pub run_id: String,
}

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ManualDreamCommandRequest {
    pub focus: Option<String>,
    pub conversation_ids: Option<Vec<String>>,
    pub target_doc: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DreamCommandOutcome {
    Fired { run_id: String, status: String },
    Rejected { rejection: String },
}

/// Manual `/dream` request seam; the native adapter wraps `DreamTriggerWiring`.
pub trait DreamRequestSink: Send + Sync {
    fn request(
        &self,
        request: ManualDreamCommandRequest,
    ) -> BoxFuture<'_, Result<DreamCommandOutcome, String>>;
}

/// `/facts retry` seam: triggers exactly ONE reconcile/launch attempt after an unpark.
pub type FactsRetry = Arc<dyn Fn(String) -> FactsExtractorWork + Send + Sync>;

/// Dialectic-lite seam for `/people --ask`.
pub type PeopleAsk = crate::commands::people_ask::PeopleAskRunner;

/// Complete seam set handed to the command suite at registration time.
#[derive(Clone)]
pub struct MemoryCommandDeps {
    /// Bound identity for a session; `None` when the session has no memory binding.
    pub resolve_context: PromptContextResolver,
    /// Config-resolved identity for `/memfs init`, which may run before any binding exists.
    pub resolve_identity: Option<Arc<dyn Fn() -> Option<MemoryIdentityContext> + Send + Sync>>,
    /// Resolved memory settings for the bound identity.
    pub settings: Arc<dyn Fn() -> Result<serde_json::Value, String> + Send + Sync>,
    /// Prompt-assembly cache seam (`bustPromptCache`): busting makes the next
    /// agent run recompile the memory block from HEAD.
    pub bust_prompt_cache: Arc<dyn Fn() + Send + Sync>,
    /// Path of the omo config file users edit to change memory settings.
    pub config_path: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
    /// Full resolved config; `/people --ask` needs it to resolve the quick model category.
    pub full_config: Option<Arc<dyn Fn() -> Result<serde_json::Value, String> + Send + Sync>>,
    pub actions: Arc<dyn ExtensionActions>,
    pub prompt: Arc<MemoryPromptHandler>,
    /// Senpi sessions root for `/search`; empty means `<agentDir>/sessions`.
    pub sessions_dir: PathBuf,
    /// Manual `/reflect` runner. `None` means reflection is unavailable in this session.
    pub reflect: Option<Reflect>,
    /// Manual `/dream` runner. `None` means dreaming is unavailable in this session.
    pub dream: Option<Arc<dyn DreamRequestSink>>,
    /// `/facts retry` launch seam. `None` means retry clears records but cannot launch.
    pub facts_retry: Option<FactsRetry>,
    /// Git execution seam for read-only raw queries; defaults to a real node git exec.
    pub exec: Option<Arc<dyn GitExec>>,
    /// Environment handed to command-spawned children; defaults to the process environment.
    pub env: Option<BTreeMap<String, String>>,
    /// Dialectic-lite seam for `/people --ask`; defaults to a real quick child.
    pub people_ask: Option<PeopleAsk>,
    /// Clock seam; defaults to the system clock.
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    /// Doctor stale-lock liveness probe; defaults to a real signal-0 probe.
    pub is_process_alive: Option<Arc<dyn Fn(u32) -> bool + Send + Sync>>,
}

impl MemoryCommandDeps {
    /// Current wall clock in epoch milliseconds.
    pub fn now_ms(&self) -> i64 {
        self.now
            .as_ref()
            .map(|now| now())
            .unwrap_or_else(|| memory_core::support::time::now_millis())
    }

    /// Resolved settings, defaulting to an empty object when the resolver is absent.
    pub fn settings_value(&self) -> Result<serde_json::Value, String> {
        (self.settings)()
    }

    /// Config file path for user-facing hints.
    pub fn config_path_value(&self) -> Option<String> {
        self.config_path.as_ref().and_then(|path| path())
    }

    /// Full resolved config for `/people --ask`, defaulting to an empty object.
    pub fn full_config_value(&self) -> serde_json::Value {
        self.full_config
            .as_ref()
            .and_then(|config| config().ok())
            .unwrap_or_else(|| serde_json::json!({}))
    }

    /// Environment for spawned children.
    pub fn environment(&self) -> BTreeMap<String, String> {
        self.env
            .clone()
            .unwrap_or_else(|| std::env::vars().collect())
    }

    /// Sessions directory resolved for a context: the configured root or `<agentDir>/sessions`.
    pub fn sessions_root(&self, ctx: &CommandContext) -> Option<PathBuf> {
        if self.sessions_dir.as_os_str().is_empty() {
            if ctx.agent_dir.as_os_str().is_empty() {
                None
            } else {
                Some(ctx.agent_dir.join("sessions"))
            }
        } else {
            Some(self.sessions_dir.clone())
        }
    }

    /// Doctor stale-lock liveness probe.
    pub fn process_alive(&self, pid: u32) -> bool {
        match &self.is_process_alive {
            Some(probe) => probe(pid),
            None => default_is_process_alive(pid),
        }
    }
}

pub fn default_is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None::<nix::sys::signal::Signal>).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}
