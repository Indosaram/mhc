//! P8 extensions to `AgentSession` (kept in a separate module so `agent_session.rs`
//! stays clean and does not grow).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use maho_ai::types::{AssistantMessage, ImageContent, Model};
use serde_json::Value;

use crate::agent_session::{
    AgentSession, AgentSessionInner, PromptDisposition, PromptOptions, QueuedInputOptions, lock,
};
use crate::compaction_failure_text::{CompactionFailureKind, format_compaction_failure};
use crate::dynamic_prompt::FullToolPrompt;
use crate::engine_turn_hold::{HoldKind, HoldReleaseCause};
use crate::host_restart_continuation::{
    HOST_DRAIN_HOLD_KEY, RestartRelease, restart_hold_data, restart_release,
};
use crate::lifecycle_event_sequencer::LifecycleEventSequencer;
use crate::retry_wait::{RetryWait, RetryWaitEvent, next_retry_wait};
use crate::session_closed::{CLOSING_HOLD_KEY, SESSION_CLOSED_ENTRY, SessionClosedRecord};
use crate::turn_conflict::{TurnConflictError, turn_conflict};

pub(crate) struct AgentSessionP8State {
    pub(crate) lifecycle_events: Mutex<LifecycleEventSequencer<Value>>,
    pub(crate) retry_wait: Mutex<Option<RetryWait>>,
    pub(crate) closed_on_request: AtomicBool,
    pub(crate) continued_from_host_restart: AtomicBool,
    pub(crate) restart_continued: AtomicBool,
    pub(crate) session_env: Mutex<Option<BTreeMap<String, String>>>,
    pub(crate) tool_call_issuers: Mutex<HashMap<String, AssistantMessage>>,
    pub(crate) exec_bridge_settled: Mutex<BTreeSet<String>>,
    pub(crate) full_tool_prompt_guidelines: Mutex<HashMap<String, Vec<String>>>,
}

impl Default for AgentSessionP8State {
    fn default() -> Self {
        Self {
            lifecycle_events: Mutex::new(LifecycleEventSequencer::new()),
            retry_wait: Mutex::new(None),
            closed_on_request: AtomicBool::new(false),
            continued_from_host_restart: AtomicBool::new(false),
            restart_continued: AtomicBool::new(false),
            session_env: Mutex::new(None),
            tool_call_issuers: Mutex::new(HashMap::new()),
            exec_bridge_settled: Mutex::new(BTreeSet::new()),
            full_tool_prompt_guidelines: Mutex::new(HashMap::new()),
        }
    }
}

fn root_tool_call_id(tool_call_id: &str) -> &str {
    tool_call_id
        .split_once('/')
        .map_or(tool_call_id, |(root, _)| root)
}

impl AgentSession {
    /// Current retry wait tracked from session auto-retry events (senpi#7025).
    pub fn retry_wait(&self) -> Option<RetryWait> {
        lock(&self.inner.p8.retry_wait).clone()
    }

    pub(crate) fn observe_p8_event(&self, event: &maho_ext_api::AgentSessionEvent) {
        use maho_ext_api::AgentSessionEvent;
        let mapped = match event {
            AgentSessionEvent::AutoRetryStart {
                attempt,
                max_attempts,
                delay_ms,
                error_message,
            } => RetryWaitEvent::AutoRetryStart {
                attempt: u64::from(*attempt),
                max_attempts: u64::from(*max_attempts),
                delay_ms: *delay_ms,
                error_message,
            },
            AgentSessionEvent::AutoRetryEnd { .. } => RetryWaitEvent::AutoRetryEnd,
            AgentSessionEvent::Agent(maho_agent::types::AgentEvent::AgentStart) => {
                RetryWaitEvent::AgentStart
            }
            _ => RetryWaitEvent::Other,
        };
        let now_ms = maho_ai::utils::diagnostics::now_ms();
        let mut guard = lock(&self.inner.p8.retry_wait);
        let prev = guard.take();
        *guard = next_retry_wait(prev, mapped, now_ms);
    }

    /// Sequences a `session_lifecycle` event on the session event bus (senpi#5682).
    pub(crate) fn emit_sequenced_lifecycle(&self, payload: Value) {
        let bus = &self.inner.event_bus;
        let mut seq = lock(&self.inner.p8.lifecycle_events);
        seq.emit(
            payload,
            &mut |event| bus.emit("session_lifecycle", &event),
            &mut |run| {
                let mut seq = LifecycleEventSequencer::new();
                run(&mut seq, &mut |event| bus.emit("session_lifecycle", &event));
            },
        );
    }

    /// Set by `record_closed_on_request`: the teardown's `session_shutdown` then carries `closed: true`.
    pub fn closed_on_request(&self) -> bool {
        self.inner.p8.closed_on_request.load(Ordering::SeqCst)
    }

