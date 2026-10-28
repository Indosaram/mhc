//! Port of senpi extensions/runner.ts dispatch, using native static registration.
pub type LifecycleAttentionCallback = Arc<std::sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>;
use maho_ext_api::*;
pub type PromptStartObserver<'a> =
    dyn Fn(&str, &str, &str, Option<&[DynamicSystemPromptPart]>) + Send + Sync + 'a;
#[path = "shutdown_quit_budget.rs"]
pub mod shutdown_quit_budget;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct GatedActions {
    inner: Arc<dyn ExtensionActions>,
    gate: Arc<crate::session_start_turn_gate::SessionStartTurnGate>,
}
impl ExtensionActions for GatedActions {
    fn send_message(
        &self,
        message: CustomMessage,
        options: SendMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        if options.trigger_turn {
            let inner = Arc::clone(&self.inner);
            self.gate
                .admit(Box::new(move || inner.send_message(message, options)))
        } else {
            self.inner.send_message(message, options)
        }
    }
    fn send_user_message(
        &self,
        content: UserMessageContent,
        options: SendUserMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        self.inner.send_user_message(content, options)
    }
    fn send_user_message_with_origin(
        &self,
        content: UserMessageContent,
        options: SendUserMessageOptions,
        origin: InputOrigin,
    ) -> Result<(), ExtensionFailure> {
        self.inner
            .send_user_message_with_origin(content, options, origin)
    }
    fn append_entry(
        &self,
        custom_type: &str,
        data: Option<JsonValue>,
    ) -> Result<(), ExtensionFailure> {
        self.inner.append_entry(custom_type, data)
    }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
        self.inner.get_all_tools()
    }
    fn lifecycle(&self) -> Result<Arc<dyn LifecycleApi>, ExtensionFailure> {
        self.inner.lifecycle()
    }
    fn send_itemized_message(
        &self,
        message: CustomMessage,
        options: SendMessageOptions,
        items: Vec<SendMessageItem>,
    ) -> ExtensionFuture<'_, Option<Vec<SendMessageReceipt>>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let inner = Arc::clone(&self.inner);
        let trigger_turn = options.trigger_turn;
        let start: crate::session_start_turn_gate::TurnStart = Box::new(move || {
            tokio::spawn(async move {
                let result = inner.send_itemized_message(message, options, items).await;
                let _receiver_dropped = tx.send(result);
            });
            Ok(())
        });
        let admitted = if trigger_turn {
            self.gate.admit(start)
        } else {
            start()
        };
        Box::pin(async move {
            admitted?;
            rx.await
                .map_err(|_| ExtensionFailure::new("Itemized admission was cancelled"))?
        })
    }
    fn arm_tool_barrier(&self, spec: JsonValue) -> Result<(), ExtensionFailure> {
        self.inner.arm_tool_barrier(spec)
    }
    fn release_tool_barrier(&self, id: &str) -> Result<bool, ExtensionFailure> {
        self.inner.release_tool_barrier(id)
    }
    fn relocation_offered(&self) -> Result<bool, ExtensionFailure> {
        self.inner.relocation_offered()
    }
    fn relocation_targets(&self) -> ExtensionFuture<'_, JsonValue> {
        self.inner.relocation_targets()
    }
    fn request_relocation(&self, request: JsonValue) -> ExtensionFuture<'_, JsonValue> {
        self.inner.request_relocation(request)
    }
    fn redact_for_upload(&self, text: &str, literals: &[String]) -> Option<UploadRedaction> {
        self.inner.redact_for_upload(text, literals)
    }
    fn host_version(&self) -> Option<String> {
        self.inner.host_version()
    }
    fn get_subscription_limits(&self) -> Vec<AccountLimits> {
        self.inner.get_subscription_limits()
    }
    fn on_subscription_limits(
        &self,
        listener: SubscriptionLimitsListener,
    ) -> Option<UiUnsubscribe> {
        self.inner.on_subscription_limits(listener)
    }
    fn send_message_with_receipt(
        &self,
        message: CustomMessage,
        options: SendMessageOptions,
    ) -> Result<MessageDeliveryReceipt, ExtensionFailure> {
        self.inner.send_message_with_receipt(message, options)
    }
}

pub type ErrorListener = Arc<dyn Fn(&ExtensionError) + Send + Sync>;
pub type HookObserver = Arc<dyn Fn(&ToolHookLifecycleEvent) + Send + Sync>;
pub type WarningListener = Arc<dyn Fn(&str) + Send + Sync>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinShortcut {
    pub keybinding: String,
    pub restrict_override: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutDiagnostic {
    pub message: String,
    pub path: String,
}
#[derive(Debug)]
pub struct OwnedContextDispatchError {
    pub messages: Vec<AgentMessage>,
    pub error: ExtensionFailure,
}
struct HookRun {
    event: ToolHookLifecycleEvent,
    state: Arc<std::sync::Mutex<(bool, String)>>,
}
struct ReloadRequest {
    result: std::sync::Mutex<Option<Result<(), ExtensionFailure>>>,
    ready: tokio::sync::Notify,
}
type ReloadState = Arc<std::sync::Mutex<Option<Arc<ReloadRequest>>>>;
struct CommandActions {
    inner: Arc<dyn ExtensionCommandContextActions>,
    reload: ReloadState,
    runtime: ExtensionRuntime,
    replaced: Arc<std::sync::atomic::AtomicBool>,
}
impl ExtensionCommandContextActions for CommandActions {
    fn wait_for_idle(&self) -> ExtensionFuture<'_, ()> {
        Box::pin(async move {
            self.runtime.assert_active()?;
            self.inner.wait_for_idle().await?;
            self.runtime.assert_active()
        })
    }
    fn new_session(
        &self,
        options: NewSessionOptions,
    ) -> ExtensionFuture<'_, SessionNavigationResult> {
        Box::pin(async move {
            self.runtime.assert_active()?;
            let result = self.inner.new_session(options).await?;
            if !result.cancelled {
                self.replaced
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(result)
        })
    }
    fn fork<'a>(
        &'a self,
        entry_id: &'a str,
        options: ForkOptions,
    ) -> ExtensionFuture<'a, SessionNavigationResult> {
        Box::pin(async move {
            self.runtime.assert_active()?;
            let result = self.inner.fork(entry_id, options).await?;
            if !result.cancelled {
                self.replaced
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(result)
        })
    }
    fn navigate_tree<'a>(
        &'a self,
        target_id: &'a str,
        options: ExtensionTreeNavigationOptions,
    ) -> ExtensionFuture<'a, SessionNavigationResult> {
        Box::pin(async move {
            self.runtime.assert_active()?;
            let result = self.inner.navigate_tree(target_id, options).await?;
            self.runtime.assert_active()?;
            Ok(result)
        })
    }
    fn edit_assistant_message<'a>(
        &'a self,
        entry_id: &'a str,
        text: &'a str,
        options: EditMessageOptions,
    ) -> ExtensionFuture<'a, EditMessageResult> {
        Box::pin(async move {
            self.runtime.assert_active()?;
            let result = self
                .inner
                .edit_assistant_message(entry_id, text, options)
                .await?;
            self.runtime.assert_active()?;
            Ok(result)
        })
    }
    fn edit_user_message<'a>(
        &'a self,
        entry_id: &'a str,
        text: &'a str,
        options: EditMessageOptions,
    ) -> ExtensionFuture<'a, EditMessageResult> {
        Box::pin(async move {
            self.runtime.assert_active()?;
            let result = self
                .inner
                .edit_user_message(entry_id, text, options)
                .await?;
            self.runtime.assert_active()?;
            Ok(result)
        })
    }
    fn switch_session<'a>(
        &'a self,
        path: &'a str,
        options: SwitchSessionOptions,
    ) -> ExtensionFuture<'a, SessionNavigationResult> {
        Box::pin(async move {
            self.runtime.assert_active()?;
            let result = self.inner.switch_session(path, options).await?;
            if !result.cancelled {
                self.replaced
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(result)
        })
    }
    fn reload(&self) -> ExtensionFuture<'_, ()> {
        let actions = self.inner.clone();
        Box::pin(async move {
            self.runtime.assert_active()?;
            coalesced_reload(
                self.reload.clone(),
                Box::pin(async move { actions.reload().await }),
            )
            .await?;
            self.replaced
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
    }
}
async fn coalesced_reload(
    state: ReloadState,
    mut operation: ExtensionFuture<'static, ()>,
) -> Result<(), ExtensionFailure> {
    let scope = maho_ai::node::provider_scope::active_provider_scope();
    if scope
        .as_ref()
        .is_some_and(|scope| scope.state() == maho_ai::node::provider_scope::ScopeState::Closed)
    {
        return Err(ExtensionFailure::new(
            maho_ai::node::provider_scope::ProviderScopeError::Closed.to_string(),
        ));
    }
    let request = {
        let mut current = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(request) = current.as_ref() {
            Arc::clone(request)
        } else {
            let request = Arc::new(ReloadRequest {
                result: std::sync::Mutex::new(None),
                ready: tokio::sync::Notify::new(),
            });
            *current = Some(Arc::clone(&request));
            let state = state.clone();
            let pending = request.clone();
            // JS starts reloadHandler synchronously up to its first await. Do not
            // move that admission into a later task behind an incoming prompt.
            let started = operation
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()));
            tokio::spawn(async move {
                let result = match started {
                    std::task::Poll::Ready(result) => result,
                    std::task::Poll::Pending => match &scope {
                        Some(scope) => {
                            match maho_ai::node::provider_scope::run_with_provider_scope_async(
                                scope, operation,
                            )
                            .await
                            {
                                Ok(result) => result,
                                Err(error) => Err(ExtensionFailure::new(error.to_string())),
                            }
                        }
                        None => operation.await,
                    },
                };
                *pending
                    .result
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
                *state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                pending.ready.notify_waiters();
            });
            request
        }
    };
    loop {
        let notified = request.ready.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Some(result) = request
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return result;
        }
        notified.await;
    }
}
tokio::task_local! {
    /// The extension path whose own provider hook must be skipped while it prepares a provider
    /// request. Upstream threads `excludeBeforeProviderRequestExtensionPath` per `createContext`
    /// call; here it travels in a task-local so `ctx.session_manager` stays one stable adapter.
    static PROVIDER_EXCLUDE_PATH: Option<String>;
}
/// Wrap a dispatch handler so its invocation carries the per-extension provider-exclusion path.
/// Every dispatch site iterates `handlers()`, so this single wrapper keeps the path available
/// without allocating a fresh `ctx.session_manager` per dispatch.
///
/// The path is scoped to the handler future, so `ctx.prepare_provider_request` awaited inside a
/// handler resolves the same exclusion upstream captures in the `createContext` closure. A handler
/// that hands `ctx` to a `tokio::spawn` and calls `prepare_provider_request` there loses the path
/// (task-locals do not cross spawns); that is benign only while the owning extension registers no
/// `context`/`before_provider_request` handler (the sole spawn caller, cache-keepalive, registers
/// neither; compaction calls it in-task). See the runner evidence note.
fn with_provider_exclude_path(path: String, handler: ExtensionHandler) -> ExtensionHandler {
    Arc::new(move |event, context| {
        let handler = handler.clone();
        let path = path.clone();
        let mut future = Box::pin(PROVIDER_EXCLUDE_PATH.scope(Some(path), handler(event, context)));
        // A handler still running across a session replacement touches its now-stale ctx, which
        // panics with an `ExtensionFailure` (senpi throws the same error). Surface it as that
        // handler's error instead of unwinding through the UI runtime and aborting the process.
        Box::pin(std::future::poll_fn(
            move |cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                future.as_mut().poll(cx)
            })) {
                Ok(poll) => poll,
                Err(payload) => match payload.downcast::<ExtensionFailure>() {
                    Ok(failure) => std::task::Poll::Ready(Err(*failure)),
                    Err(payload) => std::panic::resume_unwind(payload),
                },
            },
        ))
    })
}
struct ContextSessionManager {
    session: Arc<dyn SessionManager>,
    actions: Arc<dyn ExtensionContextActions>,
    runtime: ExtensionRuntime,
    reload: ReloadState,
    provider_runner: Option<ExtensionRunner>,
}
impl ContextSessionManager {
    fn active(&self) {
        if let Err(error) = self.runtime.assert_active() {
            std::panic::panic_any(error);
        }
    }
}
impl ExtensionKernelTools for ContextSessionManager {
    fn invoke_scope(&self) -> bool {
        self.active();
        let current = crate::kernel_tools_context::current_kernel_tools();
        current
            .as_deref()
            .or_else(|| self.actions.kernel_tools())
            .expect("bound kernel tools")
            .invoke_scope()
    }
    fn describe<'a>(&'a self, names: &'a [String]) -> ExtensionFuture<'a, JsonValue> {
        Box::pin(async move {
            self.runtime.assert_active()?;
            let current = crate::kernel_tools_context::current_kernel_tools();
            let tools = current
                .as_deref()
                .or_else(|| self.actions.kernel_tools())
                .ok_or_else(|| {
                    ExtensionFailure::new("Kernel tools are not available outside an invocation")
                })?;
            let result = tools.describe(names).await?;
            self.runtime.assert_active()?;
            Ok(result)
        })
    }
    fn invoke(
        &self,
        request: KernelToolInvokeRequest,
        options: KernelToolInvokeOptions,
    ) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async move {
            self.runtime.assert_active()?;
            let current = crate::kernel_tools_context::current_kernel_tools();
            let tools = current
                .as_deref()
                .or_else(|| self.actions.kernel_tools())
                .ok_or_else(|| {
                    ExtensionFailure::new("Kernel tools are not available outside an invocation")
                })?;
            let result = tools.invoke(request, options).await?;
            self.runtime.assert_active()?;
            Ok(result)
        })
    }
}
impl ToolSessionManager for ContextSessionManager {
    fn session_id(&self) -> &str {
        self.active();
        self.session.session_id()
    }
    fn session_file(&self) -> Option<&std::path::Path> {
        self.active();
        self.session.session_file()
    }
}
impl SessionManager for ContextSessionManager {
    fn get_session_dir(&self) -> Option<std::path::PathBuf> {
        self.active();
        self.session.get_session_dir()
    }
    fn get_entries(&self) -> Vec<SessionEntry> {
        self.active();
        self.session.get_entries()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        self.active();
        self.session.get_branch()
    }
    fn get_branch_tail(
        &self,
        min_entries: usize,
        satisfied: &mut dyn FnMut(&JsonValue) -> bool,
    ) -> (usize, Vec<SessionEntry>) {
        self.active();
        self.session.get_branch_tail(min_entries, satisfied)
    }
    fn get_leaf_id(&self) -> Option<String> {
        self.active();
        self.session.get_leaf_id()
    }
    fn get_session_name(&self) -> Option<String> {
        self.active();
        self.session.get_session_name()
    }
    fn is_child_session(&self) -> bool {
        self.active();
        self.session.is_child_session()
    }
    fn fork_provenance(&self) -> Option<ForkProvenance> {
        self.active();
        self.session.fork_provenance()
    }
    fn extension_context_actions(&self) -> Option<&dyn ExtensionContextActions> {
        Some(self)
    }
}
impl ExtensionContextActions for ContextSessionManager {
    fn assert_active(&self) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()
    }
    fn set_approved_monitor_parent(
        &self,
        tool_call_id: &str,
        input: &JsonValue,
        parent: &std::path::Path,
    ) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        self.actions
            .set_approved_monitor_parent(tool_call_id, input, parent)?;
        self.runtime.assert_active()
    }
    fn take_approved_monitor_parent(
        &self,
        tool_call_id: &str,
        input: &JsonValue,
    ) -> Result<Option<std::path::PathBuf>, ExtensionFailure> {
        self.runtime.assert_active()?;
        let parent = self
            .actions
            .take_approved_monitor_parent(tool_call_id, input)?;
        self.runtime.assert_active()?;
        Ok(parent)
    }
    fn get_model(&self) -> Option<Model> {
        self.active();
        self.actions.get_model()
    }
    fn get_service_tier(&self) -> Option<ServiceTier> {
        self.active();
        self.actions.get_service_tier()
    }
    fn get_effective_service_tier(&self) -> Option<ServiceTier> {
        self.active();
        self.actions.get_effective_service_tier()
    }
    fn apply_fast_mode(
        &self,
        enabled: bool,
        context: ExtensionContext,
    ) -> ExtensionFuture<'static, FastModeResult> {
        let actions = self.actions.clone();
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.assert_active()?;
            let result = actions.apply_fast_mode(enabled, context).await?;
            runtime.assert_active()?;
            Ok(result)
        })
    }
    fn get_scoped_models(&self) -> Vec<ScopedModel> {
        self.active();
        self.actions.get_scoped_models()
    }
    fn get_agent_dir(&self) -> std::path::PathBuf {
        self.active();
        self.actions.get_agent_dir()
    }
    fn is_idle(&self) -> bool {
        self.active();
        self.actions.is_idle()
    }
    fn engine_turns_held(&self) -> bool {
        self.active();
        self.actions.engine_turns_held()
    }
    fn todo_turn_end_backstop_enabled(&self) -> bool {
        self.active();
        self.actions.todo_turn_end_backstop_enabled()
    }
    fn note_held_wake(&self, source: &str) {
        self.active();
        self.actions.note_held_wake(source);
    }
    fn get_pause_state(&self) -> JsonValue {
        self.active();
        self.actions.get_pause_state()
    }
    fn fence_run(&self, run_ref: JsonValue) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        self.actions.fence_run(run_ref)
    }
    fn supports_release_run_fence(&self) -> bool {
        self.active();
        self.actions.supports_release_run_fence()
    }
    fn release_run_fence(&self, run_ref: JsonValue) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        self.actions.release_run_fence(run_ref)
    }
    fn has_queued_person_input(&self) -> bool {
        self.active();
        self.actions.has_queued_person_input()
    }

    fn offload(
        &self,
        id: &str,
        args: JsonValue,
        options: OffloadOptions,
    ) -> Option<ExtensionFuture<'_, JsonValue>> {
        self.active();
        self.actions.offload(id, args, options)
    }
    fn question_input(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        self.active();
        let inner = self.actions.question_input()?;
        let runtime = self.runtime.clone();
        Some(Arc::new(move || {
            if runtime.assert_active().is_err() {
                return;
            }
            // The answer's own send_user_message follows synchronously and takes this token at admission.
            runtime.mark_person_answer_pending();
            inner();
        }))
    }
    fn is_project_trusted(&self) -> bool {
        self.active();
        self.actions.is_project_trusted()
    }
    fn get_signal(&self) -> Option<AbortSignal> {
        self.active();
        self.actions.get_signal()
    }
    fn get_steering_signal(&self) -> Option<AbortSignal> {
        self.active();
        self.actions.get_steering_signal()
    }
    fn get_thinking_level(&self) -> Option<ThinkingLevel> {
        self.active();
        self.actions.get_thinking_level()
    }
    fn abort(&self, source: Option<AbortSource>) {
        self.active();
        self.actions.abort(source);
    }
    fn has_pending_messages(&self) -> bool {
        self.active();
        self.actions.has_pending_messages()
    }
    fn request_reload(&self) -> ExtensionFuture<'_, ()> {
        let actions = self.actions.clone();
        Box::pin(async move {
            self.runtime.assert_active()?;
            coalesced_reload(
                self.reload.clone(),
                Box::pin(async move { actions.request_reload().await }),
            )
            .await?;
            self.runtime.assert_active()
        })
    }
    fn is_compacting(&self) -> bool {
        self.active();
        self.actions.is_compacting()
    }
    fn get_browser_engine(&self) -> Option<&'static str> {
        self.actions.get_browser_engine()
    }
    fn check_reload_veto(&self) -> ExtensionFuture<'_, ReloadVetoDecision> {
        Box::pin(async move {
            self.runtime.assert_active()?;
            let result = self.actions.check_reload_veto().await?;
            self.runtime.assert_active()?;
            Ok(result)
        })
    }
    fn shutdown(&self) {
        self.active();
        self.actions.shutdown();
    }
    fn get_context_usage(&self) -> Option<ContextUsage> {
        self.active();
        self.actions.get_context_usage()
    }
    fn get_compaction_settings(&self) -> CompactionSettings {
        self.active();
        self.actions.get_compaction_settings()
    }
    fn get_resolved_compaction_settings(&self) -> Option<ResolvedCompactionSettings> {
        self.active();
        self.actions.get_resolved_compaction_settings()
    }
    fn get_compaction_preparation(&self) -> Option<CompactionPreparationDetails> {
        self.active();
        self.actions.get_compaction_preparation()
    }
    fn get_prompt_cache_safe_wait_seconds(&self) -> Option<f64> {
        self.active();
        self.actions.get_prompt_cache_safe_wait_seconds()
    }
    fn get_prompt_cache_goal_backstop_max_seconds(&self) -> f64 {
        self.active();
        self.actions.get_prompt_cache_goal_backstop_max_seconds()
    }
    fn get_prompt_cache_keep_alive_settings(&self) -> PromptCacheKeepAliveSettings {
        self.active();
        self.actions.get_prompt_cache_keep_alive_settings()
    }
    fn get_look_at_settings(&self) -> LookAtSettings {
        self.active();
        self.actions.get_look_at_settings()
    }
    fn get_ask_user_settings(&self) -> AskUserSettings {
        self.active();
        self.actions.get_ask_user_settings()
    }
    fn get_image_settings(&self) -> ImageSettings {
        self.active();
        self.actions.get_image_settings()
    }
    fn session_settings(&self) -> &dyn ExtensionSessionSettings {
        self.active();
        self
    }
    fn compact(&self, options: CompactOptions) {
        self.active();
        self.actions.compact(options);
    }
    fn prepare_provider_request(
        &self,
        messages: Vec<AgentMessage>,
    ) -> ExtensionFuture<'_, ProviderRequestPreparation> {
        Box::pin(async move {
            self.runtime.assert_active()?;
            let exclude = PROVIDER_EXCLUDE_PATH
                .try_with(|path| path.clone())
                .ok()
                .flatten();
            let result = match &self.provider_runner {
                Some(runner) => runner.prepare_provider_request(messages, exclude).await?,
                None => self.actions.prepare_provider_request(messages).await?,
            };
            self.runtime.assert_active()?;
            Ok(result)
        })
    }
    fn begin_compaction(&self, options: BeginCompactionOptions) -> Option<AbortSignal> {
        self.active();
        self.actions.begin_compaction(options)
    }
    fn update_compaction(&self, options: UpdateCompactionOptions) {
        self.active();
        self.actions.update_compaction(options);
    }
    fn end_compaction(&self, options: EndCompactionOptions) {
        self.active();
        self.actions.end_compaction(options);
    }
    fn get_message_revision(&self) -> u64 {
        self.active();
        self.actions.get_message_revision()
    }
    fn apply_compaction(
        &self,
        result: CompactionResult,
        options: ApplyCompactionOptions,
    ) -> ExtensionFuture<'_, ApplyCompactionResult> {
        Box::pin(async move {
            self.runtime.assert_active()?;
            let result = self.actions.apply_compaction(result, options).await?;
            self.runtime.assert_active()?;
            Ok(result)
        })
    }
    fn get_system_prompt(&self) -> String {
        self.active();
        self.actions.get_system_prompt()
    }
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions {
        self.active();
        self.actions.get_system_prompt_options()
    }
    fn get_loaded_hook_sources(&self) -> LoadedHookSources {
        self.active();
        self.actions.get_loaded_hook_sources()
    }
    fn get_registered_mcp_servers(&self) -> Option<Vec<RegisteredMcpServerDeclaration>> {
        self.active();
        self.provider_runner
            .as_ref()
            .map(ExtensionRunner::get_registered_mcp_servers)
            .or_else(|| self.actions.get_registered_mcp_servers())
    }
    fn kernel_tools(&self) -> Option<&dyn ExtensionKernelTools> {
        self.active();
        if crate::kernel_tools_context::current_kernel_tools().is_some()
            || self.actions.kernel_tools().is_some()
        {
            Some(self)
        } else {
            None
        }
    }
}

impl ExtensionSessionSettings for ContextSessionManager {
    fn get_retry_fallback_settings(&self) -> RetryFallbackSettings {
        self.active();
        self.actions
            .session_settings()
            .get_retry_fallback_settings()
    }
    fn set_fallback_chain<'a>(
        &'a self,
        key: &'a str,
        entries: &'a [String],
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            self.runtime.assert_active()?;
            self.actions
                .session_settings()
                .set_fallback_chain(key, entries)
                .await?;
            self.runtime.assert_active()
        })
    }
    fn remove_fallback_chain<'a>(&'a self, key: &'a str) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            self.runtime.assert_active()?;
            self.actions
                .session_settings()
                .remove_fallback_chain(key)
                .await?;
            self.runtime.assert_active()
        })
    }
    fn set_model_fallback_enabled(&self, enabled: bool) -> ExtensionFuture<'_, ()> {
        Box::pin(async move {
            self.runtime.assert_active()?;
            self.actions
                .session_settings()
                .set_model_fallback_enabled(enabled)
                .await?;
            self.runtime.assert_active()
        })
    }
    fn set_fallback_revert_policy(&self, policy: FallbackRevertPolicy) -> ExtensionFuture<'_, ()> {
        Box::pin(async move {
            self.runtime.assert_active()?;
            self.actions
                .session_settings()
                .set_fallback_revert_policy(policy)
                .await?;
            self.runtime.assert_active()
        })
    }
    fn reload(&self) -> ExtensionFuture<'_, ()> {
        Box::pin(async move {
            self.runtime.assert_active()?;
            self.actions.session_settings().reload().await?;
            self.runtime.assert_active()
        })
    }
    fn get_fallback_status(&self) -> Option<RetryFallbackStatus> {
        self.active();
        self.actions.session_settings().get_fallback_status()
    }
}