    /// A graceful close ended this session on request (oh-my-openagent#14313): records `session-closed`.
    pub fn record_closed_on_request(&self, record: SessionClosedRecord) {
        self.inner
            .p8
            .closed_on_request
            .store(true, Ordering::SeqCst);
        let data = serde_json::to_value(record).unwrap_or(Value::Null);
        let entry =
            lock(&self.inner.session_manager).append_custom(SESSION_CLOSED_ENTRY, Some(data));
        if let Some(id) = entry.get("id").and_then(Value::as_str) {
            self.emit_entry_appended(id);
        }
    }

    /// A superseded host generation draining an unattached session (senpi#3114, #7020).
    pub fn hold_automatic_turns(&self) {
        lock(&self.inner.engine_turn_hold).add_hold(
            HoldKind::Unattended,
            Some(HOST_DRAIN_HOLD_KEY),
            None,
        );
    }

    /// A graceful close drains the running turn (oh-my-openagent#14313).
    pub fn hold_for_close(&self) {
        lock(&self.inner.engine_turn_hold).add_hold(
            HoldKind::Unattended,
            Some(CLOSING_HOLD_KEY),
            None,
        );
    }

    pub fn release_close_hold(&self) {
        let _ = lock(&self.inner.engine_turn_hold).release(
            &[CLOSING_HOLD_KEY.to_owned()],
            HoldReleaseCause::SessionClose,
        );
    }

    pub fn set_continued_from_host_restart(&self, continued: bool) {
        self.inner
            .p8
            .continued_from_host_restart
            .store(continued, Ordering::SeqCst);
    }

    pub fn continued_from_host_restart(&self) -> bool {
        self.inner
            .p8
            .continued_from_host_restart
            .load(Ordering::SeqCst)
    }

    /// A session reopened after host exit continues as left (senpi#7020, #7079).
    pub async fn continue_after_host_restart(&self) -> Result<(), String> {
        if !self.continued_from_host_restart()
            || self.inner.p8.restart_continued.swap(true, Ordering::SeqCst)
        {
            return Ok(());
        }
        let holds = lock(&self.inner.engine_turn_hold).holds();
        let release = restart_release(&holds, || {
            self.with_session_manager(|sm| sm.current_branch())
        });
        let Some(release) = release else {
            return Ok(());
        };
        match release {
            RestartRelease::Release { keys } => {
                let _ = lock(&self.inner.engine_turn_hold)
                    .release(&keys, HoldReleaseCause::ResumeSession);
                Ok(())
            }
            RestartRelease::Resume { interrupted_run } => {
                if interrupted_run {
                    self.mark_restart_continuation();
                }
                self.resume_session().await
            }
        }
    }

    fn mark_restart_continuation(&self) {
        let holds = lock(&self.inner.engine_turn_hold).holds();
        if let Some(data) = restart_hold_data(self.continued_from_host_restart(), || {
            self.with_session_manager(|sm| sm.current_branch())
        }) && !holds.iter().any(|hold| hold.key == "interrupted-run")
        {
            lock(&self.inner.engine_turn_hold).add_hold(
                HoldKind::SessionHold,
                Some("interrupted-run"),
                Some(data),
            );
        }
    }

    /// Per-session caller env for tool subprocesses (`open_session.env`, senpi#4381).
    pub fn session_env(&self) -> Option<BTreeMap<String, String>> {
        lock(&self.inner.p8.session_env).clone()
    }

    pub fn set_session_env(&self, env: Option<BTreeMap<String, String>>) {
        *lock(&self.inner.p8.session_env) = env;
    }

    /// Records the issuing assistant message of a model-issued tool call (senpi#5240).
    pub fn record_tool_call_issuer(&self, call_id: &str, message: AssistantMessage) {
        lock(&self.inner.p8.tool_call_issuers).insert(call_id.to_owned(), message);
    }

    /// Returns the issuing assistant message for `call_id` or its root `<root>/<n>` call.
    pub fn root_tool_call_issuer(&self, call_id: &str) -> Option<AssistantMessage> {
        let root = root_tool_call_id(call_id);
        lock(&self.inner.p8.tool_call_issuers).get(root).cloned()
    }

    /// Marks an exec-bridge tool call settled on every completion path (senpi#5780).
    pub fn settle_exec_bridge_tool_call(&self, tool_call_id: &str) {
        lock(&self.inner.p8.tool_call_issuers).remove(tool_call_id);
        lock(&self.inner.p8.exec_bridge_settled).insert(tool_call_id.to_owned());
    }

    /// Active turn id while streaming, if any.
    pub fn current_turn_id(&self) -> Option<String> {
        let ledger = lock(&self.inner.lifecycle);
        if ledger.run_active {
            ledger.run_id.clone()
        } else {
            None
        }
    }