#[derive(Clone)]
pub struct ExtensionRunner {
    pub extensions: Arc<Vec<LoadedExtension>>,
    pub runtime: ExtensionRuntime,
    pub events: EventBus,
    context: ExtensionContext,
    error_listeners: Vec<ErrorListener>,
    pub errors: Vec<ExtensionError>,
    pub warnings: Vec<String>,
    pub shutdown_warn_ms: u64,
    pub shutdown_timeout_ms: u64,
    hook_observer: Option<HookObserver>,
    lifecycle_attention: LifecycleAttentionCallback,
    lifecycle_human_wait: Arc<std::sync::Mutex<Option<JsonValue>>>,
    warning_listener: Option<WarningListener>,
    next_hook_index: u64,
    context_actions: Option<Arc<dyn ExtensionContextActions>>,
    reload: ReloadState,
    shutdown_budget: Option<Arc<dyn Fn() -> (u64, u64) + Send + Sync>>,
    /// Total wait across every `session_shutdown` handler of a quit that exits the process (senpi#3711); 0 disables.
    shutdown_quit_budget: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
    /// When the process-exit quit was requested; shared across clones so the deadline counts from the request.
    process_exit_requested_at: Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    runtime_factory: Option<RuntimeFactory>,
    /// One stable `ctx.session_manager` adapter per runner (session/generation); see
    /// `create_context_for_extension`. Cleared whenever the bound context/actions change.
    context_session_manager: Arc<std::sync::Mutex<Option<Arc<dyn SessionManager>>>>,
    /// The host-bound UI and mode, shared by every clone of this runner. Upstream `createContext`
    /// reads `runner.uiContext` / `runner.mode` through live getters, so a clone taken before the
    /// host binds (e.g. the tool context factory) must still observe the later binding.
    live_bindings: Arc<std::sync::Mutex<LiveBindings>>,
    factory_context: ExtensionContext,
    /// The session-bound error listener (pinned `_extensionErrorUnsubscriber` swap in `_applyExtensionBindings`).
    session_error_listener: Option<ErrorListener>,
    handle_ledgers: crate::extension_handle_ledger::RunnerHandleLedgers,
    session_start_turns: Arc<crate::session_start_turn_gate::SessionStartTurnGate>,
    reported_child_module_replacements: Arc<std::sync::Mutex<BTreeSet<String>>>,
}
#[derive(Default)]
struct LiveBindings {
    ui: Option<Arc<dyn ExtensionUi>>,
    mode: Option<(ExtensionMode, bool)>,
}
pub type RuntimeFactory =
    Arc<dyn Fn(ExtensionContext) -> ExtensionFuture<'static, ExtensionRunner> + Send + Sync>;
struct SharedExtension(Arc<dyn Extension>);
impl Extension for SharedExtension {
    fn register(&self, api: &mut ExtensionApi) {
        self.0.register(api);
    }
}
impl ExtensionRunner {
    /// Loaded extensions' live memory reporters, in extension load order.
    /// Report construction keeps the first registration of each name.
    pub fn get_memory_reporters(&self) -> Vec<NamedMemoryReporter> {
        self.extensions
            .iter()
            .flat_map(|extension| self.runtime.memory_reporters(&extension.identity.path))
            .collect()
    }
    pub fn new(
        extensions: Vec<LoadedExtension>,
        runtime: ExtensionRuntime,
        events: EventBus,
        context: ExtensionContext,
    ) -> Self {
        let handle_ledgers =
            crate::extension_handle_ledger::RunnerHandleLedgers::register_runner_extensions(
                &extensions,
            );
        Self {
            handle_ledgers,
            session_start_turns: Arc::new(
                crate::session_start_turn_gate::SessionStartTurnGate::new(Arc::new(|_| {})),
            ),
            reported_child_module_replacements: Arc::default(),
            extensions: Arc::new(extensions),
            runtime,
            events,
            factory_context: context.clone(),
            context,
            error_listeners: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            shutdown_warn_ms: 2000,
            shutdown_timeout_ms: 10000,
            hook_observer: None,
            lifecycle_attention: Arc::default(),
            lifecycle_human_wait: Arc::default(),
            warning_listener: None,
            next_hook_index: 0,
            context_actions: None,
            reload: Arc::new(std::sync::Mutex::new(None)),
            shutdown_budget: None,
            shutdown_quit_budget: None,
            process_exit_requested_at: Arc::new(std::sync::Mutex::new(None)),
            runtime_factory: None,
            context_session_manager: Arc::new(std::sync::Mutex::new(None)),
            live_bindings: Arc::default(),
            session_error_listener: None,
        }
    }
    pub fn from_static(extensions: Vec<Box<dyn Extension>>, context: ExtensionContext) -> Self {
        Self::from_shared_static(extensions.into_iter().map(Arc::from).collect(), context)
    }
    fn from_shared_static(extensions: Vec<Arc<dyn Extension>>, context: ExtensionContext) -> Self {
        let retained = extensions.clone();
        let factories = extensions
            .into_iter()
            .enumerate()
            .map(|(index, extension)| {
                let path = format!("<inline:{}>", index.saturating_add(1));
                let source_info = SourceInfo {
                    path: path.clone(),
                    source: "inline".into(),
                    ..SourceInfo::default()
                };
                crate::loader::NativeExtensionFactory {
                    path,
                    source_info,
                    extension: Box::new(SharedExtension(extension)),
                }
            })
            .collect();
        let loaded = crate::loader::load_extensions(
            factories,
            &context.cwd,
            ExtensionSessionProfile::default(),
        );
        let mut runner = Self::new(loaded.extensions, loaded.runtime, loaded.events, context);
        runner.runtime_factory = Some(Arc::new(move |context| {
            let extensions = retained.clone();
            Box::pin(async move { Ok(Self::from_shared_static(extensions, context)) })
        }));
        for error in loaded.errors {
            runner.emit_error(error);
        }
        runner
    }
    pub fn set_runtime_factory(&mut self, factory: RuntimeFactory) {
        self.runtime_factory = Some(factory);
    }
    pub fn bind_native_factory_loader(
        &mut self,
        loader: Arc<dyn Fn() -> Vec<crate::loader::NativeExtensionFactory> + Send + Sync>,
        profile: ExtensionSessionProfile,
    ) {
        self.set_runtime_factory(Arc::new(move |context| {
            let loaded = crate::loader::load_extensions(loader(), &context.cwd, profile.clone());
            let loader = loader.clone();
            let profile = profile.clone();
            Box::pin(async move {
                let mut runner =
                    Self::new(loaded.extensions, loaded.runtime, loaded.events, context);
                runner.bind_native_factory_loader(loader, profile);
                for error in loaded.errors {
                    runner.emit_error(error);
                }
                Ok(runner)
            })
        }));
    }
    pub fn from_async_factories(
        factories: Vec<crate::loader::NativeAsyncExtensionFactory>,
        context: ExtensionContext,
        profile: ExtensionSessionProfile,
    ) -> ExtensionFuture<'static, Self> {
        Box::pin(async move {
            let retained = factories.clone();
            let loaded =
                crate::loader::load_extensions_async(factories, &context.cwd, profile.clone())
                    .await;
            let mut runner = Self::new(loaded.extensions, loaded.runtime, loaded.events, context);
            runner.set_runtime_factory(Arc::new(move |context| {
                Self::from_async_factories(retained.clone(), context, profile.clone())
            }));
            for error in loaded.errors {
                runner.emit_error(error);
            }
            Ok(runner)
        })
    }
    pub fn from_loaded_extensions(
        loaded: crate::loader::LoadExtensionsResult,
        factories: Vec<crate::loader::NativeAsyncExtensionFactory>,
        context: ExtensionContext,
        profile: ExtensionSessionProfile,
    ) -> Self {
        let mut runner = Self::new(loaded.extensions, loaded.runtime, loaded.events, context);
        runner.set_runtime_factory(Arc::new(move |context| {
            Self::from_async_factories(factories.clone(), context, profile.clone())
        }));
        for error in loaded.errors {
            runner.emit_error(error);
        }
        runner
    }
    pub async fn recreate(&self) -> Result<Self, ExtensionFailure> {
        self.recreate_with_context(self.factory_context.clone())
            .await
    }
    pub async fn recreate_with_context(
        &self,
        context: ExtensionContext,
    ) -> Result<Self, ExtensionFailure> {
        let factory = self
            .runtime_factory
            .as_ref()
            .ok_or_else(|| ExtensionFailure::new("Extension runtime factory is not bound"))?;
        let mut runner = factory(context).await?;
        runner.runtime_factory = self.runtime_factory.clone();
        runner.error_listeners = self.error_listeners.clone();
        runner.session_error_listener = self.session_error_listener.clone();
        runner.warning_listener = self.warning_listener.clone();
        runner.hook_observer = self.hook_observer.clone();
        runner.shutdown_budget = self.shutdown_budget.clone();
        runner.shutdown_quit_budget = self.shutdown_quit_budget.clone();
        runner.shutdown_warn_ms = self.shutdown_warn_ms;
        runner.shutdown_timeout_ms = self.shutdown_timeout_ms;
        Ok(runner)
    }
    pub fn ensure_recreation_available(&self) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        self.runtime_factory
            .as_ref()
            .ok_or_else(|| ExtensionFailure::new("Extension runtime factory is not bound"))?;
        Ok(())
    }
    pub fn bind_core(&mut self, actions: Arc<dyn ExtensionActions>, context: ExtensionContext) {
        self.runtime.bind(Arc::new(GatedActions {
            inner: actions,
            gate: Arc::clone(&self.session_start_turns),
        }));
        self.context = context;
        *self
            .context_session_manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
    pub fn bind_context_actions(
        &mut self,
        actions: Arc<dyn ExtensionContextActions>,
    ) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        let prompt_actions = Arc::clone(&actions);
        self.context.get_system_prompt_fn = Arc::new(move || prompt_actions.get_system_prompt());
        let option_actions = Arc::clone(&actions);
        self.context.get_system_prompt_options_fn =
            Arc::new(move || option_actions.get_system_prompt_options());
        self.context_actions = Some(Arc::clone(&actions));
        self.context.session_manager = Arc::new(ContextSessionManager {
            session: Arc::clone(&self.context.session_manager),
            actions,
            runtime: self.runtime.clone(),
            reload: Arc::clone(&self.reload),
            provider_runner: None,
        });
        *self
            .context_session_manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        Ok(())
    }
    pub fn bind_providers(
        &mut self,
        actions: Arc<dyn ExtensionProviderActions>,
    ) -> Result<(), ExtensionFailure> {
        self.runtime.bind_providers(actions)?;
        for error in self.runtime.take_provider_errors() {
            self.emit_error(error);
        }
        Ok(())
    }
    pub fn bind_ui(&mut self, ui: Arc<dyn ExtensionUi>) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        let wait = Arc::clone(&self.lifecycle_human_wait);
        let attention = Arc::clone(&self.lifecycle_attention);
        let ui: Arc<dyn ExtensionUi> = Arc::new(crate::ui::LifecycleUi::new(
            ui,
            self.runtime.clone(),
            Arc::new(move |event| {
                match event {
                    ExtensionEvent::UiPromptStart { kind, .. } => {
                        *wait
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(
                            serde_json::json!({"kind": if matches!(kind, UiPromptKind::Confirm) { "approval" } else { "question" }, "requestId": uuid::Uuid::new_v4().to_string(), "revision": 1, "since": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)}),
                        );
                    }
                    ExtensionEvent::UiPromptEnd { .. } => {
                        wait.lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take();
                    }
                    _ => return,
                }
                let observer = attention
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                if let Some(observer) = observer {
                    observer();
                }
            }),
        ));
        self.context.ui = Arc::clone(&ui);
        self.live_bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ui = Some(ui);
        *self
            .context_session_manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        Ok(())
    }
    pub fn set_extension_mode(
        &mut self,
        mode: ExtensionMode,
        has_ui: bool,
    ) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        self.context.mode = mode;
        self.context.has_ui = has_ui;
        self.live_bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .mode = Some((mode, has_ui));
        Ok(())
    }
    pub fn bind_session_actions(
        &self,
        actions: Arc<dyn ExtensionSessionActions>,
    ) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        for extension in self.extensions.iter() {
            for activator in &extension.lazy_tool_activators {
                actions.register_lazy_tool_activator(Arc::clone(activator))?;
            }
            for source in &extension.native_tool_search_injection_failure_sources {
                actions.register_native_tool_search_injection_failure_source(Arc::clone(source))?;
            }
            for (name, hint) in &extension.removed_tool_hints {
                actions.register_removed_tool_hint(name, hint)?;
            }
        }
        self.runtime.bind_session_actions(actions);
        Ok(())
    }
    pub fn create_command_context(
        &self,
        actions: Arc<dyn ExtensionCommandContextActions>,
    ) -> Result<ExtensionCommandContext, ExtensionFailure> {
        Ok(ExtensionCommandContext {
            context: self.create_context()?,
            actions: Arc::new(CommandActions {
                inner: actions,
                reload: self.reload.clone(),
                runtime: self.runtime.clone(),
                replaced: Arc::default(),
            }),
            runtime: self.runtime.clone(),
        })
    }
    pub async fn prepare_provider_request(
        &self,
        messages: Vec<AgentMessage>,
        exclude_path: Option<String>,
    ) -> Result<ProviderRequestPreparation, ExtensionFailure> {
        self.runtime.assert_active()?;
        let mut context_runner = self.clone();
        let messages = context_runner
            .emit_context_owned_from(
                messages,
                &[],
                exclude_path.as_deref(),
                ContextEventSource::Projection,
            )
            .await
            .map_err(|failure| failure.error)?;
        let payload_runner = self.clone();
        let header_runner = self.clone();
        Ok(ProviderRequestPreparation {
            messages,
            transform_payload: Arc::new(move |payload| {
                let mut runner = payload_runner.clone();
                let exclude = exclude_path.clone();
                Box::pin(async move {
                    runner.runtime.assert_active()?;
                    runner
                        .emit_before_provider_request(payload, exclude.as_deref())
                        .await
                })
            }),
            transform_headers: Arc::new(move |headers| {
                let mut runner = header_runner.clone();
                Box::pin(async move {
                    runner.runtime.assert_active()?;
                    runner.emit_before_provider_headers(headers).await
                })
            }),
        })
    }
    pub async fn invoke_command(
        &self,
        name: &str,
        args: &str,
        context: &ExtensionCommandContext,
    ) -> Result<(), ExtensionFailure> {
        self.runtime.assert_active()?;
        let commands = self.get_registered_commands();
        let resolved = commands
            .iter()
            .find(|command| command.invocation_name == name)
            .ok_or_else(|| ExtensionFailure::new(format!("Unknown extension command: {name}")))?;
        let handlers = self
            .extensions
            .iter()
            .find_map(|extension| {
                let (commands, handlers) = self.live_command_snapshot(extension);
                commands
                    .iter()
                    .any(|command| Arc::ptr_eq(&command.handler, &resolved.command.handler))
                    .then_some(handlers)
            })
            .ok_or_else(|| ExtensionFailure::new("Command owner is unavailable"))?;
        // A command may retire its own runtime through session navigation. Track that per
        // invocation so an unrelated invalidation still rejects a pending command's completion.
        let replaced = Arc::default();
        let context = ExtensionCommandContext {
            actions: Arc::new(CommandActions {
                inner: context.actions.clone(),
                reload: Arc::default(),
                runtime: self.runtime.clone(),
                replaced: Arc::clone(&replaced),
            }),
            ..context.clone()
        };
        match handlers.get(&resolved.command.name) {
            Some(handler) => handler(args, &context).await,
            None => (resolved.command.handler)(args, &context.context).await,
        }?;
        if replaced.load(std::sync::atomic::Ordering::SeqCst) {
            Ok(())
        } else {
            self.runtime.assert_active()
        }
    }
    pub async fn apply_fast_mode(
        &self,
        enabled: bool,
        context: &ExtensionContext,
    ) -> Result<FastModeResult, ExtensionFailure> {
        self.runtime.assert_active()?;
        let extension = self
            .extensions
            .iter()
            .find(|extension| extension.identity.path == "<builtin:service-tier>")
            .ok_or_else(|| ExtensionFailure::new("Fast mode extension is not registered"))?;
        let action = extension
            .fast_mode_action
            .as_ref()
            .ok_or_else(|| ExtensionFailure::new("Fast mode action is not registered"))?;
        let result = action(enabled, context.clone()).await?;
        self.runtime.assert_active()?;
        Ok(result)
    }
    pub fn get_shortcuts(&self) -> BTreeMap<String, ExtensionShortcut> {
        let mut shortcuts = BTreeMap::new();
        for extension in self.extensions.iter() {
            for (key, shortcut) in self
                .runtime
                .live_shortcuts(&extension.identity.path)
                .unwrap_or_else(|| extension.shortcuts.clone())
            {
                shortcuts.insert(key.to_lowercase(), shortcut);
            }
        }
        shortcuts
    }
    pub fn resolve_shortcuts(
        &self,
        builtins: &BTreeMap<String, BuiltinShortcut>,
    ) -> (BTreeMap<String, ExtensionShortcut>, Vec<ShortcutDiagnostic>) {
        let mut shortcuts: BTreeMap<String, ExtensionShortcut> = BTreeMap::new();
        let mut diagnostics = Vec::new();
        for extension in self.extensions.iter() {
            for (key, shortcut) in self
                .runtime
                .live_shortcuts(&extension.identity.path)
                .unwrap_or_else(|| extension.shortcuts.clone())
            {
                let normalized = key.to_lowercase();
                if let Some(builtin) = builtins.get(&normalized) {
                    if builtin.restrict_override {
                        diagnostics.push(ShortcutDiagnostic { message: format!("Extension shortcut '{key}' from {} conflicts with built-in shortcut. Skipping.", shortcut.extension_path), path: shortcut.extension_path.clone() });
                        continue;
                    }
                    diagnostics.push(ShortcutDiagnostic { message: format!("Extension shortcut conflict: '{key}' is built-in shortcut for {} and {}. Using {}.", builtin.keybinding, shortcut.extension_path, shortcut.extension_path), path: shortcut.extension_path.clone() });
                }
                if let Some(existing) = shortcuts.get(&normalized) {
                    diagnostics.push(ShortcutDiagnostic { message: format!("Extension shortcut conflict: '{key}' registered by both {} and {}. Using {}.", existing.extension_path, shortcut.extension_path, shortcut.extension_path), path: shortcut.extension_path.clone() });
                }
                shortcuts.insert(normalized, shortcut.clone());
            }
        }
        (shortcuts, diagnostics)
    }
    pub fn transform_markdown(&self, markdown: &str, context: &MarkdownTransformContext) -> String {
        let mut transformed = markdown.to_owned();
        for extension in self.extensions.iter() {
            if let Some(transformer) = self
                .runtime
                .live_markdown_transformer(&extension.identity.path)
                .or_else(|| extension.markdown_transformer.clone())
            {
                transformed = transformer(&transformed, context);
            }
        }
        transformed
    }
    pub async fn handle_rpc_request(
        &self,
        name: &str,
        data: JsonValue,
    ) -> Result<JsonValue, ExtensionFailure> {
        self.runtime.assert_active()?;
        let name = name.trim();
        if name.is_empty() {
            return Err(ExtensionFailure::new(
                "Extension RPC request name must not be empty",
            ));
        }
        let mut handlers = self.extensions.iter().filter_map(|extension| {
            self.runtime
                .live_rpc_handlers(&extension.identity.path)
                .unwrap_or_else(|| extension.rpc_handlers.clone())
                .get(name)
                .cloned()
        });
        let handler = handlers.next().ok_or_else(|| {
            ExtensionFailure::new(format!("Unknown extension RPC request: {name}"))
        })?;
        if handlers.next().is_some() {
            return Err(ExtensionFailure::new(format!(
                "Multiple extension RPC request handlers registered: {name}"
            )));
        }
        let result = handler(data).await?;
        self.runtime.assert_active()?;
        Ok(result)
    }
    pub fn create_context(&self) -> Result<ExtensionContext, ExtensionFailure> {
        self.create_context_for_extension(None)
    }
    /// `_exclude_path` is retained for call-site parity; the provider-exclusion path now travels in
    /// the `PROVIDER_EXCLUDE_PATH` task-local (set by `with_provider_exclude_path`), so the returned
    /// `ctx.session_manager` can be ONE stable adapter per runner (upstream `runner.sessionManager`).
    pub fn create_context_for_extension(
        &self,
        _exclude_path: Option<&str>,
    ) -> Result<ExtensionContext, ExtensionFailure> {
        self.runtime.assert_active()?;
        let mut context = self.context.clone();
        {
            let live = self
                .live_bindings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(ui) = &live.ui {
                context.ui = Arc::clone(ui);
            }
            if let Some((mode, has_ui)) = live.mode {
                context.mode = mode;
                context.has_ui = has_ui;
            }
        }
        if let Some(actions) = &self.context_actions {
            // ONE stable adapter per runner (session/generation) so identity matches upstream's
            // single `runner.sessionManager`. A single locked `get_or_insert_with` keeps concurrent
            // dispatches from allocating distinct adapters; the closure only reads `self` (Arc clone,
            // never re-locks the cache), so it cannot recursively lock.
            let adapter = self
                .context_session_manager
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_or_insert_with(|| {
                    // The adapter reaches the runner to re-run the nested provider dispatch, but that
                    // clone must NOT reach this cache through `context_session_manager` or the pair forms
                    // a strong cycle (`cache -> adapter -> runner -> cache`) that keeps the runner and its
                    // runtime alive forever. Detach the clone's cache so the edge never closes; the
                    // adapter still owns the clone, preserving its lifetime.
                    let mut provider_runner = self.clone();
                    provider_runner.context_session_manager = Arc::new(std::sync::Mutex::new(None));
                    Arc::new(ContextSessionManager {
                        session: Arc::clone(&self.context.session_manager),
                        actions: Arc::clone(actions),
                        runtime: self.runtime.clone(),
                        reload: Arc::clone(&self.reload),
                        provider_runner: Some(provider_runner),
                    }) as Arc<dyn SessionManager>
                })
                .clone();
            context.session_manager = adapter;
            // Fresh per-invocation signal, matching upstream `createContext`'s closure variable.
            context.compaction_signal = Arc::new(std::sync::Mutex::new(None));
        }
        if let Some(actions) = context.session_manager.extension_context_actions() {
            actions.assert_active()?;
            context.model = actions.get_model();
            context.service_tier = actions.get_service_tier();
            context.effective_service_tier = actions.get_effective_service_tier();
            context.scoped_models = actions.get_scoped_models();
            context.agent_dir = actions.get_agent_dir();
            context.signal = actions.get_signal();
            context.steering_signal = actions.get_steering_signal();
            context.thinking_level = actions.get_thinking_level();
        }
        context.loaded_extension_paths = self
            .extensions
            .iter()
            .map(|e| e.identity.resolved_path.clone())
            .collect();
        context.registered_mcp_servers = self.get_registered_mcp_servers();
        Ok(context)
    }
    pub fn invalidate(&self, message: &str) {
        self.runtime.invalidate(message);
        self.events.clear();
    }
    pub async fn emit_removed_extensions(
        &mut self,
        reason: SessionReason,
        removed: Vec<ExtensionIdentity>,
    ) {
        let mut event = ExtensionEvent::SessionExtensionsRemoved { reason, removed };
        for (path, handler) in self.handlers(EventKind::SessionExtensionsRemoved) {
            if let Err(error) = handler(&mut event, &self.context).await {
                self.report(&path, EventKind::SessionExtensionsRemoved, error);
            }
        }
    }
    pub fn on_error(&mut self, listener: ErrorListener) {
        self.error_listeners.push(listener);
    }
    /// Replace the single session-bound error listener (pinned unsubscribe-then-`onError`).
    pub fn set_session_error_listener(&mut self, listener: Option<ErrorListener>) {
        self.session_error_listener = listener;
    }
    pub fn has_session_error_listener(&self) -> bool {
        self.session_error_listener.is_some()
    }
    pub fn set_tool_hook_lifecycle_observer(&mut self, observer: Option<HookObserver>) {
        self.hook_observer = observer;
    }
    pub fn set_warning_listener(&mut self, listener: Option<WarningListener>) {
        self.warning_listener = listener;
    }
    pub fn set_shutdown_budget_resolver(
        &mut self,
        resolver: Arc<dyn Fn() -> (u64, u64) + Send + Sync>,
    ) {
        self.shutdown_budget = Some(resolver);
    }
    pub fn set_shutdown_quit_budget_resolver(
        &mut self,
        resolver: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) {
        self.shutdown_quit_budget = Some(resolver);
    }
    /// The next `quit` emission ends in process exit, so its handlers share the total quit budget (senpi#3711).
    /// A quit that keeps the process alive (closing one session on an RPC host) keeps only the per-handler limits.
    pub fn arm_process_exit_budget(&self) {
        if let Ok(mut requested) = self.process_exit_requested_at.lock() {
            requested.get_or_insert_with(std::time::Instant::now);
        }
    }
    /// senpi `emitSessionShutdownEvent`: arms the process-exit quit budget only for a `quit` that exits
    /// the process, then emits when any handler is registered. Returns whether handlers were emitted to.
    pub async fn emit_session_shutdown_event(
        &mut self,
        event: SessionShutdownEvent,
        process_exit: bool,
    ) -> Result<bool, ExtensionFailure> {
        if process_exit && event.reason == SessionReason::Quit {
            self.arm_process_exit_budget();
        }
        let emitted = self.has_handlers(EventKind::SessionShutdown);
        let outcome = if emitted {
            self.emit(ExtensionEvent::SessionShutdown(event))
                .await
                .map(|_| ())
        } else {
            Ok(())
        };
        self.reclaim_handles();
        outcome.map(|()| emitted)
    }
    pub fn emit_error(&mut self, error: ExtensionError) {
        for listener in &self.error_listeners {
            listener(&error);
        }
        if let Some(listener) = &self.session_error_listener {
            listener(&error);
        }
        self.errors.push(error);
    }
    fn report(&mut self, path: &str, kind: EventKind, error: ExtensionFailure) {
        self.emit_error(ExtensionError {
            extension_path: path.into(),
            event: kind.as_str().into(),
            error: error.message,
            stack: error.stack,
        });
    }
    pub fn has_handlers(&self, kind: EventKind) -> bool {
        self.extensions.iter().any(|e| {
            e.handlers
                .get(&kind)
                .is_some_and(|handlers| !handlers.is_empty())
                || self.runtime.has_live_handlers(&e.identity.path, kind)
        })
    }
    pub fn set_lifecycle_attention_observer(&self, observer: Arc<dyn Fn() + Send + Sync>) {
        *self
            .lifecycle_attention
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(observer);
    }
    pub fn get_lifecycle_human_wait(&self) -> Option<JsonValue> {
        if self.context.mode == ExtensionMode::Rpc {
            return None;
        }
        self.lifecycle_human_wait
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn get_third_party_session_start_paths(&self) -> Vec<String> {
        self.extensions
            .iter()
            .filter(|ext| {
                ext.source_info.source != "builtin"
                    && ext
                        .handlers
                        .get(&EventKind::SessionStart)
                        .is_some_and(|handlers| !handlers.is_empty())
            })
            .map(|ext| ext.identity.path.clone())
            .collect()
    }

    pub fn extension_paths(&self) -> Vec<String> {
        self.extensions
            .iter()
            .map(|extension| extension.identity.path.clone())
            .collect()
    }
    pub fn third_party_session_start_paths(&self) -> Vec<String> {
        self.get_third_party_session_start_paths()
    }

    pub fn preview_unsafe_before_agent_start_paths(&self) -> Vec<String> {
        self.extensions
            .iter()
            .filter_map(|extension| {
                let handlers = extension.handlers.get(&EventKind::BeforeAgentStart)?;
                let options = extension.handler_options.get(&EventKind::BeforeAgentStart);
                handlers
                    .iter()
                    .enumerate()
                    .any(|(index, _)| {
                        !options
                            .and_then(|options| options.get(index))
                            .is_some_and(|options| options.preview_safe)
                    })
                    .then(|| extension.identity.path.clone())
            })
            .collect()
    }
    fn handlers(&self, kind: EventKind) -> Vec<(String, ExtensionHandler)> {
        self.raw_handlers(kind)
            .into_iter()
            .map(|(path, handler)| {
                let handler = crate::extension_handle_ledger::owned_by(
                    self.handle_ledgers.ledger_for(&path),
                    handler,
                );
                (path.clone(), with_provider_exclude_path(path, handler))
            })
            .collect()
    }
    /// True while `session_start` handlers run and the turns they trigger are still held.
    pub fn session_start_turns_held(&self) -> bool {
        self.session_start_turns.holding()
    }
    /// The gate that holds extension-requested turns while `session_start` handlers dispatch.
    pub fn session_start_turns(&self) -> Arc<crate::session_start_turn_gate::SessionStartTurnGate> {
        Arc::clone(&self.session_start_turns)
    }
    /// Every registered child module by id. The last extension to register an id wins, in load
    /// order; each replacement is logged once.
    pub fn get_all_child_modules(&self) -> BTreeMap<String, RegisteredChildModule> {
        let mut reported = self
            .reported_child_module_replacements
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (modules, warnings) = merge_child_modules(
            self.extensions
                .iter()
                .map(|extension| (extension.identity.path.as_str(), &extension.child_modules)),
            &mut reported,
        );
        drop(reported);
        for warning in warnings {
            diagnostic(&warning);
        }
        modules
    }
    /// Emits `held_wakes_released`: sources released by real input or an explicit Resume.
    pub async fn emit_held_wakes_released(
        &mut self,
        sources: BTreeMap<String, u32>,
    ) -> Result<(), ExtensionFailure> {
        self.emit(ExtensionEvent::HeldWakesReleased { sources })
            .await
            .map(|_| ())
    }
    /// `reclaimRunnerHandles`: closes leaked handles and reports one warning per extension.
    pub fn reclaim_handles(&mut self) {
        for message in self.handle_ledgers.reclaim_all() {
            if let Some(listener) = &self.warning_listener {
                listener(&message);
            }
            self.warnings.push(message);
        }
    }
    fn raw_handlers(&self, kind: EventKind) -> Vec<(String, ExtensionHandler)> {
        let mut out = Vec::new();
        for e in self.extensions.iter() {
            // Load-registered handlers stay authoritative; a late runtime registration (e.g. the
            // OMO task component at SessionStart) adds to them instead of hiding them. Dedup by
            // handler identity: `ExtensionApi::on` re-publishes the full registered list, which can
            // already include the load-registered handlers, so a plain extend would double-dispatch.
            // Extensions with no handler for `kind` are skipped without allocating.
            let loaded = e.handlers.get(&kind).map_or(&[][..], Vec::as_slice);
            let live = self
                .runtime
                .live_handlers(&e.identity.path, kind)
                .unwrap_or_default();
            if loaded.is_empty() && live.is_empty() {
                continue;
            }
            let start = out.len();
            for handler in loaded {
                out.push((e.identity.path.clone(), handler.clone()));
            }
            for handler in live {
                let ptr = Arc::as_ptr(&handler) as *const ();
                if !out[start..]
                    .iter()
                    .any(|(_, seen): &(String, ExtensionHandler)| {
                        Arc::as_ptr(seen) as *const () == ptr
                    })
                {
                    out.push((e.identity.path.clone(), handler));
                }
            }
        }
        out
    }
    pub fn get_all_registered_tools(&self) -> Vec<RegisteredTool> {
        // PERF-DIVERGENCE(senpi runner.ts): upstream merges tools with linear name scans per tool; this port indexes the first position per name in a HashMap and moves tools instead of cloning them. Precedence and output order are identical.
        let mut tools: Vec<RegisteredTool> = Vec::new();
        let mut tool_index: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for ext in self.extensions.iter() {
            let mut merged = ext.tools.clone();
            if let Some(live) = self.runtime.live_tools(&ext.identity.path) {
                let mut merged_index: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                for (index, tool) in merged.iter().enumerate() {
                    merged_index
                        .entry(tool.definition.name.clone())
                        .or_insert(index);
                }
                for tool in live {
                    if let Some(&index) = merged_index.get(&tool.definition.name) {
                        merged[index] = tool;
                    } else {
                        merged_index.insert(tool.definition.name.clone(), merged.len());
                        merged.push(tool);
                    }
                }
            }
            for tool in merged {
                if let Some(&index) = tool_index.get(&tool.definition.name) {
                    let existing = &mut tools[index];
                    if existing.source_info.source == "builtin"
                        && tool.source_info.source != "builtin"
                    {
                        *existing = tool;
                    }
                } else {
                    tool_index.insert(tool.definition.name.clone(), tools.len());
                    tools.push(tool);
                }
            }
        }
        tools
    }
    pub fn get_all_tools(&self) -> Vec<ToolInfo> {
        self.get_all_registered_tools()
            .into_iter()
            .map(|t| normalize_tool_exposure(&t.definition, t.source_info))
            .collect()
    }
    pub fn get_tool_definition(&self, name: &str) -> Option<ToolDefinition> {
        self.extensions.iter().find_map(|extension| {
            // PERF-DIVERGENCE(senpi runner.ts): upstream scans each extension's tool list in place; this port searches the static list by reference and clones only the match instead of cloning the whole Vec. Result is identical.
            match self.runtime.live_tools(&extension.identity.path) {
                Some(live) => live
                    .into_iter()
                    .find(|tool| tool.definition.name == name)
                    .map(|tool| tool.definition),
                None => extension
                    .tools
                    .iter()
                    .find(|tool| tool.definition.name == name)
                    .map(|tool| tool.definition.clone()),
            }
        })
    }
    pub fn get_tool_definition_owned(&self, name: &str) -> Option<ToolDefinition> {
        self.get_all_registered_tools()
            .into_iter()
            .find(|tool| tool.definition.name == name)
            .map(|tool| tool.definition)
    }
    pub fn get_tool_renderers<TState: 'static, TArgs: 'static>(
        &self,
        name: &str,
    ) -> Option<Arc<ToolRenderers<TState, TArgs>>> {
        let selected = self.tool_renderer_owner(name)?;
        self.runtime
            .live_tool_renderer(&selected.identity.path, name)
            .unwrap_or_else(|| selected.tool_renderers.get(name).cloned())?
            .downcast::<ToolRenderers<TState, TArgs>>()
            .ok()
    }
    /// The object-safe erased renderer set for a tool name, over the same canonical per-name
    /// selection as [`Self::get_tool_renderers`].
    pub fn get_erased_tool_renderers(&self, name: &str) -> Option<Arc<dyn ErasedToolRenderers>> {
        let selected = self.tool_renderer_owner(name)?;
        self.runtime
            .live_erased_tool_renderer(&selected.identity.path, name)
            .unwrap_or_else(|| selected.erased_tool_renderers.get(name).cloned())
    }
    /// The canonical renderer owner for a tool name: the first extension whose live (else loaded)
    /// tool list has the name, replaced by a later non-builtin owner when the current one is builtin.
    /// Membership is tested without cloning any tool list.
    fn tool_renderer_owner(&self, name: &str) -> Option<&LoadedExtension> {
        let mut selected: Option<&LoadedExtension> = None;
        for extension in self.extensions.iter() {
            let has_tool = self
                .runtime
                .live_tools_contain(&extension.identity.path, name)
                .unwrap_or_else(|| {
                    extension
                        .tools
                        .iter()
                        .any(|tool| tool.definition.name == name)
                });
            if has_tool
                && selected.is_none_or(|current| {
                    current.source_info.source == "builtin"
                        && extension.source_info.source != "builtin"
                })
            {
                selected = Some(extension);
            }
        }
        selected
    }
    pub fn native_tool_renderers_snapshot<TState: 'static, TArgs: 'static>(
        &self,
    ) -> BTreeMap<String, Arc<ToolRenderers<TState, TArgs>>> {
        self.get_all_registered_tools()
            .into_iter()
            .filter_map(|tool| {
                self.get_tool_renderers(&tool.definition.name)
                    .map(|renderers| (tool.definition.name, renderers))
            })
            .collect()
    }
    /// The ONE coherent command snapshot for an extension path: the registrations the loader
    /// committed, unioned with anything a late registration published for the SAME path (the OMO
    /// task component registers its commands at SessionStart through a clone whose
    /// `LoadedExtension` starts empty, and `publish_commands` replaces the whole live list for a
    /// path). A load publishes no commands at all - `commit_registration` flushes only flags,
    /// providers and tool executors - so the loaded commands exist only in
    /// `LoadedExtension.commands` and reading either half alone hides the other. A late entry wins
    /// by name and drops the loaded context handler of that name, so a replaced command can never
    /// dispatch through a stale handler.
    fn live_command_snapshot(
        &self,
        extension: &LoadedExtension,
    ) -> (
        Vec<RegisteredCommand>,
        BTreeMap<String, CommandContextHandler>,
    ) {
        let mut commands = extension.commands.clone();
        let mut handlers = extension.command_context_handlers.clone();
        if let Some((live, live_handlers)) = self.runtime.live_commands(&extension.identity.path) {
            for command in live {
                let name = command.name.clone();
                match commands.iter_mut().find(|existing| existing.name == name) {
                    Some(existing) => {
                        *existing = command;
                        handlers.remove(&name);
                    }
                    None => commands.push(command),
                }
            }
            for (name, handler) in live_handlers {
                handlers.insert(name, handler);
            }
        }
        (commands, handlers)
    }

    /// The coherent argument-completion map for an extension path, derived the same way as the
    /// command snapshot: a late registration that replaces a command also replaces that command's
    /// completions (`register_command` clears them), so the loaded map drops every name the live
    /// list carries before the live map is merged in; otherwise a replaced command with no
    /// completions would keep serving the loaded ones.
    fn command_argument_completions_for(
        &self,
        extension: &LoadedExtension,
    ) -> BTreeMap<String, CommandArgumentCompletions> {
        let mut completions = extension.command_argument_completions.clone();
        if let Some((live, _)) = self.runtime.live_commands(&extension.identity.path) {
            for command in &live {
                completions.remove(&command.name);
            }
        }
        if let Some(live) = self
            .runtime
            .live_command_argument_completions(&extension.identity.path)
        {
            completions.extend(live);
        }
        completions
    }

    pub fn get_registered_commands(&self) -> Vec<ResolvedCommand> {
        let commands: Vec<_> = self
            .extensions
            .iter()
            .flat_map(|e| self.live_command_snapshot(e).0)
            .collect();
        let mut counts = BTreeMap::new();
        for command in &commands {
            *counts.entry(command.name.clone()).or_insert(0usize) += 1;
        }
        let mut seen = BTreeMap::new();
        let mut taken = BTreeSet::new();
        commands
            .into_iter()
            .map(|command| {
                let occurrence = seen.entry(command.name.clone()).or_insert(0usize);
                *occurrence += 1;
                let mut suffix = *occurrence;
                let mut name = if counts[&command.name] > 1 {
                    format!("{}:{suffix}", command.name)
                } else {
                    command.name.clone()
                };
                while taken.contains(&name) {
                    suffix += 1;
                    name = format!("{}:{suffix}", command.name);
                }
                taken.insert(name.clone());
                ResolvedCommand {
                    command: command.clone(),
                    invocation_name: name,
                }
            })
            .collect()
    }
    pub fn get_command(&self, name: &str) -> Option<ResolvedCommand> {
        self.get_registered_commands()
            .into_iter()
            .find(|c| c.invocation_name == name)
    }
    pub async fn get_command_argument_completions(
        &self,
        name: &str,
        prefix: &str,
    ) -> Result<Option<Vec<AutocompleteItem>>, ExtensionFailure> {
        self.runtime.assert_active()?;
        let resolved = self
            .get_command(name)
            .ok_or_else(|| ExtensionFailure::new(format!("Unknown extension command: {name}")))?;
        let completions = self
            .extensions
            .iter()
            .find_map(|extension| {
                let commands = self.live_command_snapshot(extension).0;
                commands
                    .iter()
                    .any(|command| Arc::ptr_eq(&command.handler, &resolved.command.handler))
                    .then(|| self.command_argument_completions_for(extension))
            })
            .and_then(|completions| completions.get(&resolved.command.name).cloned());
        let result = match completions {
            Some(completions) => completions(prefix).await?,
            None => None,
        };
        self.runtime.assert_active()?;
        Ok(result)
    }
    pub fn get_flags(&self) -> BTreeMap<String, ExtensionFlag> {
        let mut flags = BTreeMap::new();
        for ext in self.extensions.iter() {
            for flag in self
                .runtime
                .live_flags(&ext.identity.path)
                .unwrap_or_else(|| ext.flags.clone())
            {
                flags.entry(flag.name.clone()).or_insert(flag);
            }
        }
        flags
    }
    pub fn get_registered_mcp_servers(&self) -> Vec<RegisteredMcpServerDeclaration> {
        let mut servers: Vec<RegisteredMcpServerDeclaration> = Vec::new();
        for ext in self.extensions.iter() {
            for server in self
                .runtime
                .live_mcp_servers(&ext.identity.path)
                .unwrap_or_else(|| ext.mcp_servers.clone())
            {
                if !servers.iter().any(|s| s.name == server.name) {
                    servers.push(server);
                }
            }
        }
        servers
    }
    pub fn get_mcp_server_diagnostics(&self) -> Vec<String> {
        let mut owners: BTreeMap<String, &str> = BTreeMap::new();
        let mut warnings = Vec::new();
        for ext in self.extensions.iter() {
            for server in self
                .runtime
                .live_mcp_servers(&ext.identity.path)
                .unwrap_or_else(|| ext.mcp_servers.clone())
            {
                if let Some(owner) = owners.get(server.name.as_str()) {
                    warnings.push(format!("MCP server '{}' declared by both {} and {}; keeping first declaration from {}.", server.name, owner, ext.identity.path, owner));
                } else {
                    owners.insert(server.name, &ext.identity.path);
                }
            }
        }
        warnings
    }
    pub fn get_message_renderer(&self, custom_type: &str) -> Option<MessageRenderer> {
        self.get_message_renderer_owned(custom_type)
    }
    pub fn get_message_renderer_owned(&self, custom_type: &str) -> Option<MessageRenderer> {
        self.extensions.iter().find_map(|extension| {
            self.runtime
                .live_message_renderers(&extension.identity.path)
                .unwrap_or_else(|| extension.message_renderers.clone())
                .get(custom_type)
                .cloned()
        })
    }
    pub fn get_entry_renderer_owned(
        &self,
        custom_type: &str,
    ) -> Option<(EntryRenderer, Option<EntryRendererOptions>)> {
        self.extensions.iter().find_map(|extension| {
            match self.runtime.live_entry_renderers(&extension.identity.path) {
                Some(renderers) => renderers.get(custom_type).cloned(),
                None => extension
                    .entry_renderers
                    .get(custom_type)
                    .cloned()
                    .map(|renderer| {
                        (
                            renderer,
                            extension.entry_renderer_options.get(custom_type).cloned(),
                        )
                    }),
            }
        })
    }
    pub fn get_entry_renderer(&self, custom_type: &str) -> Option<EntryRenderer> {
        self.get_entry_renderer_owned(custom_type)
            .map(|(renderer, _)| renderer)
    }
    pub fn get_entry_renderer_options(&self, custom_type: &str) -> Option<EntryRendererOptions> {
        self.get_entry_renderer_owned(custom_type)
            .and_then(|(_, options)| options)
    }
    pub fn get_filesystem_policy_denied_roots(&self) -> Vec<std::path::PathBuf> {
        self.extensions
            .iter()
            .flat_map(|extension| {
                self.runtime
                    .live_filesystem_policies(&extension.identity.path)
                    .unwrap_or_else(|| extension.filesystem_policies.clone())
            })
            .flat_map(|policy| policy.denied_roots.unwrap_or_default())
            .collect()
    }

    pub async fn emit(
        &mut self,
        mut event: ExtensionEvent,
    ) -> Result<EventResult, ExtensionFailure> {
        self.runtime.assert_active()?;
        let kind = event.kind();
        let mut result = EventResult::None;
        if kind == EventKind::SessionShutdown
            && self.has_handlers(kind)
            && let Some(resolve) = &self.shutdown_budget
        {
            (self.shutdown_warn_ms, self.shutdown_timeout_ms) = resolve();
        }
        let quit_deadline = match &event {
            ExtensionEvent::SessionShutdown(shutdown) if shutdown.reason == SessionReason::Quit => {
                let requested = self
                    .process_exit_requested_at
                    .lock()
                    .ok()
                    .and_then(|requested| *requested);
                let total = self
                    .shutdown_quit_budget
                    .as_ref()
                    .map_or(2_000, |resolve| resolve());
                requested.filter(|_| total > 0).map(|requested| {
                    shutdown_quit_budget::QuitDeadline::new(
                        total,
                        std::time::Instant::now,
                        requested,
                    )
                })
            }
            _ => None,
        };
        let (base_warn_ms, base_timeout_ms) = (self.shutdown_warn_ms, self.shutdown_timeout_ms);
        for (path, handler) in self.handlers(kind) {
            let context = self.create_context_for_extension(Some(&path))?;
            let mut ends_at_quit_deadline = false;
            if let Some(deadline) = &quit_deadline {
                let quit = deadline.budget_for(shutdown_quit_budget::HandlerBudget {
                    warn_ms: base_warn_ms,
                    timeout_ms: base_timeout_ms,
                });
                (self.shutdown_warn_ms, self.shutdown_timeout_ms) =
                    (quit.budget.warn_ms, quit.budget.timeout_ms);
                ends_at_quit_deadline = quit.ends_at_quit_deadline;
            }
            let handled = if kind == EventKind::SessionShutdown {
                self.run_shutdown(&path, &mut event, &context, &handler, ends_at_quit_deadline)
                    .await
            } else {
                handler(&mut event, &context).await
            };
            self.runtime.assert_active()?;
            match handled {
                Ok(next) => {
                    if kind.is_session_before()
                        && let EventResult::SessionBefore(before) = &next
                    {
                        let cancel = before.cancel == Some(true);
                        result = next;
                        if cancel {
                            return Ok(result);
                        }
                    }
                }
                Err(error) => self.report(&path, kind, error),
            }
        }
        if quit_deadline.is_some() {
            (self.shutdown_warn_ms, self.shutdown_timeout_ms) = (base_warn_ms, base_timeout_ms);
        }
        Ok(result)
    }
    async fn run_shutdown(
        &mut self,
        path: &str,
        event: &mut ExtensionEvent,
        context: &ExtensionContext,
        handler: &ExtensionHandler,
        ends_at_quit_deadline: bool,
    ) -> Result<EventResult, ExtensionFailure> {
        let started_at = std::time::Instant::now();
        let signal = AbortSignal::default();
        match event {
            ExtensionEvent::SessionShutdown(shutdown) => shutdown.signal = Some(signal.clone()),
            ExtensionEvent::SessionMoveExport {
                signal: current, ..
            }
            | ExtensionEvent::SessionMoveImport {
                signal: current, ..
            } => *current = signal.clone(),
            _ => {}
        }
        let future = handler(event, context);
        tokio::pin!(future);
        let warn = tokio::time::sleep(Duration::from_millis(self.shutdown_warn_ms));
        tokio::pin!(warn);
        let cap = tokio::time::sleep(Duration::from_millis(self.shutdown_timeout_ms));
        tokio::pin!(cap);
        let mut warned = false;
        loop {
            tokio::select! {
                result = &mut future => return result,
                () = &mut warn, if self.shutdown_warn_ms > 0 && !warned => {
                    warned = true;
                    let cap_note = if self.shutdown_timeout_ms > 0 { format!(" (hard cap {}ms)", self.shutdown_timeout_ms) } else { String::new() };
                    let message = format!("Extension {path} is still running its session_shutdown handler after {}ms{cap_note}.", self.shutdown_warn_ms);
                    if let Some(listener) = &self.warning_listener { listener(&message); }
                    self.warnings.push(message);
                }
                () = &mut cap, if self.shutdown_timeout_ms > 0 => {
                    signal.abort();
                    if ends_at_quit_deadline {
                        let message = format!("Extension {path} session_shutdown handler overran the quit budget after {}ms; quitting without it.", started_at.elapsed().as_millis());
                        if let Some(listener) = &self.warning_listener { listener(&message); }
                        self.warnings.push(message);
                        return Ok(EventResult::None);
                    }
                    return Err(ExtensionFailure::new(format!("handler timed out after {}ms", self.shutdown_timeout_ms)));
                }
            }
        }
    }
    pub async fn emit_session_move(
        &mut self,
        mut event: ExtensionEvent,
    ) -> Result<Vec<ExtensionError>, ExtensionFailure> {
        self.runtime.assert_active()?;
        let kind = event.kind();
        if !matches!(
            kind,
            EventKind::SessionMoveExport | EventKind::SessionMoveImport
        ) {
            return Err(ExtensionFailure::new("Expected a session move event"));
        }
        if let Some(budget) = &self.shutdown_budget {
            (self.shutdown_warn_ms, self.shutdown_timeout_ms) = budget();
        }
        let mut failures = Vec::new();
        for (path, handler) in self.handlers(kind) {
            let context = self.create_context_for_extension(Some(&path))?;
            if let Err(error) = self
                .run_shutdown(&path, &mut event, &context, &handler, false)
                .await
            {
                failures.push(ExtensionError {
                    extension_path: path.clone(),
                    event: kind.as_str().into(),
                    error: error.message.clone(),
                    stack: error.stack.clone(),
                });
                self.report(&path, kind, error);
            }
        }
        Ok(failures)
    }

    pub async fn emit_before_agent_start(
        &mut self,
        event: BeforeAgentStartEvent,
    ) -> Result<Option<BeforeAgentStartCombinedResult>, ExtensionFailure> {
        self.emit_before_agent_start_observed(event, None).await
    }

    pub async fn emit_before_agent_start_observed(
        &mut self,
        event: BeforeAgentStartEvent,
        observe: Option<&PromptStartObserver<'_>>,
    ) -> Result<Option<BeforeAgentStartCombinedResult>, ExtensionFailure> {
        self.runtime.assert_active()?;
        let mut event = ExtensionEvent::BeforeAgentStart(event);
        let mut combined = BeforeAgentStartCombinedResult::default();
        for (path, handler) in self.handlers(EventKind::BeforeAgentStart) {
            let mut context = self.create_context_for_extension(Some(&path))?;
            if let ExtensionEvent::BeforeAgentStart(current) = &event {
                let prompt = current.system_prompt.clone();
                context.get_system_prompt_fn = Arc::new(move || prompt.clone());
            }
            let before = match &event {
                ExtensionEvent::BeforeAgentStart(current) => current.system_prompt.clone(),
                _ => String::new(),
            };
            let mut parts = None;
            match handler(&mut event, &context).await {
                Ok(EventResult::BeforeAgentStart(next)) => {
                    parts = next.system_prompt_parts;
                    if let Some(message) = next.message {
                        combined.messages.push(message);
                    }
                    if let Some(prompt) = next.system_prompt {
                        if let ExtensionEvent::BeforeAgentStart(current) = &mut event {
                            current.system_prompt = prompt.clone();
                        }
                        combined.system_prompt = Some(prompt);
                        combined.system_prompt_parts = parts.clone();
                    }
                }
                Ok(_) => {}
                Err(error) => self.report(&path, EventKind::BeforeAgentStart, error),
            }
            if let ExtensionEvent::BeforeAgentStart(current) = &event
                && current.preview
                && let Some(observe) = observe
            {
                observe(&path, &before, &current.system_prompt, parts.as_deref());
            }
            self.runtime.assert_active()?;
        }
        Ok(
            if combined.messages.is_empty() && combined.system_prompt.is_none() {
                None
            } else {
                Some(combined)
            },
        )
    }
    pub async fn emit_input(
        &mut self,
        input: InputEvent,
    ) -> Result<InputEventResult, ExtensionFailure> {
        self.runtime.assert_active()?;
        let original_text = input.text.clone();
        let original_images = input.images.clone();
        let mut event = ExtensionEvent::Input(input);
        for (path, handler) in self.handlers(EventKind::Input) {
            let context = self.create_context_for_extension(Some(&path))?;
            let result = handler(&mut event, &context).await;
            self.runtime.assert_active()?;
            match result {
                Ok(EventResult::Input(InputEventResult::Handled)) => {
                    return Ok(InputEventResult::Handled);
                }
                Ok(EventResult::Input(InputEventResult::Transform { text, images })) => {
                    if let ExtensionEvent::Input(current) = &mut event {
                        current.text = text;
                        if images.is_some() {
                            current.images = images;
                        }
                    }
                }
                Ok(_) => {}
                Err(error) => self.report(&path, EventKind::Input, error),
            }
        }
        if let ExtensionEvent::Input(current) = event {
            return Ok(
                if current.text != original_text || current.images != original_images {
                    InputEventResult::Transform {
                        text: current.text,
                        images: current.images,
                    }
                } else {
                    InputEventResult::Continue
                },
            );
        }
        Err(ExtensionFailure::new("input handler replaced event kind"))
    }
    pub async fn emit_model_select(
        &mut self,
        event: ModelSelectEvent,
    ) -> Result<Option<ModelSelectEventResult>, ExtensionFailure> {
        self.runtime.assert_active()?;
        let mut event = ExtensionEvent::ModelSelect(event);
        let mut combined: Option<ModelSelectEventResult> = None;
        for (path, handler) in self.handlers(EventKind::ModelSelect) {
            let context = self.create_context_for_extension(Some(&path))?;
            if let ExtensionEvent::ModelSelect(current) = &mut event {
                current.system_prompt_options = context.get_system_prompt_options();
            }
            match handler(&mut event, &context).await {
                Ok(EventResult::ModelSelect(next))
                    if next.system_prompt.is_some() || next.system_prompt_name.is_some() =>
                {
                    let result = combined.get_or_insert_with(Default::default);
                    if next.system_prompt.is_some() {
                        result.system_prompt = next.system_prompt;
                    }
                    if next.system_prompt_name.is_some() {
                        result.system_prompt_name = next.system_prompt_name;
                    }
                }
                Ok(_) => {}
                Err(error) => self.report(&path, EventKind::ModelSelect, error),
            }
            self.runtime.assert_active()?;
        }
        Ok(combined)
    }
    pub async fn emit_message_end(
        &mut self,
        message: AgentMessage,
    ) -> Result<Option<AgentMessage>, ExtensionFailure> {
        self.runtime.assert_active()?;
        let mut current_message = message;
        let mut modified = false;
        for (path, handler) in self.handlers(EventKind::MessageEnd) {
            let context = self.create_context_for_extension(Some(&path))?;
            let mut event = ExtensionEvent::MessageEnd {
                message: current_message.clone(),
            };
            match handler(&mut event, &context).await {
                Ok(EventResult::MessageEnd {
                    message: Some(next),
                }) => {
                    if next.role() != current_message.role() {
                        self.report(
                            &path,
                            EventKind::MessageEnd,
                            ExtensionFailure::new(
                                "message_end handlers must return a message with the same role",
                            ),
                        );
                    } else {
                        current_message = next;
                        modified = true;
                    }
                }
                Ok(_) => {}
                Err(error) => self.report(&path, EventKind::MessageEnd, error),
            }
            self.runtime.assert_active()?;
        }
        Ok(modified.then_some(current_message))
    }
    /// `entry_ids` stamps each message with its session entry (senpi `emitContext` copying
    /// `getSessionContextEntryId` onto the cloned messages); missing positions are unstamped.
    pub async fn emit_context(
        &mut self,
        messages: &[AgentMessage],
        entry_ids: &[Option<String>],
        exclude_path: Option<&str>,
    ) -> Result<Vec<AgentMessage>, ExtensionFailure> {
        self.emit_context_owned(messages.to_vec(), entry_ids, exclude_path)
            .await
            .map_err(|failure| failure.error)
    }
    pub async fn emit_context_owned(
        &mut self,
        messages: Vec<AgentMessage>,
        entry_ids: &[Option<String>],
        exclude_path: Option<&str>,
    ) -> Result<Vec<AgentMessage>, OwnedContextDispatchError> {
        self.emit_context_owned_from(messages, entry_ids, exclude_path, ContextEventSource::Agent)
            .await
    }
    pub async fn emit_context_owned_from(
        &mut self,
        messages: Vec<AgentMessage>,
        entry_ids: &[Option<String>],
        exclude_path: Option<&str>,
        source: ContextEventSource,
    ) -> Result<Vec<AgentMessage>, OwnedContextDispatchError> {
        if let Err(error) = self.runtime.assert_active() {
            return Err(OwnedContextDispatchError { messages, error });
        }
        let raw_handlers = self.raw_handlers(EventKind::Context);
        let handlers: Vec<_> = raw_handlers
            .iter()
            .map(|(path, handler)| {
                (
                    path.clone(),
                    with_provider_exclude_path(path.clone(), handler.clone()),
                )
            })
            .collect();
        if handlers.is_empty() {
            return Ok(messages);
        }
        let read_only = raw_handlers.iter().all(|(path, handler)| {
            if exclude_path == Some(path.as_str()) {
                return true;
            }
            let Some(extension) = self
                .extensions
                .iter()
                .find(|extension| extension.identity.path == *path)
            else {
                return false;
            };
            let loaded = extension
                .handlers
                .get(&EventKind::Context)
                .map_or(&[][..], Vec::as_slice);
            let loaded_options = extension
                .handler_options
                .get(&EventKind::Context)
                .map_or(&[][..], Vec::as_slice);
            if let Some(index) = loaded
                .iter()
                .position(|candidate| Arc::ptr_eq(candidate, handler))
            {
                return loaded_options
                    .get(index)
                    .and_then(|options| options.mutates_messages)
                    == Some(false);
            }
            let Some((live, live_options)) = self
                .runtime
                .live_handlers_with_options(path, EventKind::Context)
            else {
                return false;
            };
            live.iter()
                .position(|candidate| Arc::ptr_eq(candidate, handler))
                .and_then(|index| live_options.get(index))
                .and_then(|options| options.mutates_messages)
                == Some(false)
        });
        let mut fallback = (!read_only).then(|| messages.clone());
        let entry_ids = (0..messages.len())
            .map(|index| entry_ids.get(index).cloned().flatten())
            .collect();
        let mut event = ExtensionEvent::Context {
            messages,
            entry_ids,
            source,
        };
        for (path, handler) in handlers {
            if exclude_path == Some(path.as_str()) {
                continue;
            }
            let context = match self.create_context_for_extension(Some(&path)) {
                Ok(context) => context,
                Err(error) => {
                    let current = match event {
                        ExtensionEvent::Context { messages, .. } => messages,
                        _ => Vec::new(),
                    };
                    return Err(OwnedContextDispatchError {
                        messages: fallback.take().unwrap_or(current),
                        error,
                    });
                }
            };
            match handler(&mut event, &context).await {
                Ok(EventResult::Context {
                    messages: Some(next),
                }) => {
                    if let ExtensionEvent::Context {
                        messages,
                        entry_ids,
                        ..
                    } = &mut event
                    {
                        *entry_ids = next
                            .iter()
                            .enumerate()
                            .map(|(index, message)| {
                                entry_ids
                                    .get(index)
                                    .cloned()
                                    .flatten()
                                    .filter(|_| messages.get(index) == Some(message))
                            })
                            .collect();
                        *messages = next;
                    }
                }
                Ok(_) => {}
                Err(error) => self.report(&path, EventKind::Context, error),
            }
            if let Err(error) = self.runtime.assert_active() {
                let current = match event {
                    ExtensionEvent::Context { messages, .. } => messages,
                    _ => Vec::new(),
                };
                return Err(OwnedContextDispatchError {
                    messages: fallback.take().unwrap_or(current),
                    error,
                });
            }
        }
        if let ExtensionEvent::Context { messages, .. } = event {
            return Ok(messages);
        }
        let current = match event {
            ExtensionEvent::Context { messages, .. } => messages,
            _ => Vec::new(),
        };
        Err(OwnedContextDispatchError {
            messages: fallback.unwrap_or(current),
            error: ExtensionFailure::new("context handler replaced event kind"),
        })
    }
    pub async fn emit_before_provider_request(
        &mut self,
        payload: JsonValue,
        exclude_path: Option<&str>,
    ) -> Result<JsonValue, ExtensionFailure> {
        self.emit_before_provider_request_with_metadata(payload, None, None, exclude_path)
            .await
    }
    pub async fn emit_before_provider_request_with_metadata(
        &mut self,
        payload: JsonValue,
        model: Option<Model>,
        headers: Option<BTreeMap<String, Option<String>>>,
        exclude_path: Option<&str>,
    ) -> Result<JsonValue, ExtensionFailure> {
        self.runtime.assert_active()?;
        let mut event = ExtensionEvent::BeforeProviderRequest {
            payload,
            model,
            headers,
        };
        for (path, handler) in self.handlers(EventKind::BeforeProviderRequest) {
            if exclude_path == Some(path.as_str()) {
                continue;
            }
            let context = self.create_context_for_extension(Some(&path))?;
            match handler(&mut event, &context).await {
                Ok(EventResult::ProviderPayload(next)) => {
                    if let ExtensionEvent::BeforeProviderRequest { payload, .. } = &mut event {
                        *payload = next;
                    }
                }
                Ok(_) => {}
                Err(error) => self.report(&path, EventKind::BeforeProviderRequest, error),
            }
            self.runtime.assert_active()?;
        }
        if let ExtensionEvent::BeforeProviderRequest { mut payload, .. } = event {
            if let Some(object) = payload.as_object_mut() {
                for key in [
                    "server_search_fallback",
                    "__senpi_server_search_fallback",
                    "server_search_disabled",
                    "__senpi_server_search_disabled",
                ] {
                    object.remove(key);
                }
            }
            return Ok(payload);
        }
        Err(ExtensionFailure::new(
            "provider handler replaced event kind",
        ))
    }
    pub async fn emit_before_provider_headers(
        &mut self,
        headers: BTreeMap<String, Option<String>>,
    ) -> Result<BTreeMap<String, Option<String>>, ExtensionFailure> {
        self.runtime.assert_active()?;
        let mut event = ExtensionEvent::BeforeProviderHeaders { headers };
        for (path, handler) in self.handlers(EventKind::BeforeProviderHeaders) {
            let context = self.create_context_for_extension(Some(&path))?;
            if let Err(error) = handler(&mut event, &context).await {
                self.report(&path, EventKind::BeforeProviderHeaders, error);
            }
            self.runtime.assert_active()?;
        }
        if let ExtensionEvent::BeforeProviderHeaders { headers } = event {
            return Ok(headers);
        }
        Err(ExtensionFailure::new("header handler replaced event kind"))
    }
    pub async fn emit_project_trust(
        &mut self,
        cwd: std::path::PathBuf,
    ) -> Result<Option<ProjectTrustEventResult>, ExtensionFailure> {
        self.runtime.assert_active()?;
        let mut event = ExtensionEvent::ProjectTrust { cwd };
        for (path, handler) in self.handlers(EventKind::ProjectTrust) {
            let context = self.create_context_for_extension(Some(&path))?;
            let result = handler(&mut event, &context).await;
            self.runtime.assert_active()?;
            match result {
                Ok(EventResult::ProjectTrust(result))
                    if result.trusted != TrustDecision::Undecided =>
                {
                    return Ok(Some(result));
                }
                Ok(_) => {}
                Err(error) => self.report(&path, EventKind::ProjectTrust, error),
            }
        }
        Ok(None)
    }
    pub async fn emit_provider_stream_event(
        &mut self,
        provider: String,
        api: String,
        model: String,
        data: JsonValue,
    ) -> Result<(), ExtensionFailure> {
        self.emit(ExtensionEvent::ProviderStreamEvent {
            provider,
            api,
            model,
            data,
        })
        .await?;
        Ok(())
    }
    pub async fn emit_session_control_wake(
        &mut self,
        source: String,
        reason: String,
    ) -> Result<(), ExtensionFailure> {
        self.emit(ExtensionEvent::SessionControlWake { source, reason })
            .await?;
        Ok(())
    }
    pub async fn emit_tool_activated(
        &mut self,
        tool_names: Vec<String>,
    ) -> Result<(), ExtensionFailure> {
        self.emit(ExtensionEvent::ToolActivated { tool_names })
            .await?;
        Ok(())
    }
    pub async fn emit_resources_discover(
        &mut self,
        cwd: std::path::PathBuf,
        reason: SessionReason,
    ) -> Result<DiscoveredResources, ExtensionFailure> {
        self.runtime.assert_active()?;
        let mut combined = DiscoveredResources::default();
        for (path, handler) in self.handlers(EventKind::ResourcesDiscover) {
            let mut event = ExtensionEvent::ResourcesDiscover(ResourcesDiscoverEvent {
                cwd: cwd.clone(),
                reason,
                scoped_entries: true,
            });
            let context = self.create_context_for_extension(Some(&path))?;
            match handler(&mut event, &context).await {
                Ok(EventResult::ResourcesDiscover(next)) => {
                    let convert = |entries: Vec<ResourceDiscoverEntry>| {
                        entries
                            .into_iter()
                            .map(|e| DiscoveredResourceEntry {
                                path: e.path,
                                scope: e.scope,
                                extension_path: path.clone(),
                            })
                            .collect::<Vec<_>>()
                    };
                    combined.skill_paths.extend(convert(next.skill_paths));
                    combined.prompt_paths.extend(convert(next.prompt_paths));
                    combined.theme_paths.extend(convert(next.theme_paths));
                    combined.hook_paths.extend(convert(next.hook_paths));
                }
                Ok(_) => {}
                Err(error) => self.report(&path, EventKind::ResourcesDiscover, error),
            }
            self.runtime.assert_active()?;
        }
        Ok(combined)
    }
    pub async fn emit_user_bash(
        &mut self,
        command: String,
        exclude_from_context: bool,
        cwd: std::path::PathBuf,
    ) -> Result<EventResult, ExtensionFailure> {
        self.runtime.assert_active()?;
        let mut event = ExtensionEvent::UserBash {
            command,
            exclude_from_context,
            cwd,
        };
        for (path, handler) in self.handlers(EventKind::UserBash) {
            let context = self.create_context_for_extension(Some(&path))?;
            let result = handler(&mut event, &context).await;
            self.runtime.assert_active()?;
            match result {
                Ok(EventResult::None) => {}
                Ok(result) => return Ok(result),
                Err(error) => self.report(&path, EventKind::UserBash, error),
            }
        }
        Ok(EventResult::None)
    }
    fn begin_hook(
        &mut self,
        path: &str,
        tool_name: &str,
        tool_call_id: &str,
        hook_name: ToolHookName,
        context: &mut ExtensionContext,
    ) -> HookRun {
        let started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let name = match hook_name {
            ToolHookName::PreToolUse => "PreToolUse",
            ToolHookName::PostToolUse => "PostToolUse",
        };
        let base = ToolHookLifecycleEvent {
            hook_run_id: format!("{tool_call_id}:{name}:{}", self.next_hook_index),
            hook_name,
            tool_name: tool_name.into(),
            tool_call_id: tool_call_id.into(),
            extension_path: path.into(),
            status_message: hook_status_message(path, hook_name),
            started_at,
            phase: ToolHookPhase::Start,
        };
        self.next_hook_index = self.next_hook_index.wrapping_add(1);
        if let Some(observer) = &self.hook_observer {
            observer(&base);
        }
        let updates = Arc::new(std::sync::Mutex::new((false, base.status_message.clone())));
        let state = Arc::clone(&updates);
        let observer = self.hook_observer.clone();
        let mut update_event = base.clone();
        update_event.phase = ToolHookPhase::Update;
        context.update_tool_hook_status = Some(Arc::new(move |message| {
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.0 {
                return;
            }
            state.1 = sanitize_status(message);
            let mut event = update_event.clone();
            event.status_message = state.1.clone();
            if let Some(observer) = &observer {
                observer(&event);
            }
        }));
        HookRun {
            event: base,
            state: updates,
        }
    }
    fn end_hook(&self, hook: HookRun, status: ToolHookStatus, error_message: Option<String>) {
        let mut event = hook.event;
        let mut state = hook
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.0 = true;
        event.status_message = state.1.clone();
        drop(state);
        event.phase = ToolHookPhase::End {
            completed_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
            status,
            error_message,
        };
        if let Some(observer) = &self.hook_observer {
            observer(&event);
        }
    }
    pub async fn emit_tool_call(
        &mut self,
        event: &mut ToolCallEvent,
    ) -> Result<Option<ToolCallEventResult>, ExtensionFailure> {
        self.runtime.assert_active()?;
        let mut current = ExtensionEvent::ToolCall(event.clone());
        let mut combined = None;
        for (path, handler) in self.handlers(EventKind::ToolCall) {
            let mut context = self.create_context_for_extension(Some(&path))?;
            let hook = self.begin_hook(
                &path,
                &event.tool_name,
                &event.tool_call_id,
                ToolHookName::PreToolUse,
                &mut context,
            );
            let result = handler(&mut current, &context).await;
            let (status, error) = match &result {
                Err(e) => (ToolHookStatus::Failed, Some(e.message.clone())),
                Ok(EventResult::ToolCall(r)) if r.block == Some(true) => {
                    (ToolHookStatus::Blocked, None)
                }
                Ok(_) => (ToolHookStatus::Completed, None),
            };
            self.end_hook(hook, status, error);
            self.runtime.assert_active()?;
            if let ExtensionEvent::ToolCall(updated) = &current {
                *event = updated.clone();
            }
            if let EventResult::ToolCall(next) = result? {
                let blocked = next.block == Some(true);
                combined = Some(next);
                if blocked {
                    return Ok(combined);
                }
            }
        }
        Ok(combined)
    }
    pub async fn emit_tool_result(
        &mut self,
        event: ToolResultEvent,
    ) -> Result<Option<ToolResultEventResult>, ExtensionFailure> {
        self.runtime.assert_active()?;
        let tool_name = event.tool_name.clone();
        let tool_call_id = event.tool_call_id.clone();
        let mut current = ExtensionEvent::ToolResult(event);
        let mut modified = false;
        for (path, handler) in self.handlers(EventKind::ToolResult) {
            let mut context = self.create_context_for_extension(Some(&path))?;
            let hook = self.begin_hook(
                &path,
                &tool_name,
                &tool_call_id,
                ToolHookName::PostToolUse,
                &mut context,
            );
            let result = handler(&mut current, &context).await;
            self.end_hook(
                hook,
                if result.is_err() {
                    ToolHookStatus::Failed
                } else {
                    ToolHookStatus::Completed
                },
                result.as_ref().err().map(|e| e.message.clone()),
            );
            self.runtime.assert_active()?;
            match result {
                Ok(EventResult::ToolResult(next)) => {
                    if let ExtensionEvent::ToolResult(event) = &mut current {
                        if let Some(content) = next.content {
                            event.content = content;
                            modified = true;
                        }
                        if let Some(details) = next.details {
                            event.details = Some(details);
                            modified = true;
                        }
                        if let Some(is_error) = next.is_error {
                            event.is_error = is_error;
                            modified = true;
                        }
                        if let Some(usage) = next.usage {
                            event.usage = Some(usage);
                            modified = true;
                        }
                    }
                }
                Ok(_) => {}
                Err(error) => self.report(&path, EventKind::ToolResult, error),
            }
        }
        if let ExtensionEvent::ToolResult(event) = current {
            return Ok(modified.then_some(ToolResultEventResult {
                content: Some(event.content),
                details: event.details,
                is_error: Some(event.is_error),
                usage: event.usage,
            }));
        }
        Err(ExtensionFailure::new(
            "tool_result handler replaced event kind",
        ))
    }
}