    /// Refuses input bound to an `expectedTurnId` that no longer matches or is held (senpi#5776).
    pub fn refuse_stale_named_turn(
        &self,
        expected_turn_id: Option<&str>,
        is_person: bool,
    ) -> Result<(), TurnConflictError> {
        let current = self.current_turn_id();
        if let Some(err) = turn_conflict(expected_turn_id, current.as_deref()) {
            return Err(err);
        }
        if expected_turn_id.is_some() && is_person && lock(&self.inner.engine_turn_hold).held() {
            return Err(TurnConflictError::new(current));
        }
        Ok(())
    }

    pub async fn prompt_with_expected_turn(
        &self,
        text: &str,
        options: PromptOptions,
        expected_turn_id: Option<&str>,
    ) -> Result<PromptDisposition, String> {
        let is_person = crate::input_origin::input_origin_of(options.source)
            == crate::input_origin::InputOrigin::Person;
        self.refuse_stale_named_turn(expected_turn_id, is_person)
            .map_err(|err| err.to_string())?;
        self.prompt(text, options).await
    }

    pub async fn steer_with_expected_turn(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
        options: QueuedInputOptions,
        expected_turn_id: Option<&str>,
    ) -> Result<(), String> {
        let is_person = crate::input_origin::input_origin_of(options.source)
            == crate::input_origin::InputOrigin::Person;
        self.refuse_stale_named_turn(expected_turn_id, is_person)
            .map_err(|err| err.to_string())?;
        self.steer(text, images, options).await
    }

    pub async fn follow_up_with_expected_turn(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
        options: QueuedInputOptions,
        expected_turn_id: Option<&str>,
    ) -> Result<(), String> {
        let is_person = crate::input_origin::input_origin_of(options.source)
            == crate::input_origin::InputOrigin::Person;
        self.refuse_stale_named_turn(expected_turn_id, is_person)
            .map_err(|err| err.to_string())?;
        self.follow_up(text, images, options).await
    }

    /// Applies senpi#5568/#5590/#6536 account-hold releases before starting a turn on `model`.
    pub async fn prepare_provider_turn_holds(&self, model: &Model, is_user_turn: bool) {
        let is_spent = {
            let mut guard = self.inner.retry_fallback.lock().await;
            guard
                .as_mut()
                .is_some_and(|c| c.is_provider_spent(&model.provider))
        };
        if !is_spent {
            return;
        }
        if is_user_turn {
            crate::subscription_limits::usage_poller::refresh_subscription_usage(&model.provider)
                .await;
        }
        let snapshot = crate::subscription_limits::limit_store::subscription_limits().snapshot();
        let mut guard = self.inner.retry_fallback.lock().await;
        if let Some(controller) = guard.as_mut() {
            controller.release_account_hold_shown_servable(&model.provider, &snapshot);
            if is_user_turn {
                controller.release_guessed_account_hold(&model.provider);
            }
        }
    }

    /// Formats compaction failure notices cleanly using senpi#4329 helper.
    pub fn format_compaction_failure_notice(
        &self,
        kind: CompactionFailureKind,
        error: &str,
    ) -> String {
        let model = self.agent.state().model;
        let label = format!("{}/{}", model.provider, model.id);
        format_compaction_failure(kind, error, Some(&label))
    }

    /// Registers full prompt guidelines for a tool (senpi#5665).
    pub fn set_full_tool_prompt_guidelines(&self, tool_name: &str, guidelines: Vec<String>) {
        lock(&self.inner.p8.full_tool_prompt_guidelines).insert(tool_name.to_owned(), guidelines);
    }

    pub fn build_full_tool_prompt(
        &self,
        snippets: &BTreeMap<String, String>,
        fallback_guidelines: &[String],
    ) -> FullToolPrompt {
        let stored = lock(&self.inner.p8.full_tool_prompt_guidelines);
        let mut guidelines: Vec<String> = stored.values().flatten().cloned().collect();
        if guidelines.is_empty() {
            guidelines.extend(fallback_guidelines.iter().cloned());
        }
        FullToolPrompt {
            tool_snippets: snippets
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            prompt_guidelines: guidelines,
        }
    }
}

pub(crate) fn weak_provider_recovery_revision(
    inner: &std::sync::Weak<AgentSessionInner>,
    provider: &str,
) -> Option<String> {
    let inner = inner.upgrade()?;
    crate::credential_recovery_revision::credential_recovery_revision(
        Some(&inner.model_registry.auth_storage),
        provider,
    )
}

#[cfg(test)]
mod tests {
    use super::root_tool_call_id;

    #[test]
    fn root_tool_call_id_extracts_root_segment() {
        assert_eq!(root_tool_call_id("call_1"), "call_1");
        assert_eq!(root_tool_call_id("call_1/2"), "call_1");
        assert_eq!(root_tool_call_id("call_1/2/3"), "call_1");
    }
}