pub fn format_extension_error_headline(error: &ExtensionError) -> String {
    let path = sanitize_terminal(&error.extension_path);
    let event = sanitize_terminal(&error.event);
    let body = sanitize_terminal(&error.error);
    if path == RUNTIME_EXTENSION_PATH {
        if event.is_empty() {
            format!("Runtime error: {body}")
        } else {
            format!("Runtime error ({event}): {body}")
        }
    } else {
        format!("Extension \"{path}\" error: {body}")
    }
}
fn sanitize_terminal(text: &str) -> String {
    let mut output = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']' | 'P' | '_' | '^' | 'X') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || c == '\u{9c}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) | None => {}
            }
        } else if ch == '\u{9b}' {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else if ch == '\r' {
            output.push('\n');
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
        } else if ch == ' ' || ch == '\t' {
            if !output.ends_with(' ') {
                output.push(' ');
            }
        } else if !ch.is_control() || ch == '\n' {
            output.push(ch);
        }
    }
    output
}
fn bounded_status(message: &str) -> String {
    if message.chars().count() > 79 {
        format!("{}...", message.chars().take(76).collect::<String>())
    } else {
        message.into()
    }
}
fn sanitize_status(message: &str) -> String {
    bounded_status(
        &sanitize_terminal(message)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    )
}
fn hook_status_message(path: &str, hook_name: ToolHookName) -> String {
    match path {
        "<builtin:permission-system>" => "matching project rules".into(),
        "<builtin:bash-timeout>" => "applying bash timeout".into(),
        "<builtin:compaction>" => match hook_name {
            ToolHookName::PreToolUse => "checking compaction state".into(),
            ToolHookName::PostToolUse => "checking tool result size".into(),
        },
        "<builtin:tool-pair-guard>" => "checking tool/result pairs".into(),
        _ => {
            let name = path
                .strip_prefix("<builtin:")
                .and_then(|s| s.strip_suffix('>'))
                .or_else(|| path.strip_prefix('<').and_then(|s| s.strip_suffix('>')))
                .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path));
            let name = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
            bounded_status(&format!("running {name}"))
        }
    }
}
