//! Port of senpi's compaction `context-reduction-lifecycle.ts`.
use crate::{
    context_pipeline::{CompactionContextInput, build_compaction_context},
    context_reduction_handoff::{ReductionHandoff, create_reduction_handoff},
    context_reduction_state::{
        CONTEXT_REDUCTION_ENTRY_TYPE, ContextReductionRequest, ContextReductionState, clear_reduction_anchor,
        create_context_reduction_state, read_context_reduction_state, record_reduction_usage, snapshot_reduction_state,
    },
    emergency_prune::EmergencyPruneLatch,
    extension_wiring, openai_remote_model, orchestration,
    overflow_retry::estimate_total_tokens,
    policy,
    speculative,
};
use maho_ext_api::{
    AgentMessage, ContextEventSource, EventKind, EventResult, ExtensionApi, ExtensionContext, ExtensionEvent,
    ExtensionFailure, ExtensionHandlerOptions,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, PoisonError};

/// What the lifecycle reads from the extension's compaction state for one context event.
#[derive(Clone, Copy, Debug, Default)]
pub struct ContextReductionPolicy {
    pub last_yield: Option<policy::CompactionYield>,
    pub breaker_tripped: bool,
    pub lane_owns_compaction: bool,
    pub append_only_transcript: bool,
}

/// Emits a compaction log event (`emergency_prune`, `breaker_deterministic_fallback`).
pub type ContextReductionLog = Arc<dyn Fn(&ExtensionContext, &str, Value) + Send + Sync>;

pub struct ContextReductionLifecycleOptions {
    pub emergency_prune_latch: Arc<Mutex<EmergencyPruneLatch>>,
    pub emergency_instructions: String,
    pub get_tools: Arc<dyn Fn() -> Vec<maho_ai::types::Tool> + Send + Sync>,
    pub get_policy: Arc<dyn Fn(&ExtensionContext) -> ContextReductionPolicy + Send + Sync>,
    pub log: ContextReductionLog,
}

#[derive(Default)]
struct Inner {
    state: ContextReductionState,
    session_id: Option<String>,
    pending: Option<(ContextReductionRequest, Option<String>)>,
}

#[derive(Clone)]
pub struct ContextReductionLifecycle {
    inner: Arc<Mutex<Inner>>,
}

impl ContextReductionLifecycle {
    pub fn reset(&self) {
        *self.inner.lock().unwrap_or_else(PoisonError::into_inner) = Inner::default();
    }
}

/// Persists the state when its saved form changed during this event.
fn persist(api: &ExtensionApi, state: &ContextReductionState, before: &Value) -> Result<(), ExtensionFailure> {
    let saved = snapshot_reduction_state(state);
    if &saved != before {
        api.append_entry(CONTEXT_REDUCTION_ENTRY_TYPE, Some(saved))?;
    }
    Ok(())
}

/// Reload the current branch's record once per session id; the frontier validates its
/// fingerprint against the live projection before reusing it.
fn restore(inner: &mut Inner, context: &ExtensionContext) {
    let session_id = context.session_manager.session_id();
    if inner.session_id.as_deref() == Some(session_id) {
        return;
    }
    inner.state = create_context_reduction_state();
    for entry in context.session_manager.get_branch().iter().rev() {
        if entry.kind == "compaction" {
            break;
        }
        if entry.kind == "custom" && entry.data.get("customType").and_then(Value::as_str) == Some(CONTEXT_REDUCTION_ENTRY_TYPE) {
            inner.state = entry.data.get("data").and_then(read_context_reduction_state).unwrap_or_default();
            break;
        }
    }
    inner.session_id = Some(session_id.to_owned());
}

fn context_handler(
    api: &ExtensionApi,
    handoff: &ReductionHandoff,
    inner: &Mutex<Inner>,
    options: &ContextReductionLifecycleOptions,
    event: &ExtensionEvent,
    context: &ExtensionContext,
) -> Result<EventResult, ExtensionFailure> {
    let ExtensionEvent::Context { messages, entry_ids, source } = event else { return Ok(EventResult::None) };
    let mut inner = inner.lock().unwrap_or_else(PoisonError::into_inner);
    restore(&mut inner, context);
    // The journal may not yet contain the current user/tool message. The dispatcher, not
    // transcript equality, owns request provenance.
    let live = *source != ContextEventSource::Projection;
    let mut scratch = inner.state.clone();
    let working = if live { &mut inner.state } else { &mut scratch };
    if !live {
        clear_reduction_anchor(working);
    }
    if !live || !handoff.is_pending() {
        working.compaction_required = false;
    }
    let before = snapshot_reduction_state(working);
    let usage = context.get_context_usage()?;
    let window = usage.as_ref().map_or_else(|| context.model.as_ref().map_or(200_000, |model| model.context_window), |usage| usage.context_window);
    let settings = extension_wiring::live_settings(context)?;
    let policy = (options.get_policy)(context);
    let overhead_text = format!("{}\n{}", context.get_system_prompt(), serde_json::to_string(&(options.get_tools)()).map_err(|error| ExtensionFailure::new(error.to_string()))?);
    let overhead_tokens = estimate_total_tokens(&[json!({"role":"user","content":overhead_text,"timestamp":0})]);
    let prompt_window = extension_wiring::get_prompt_context_window(window as f64, context.model.as_ref().map(|model| model.max_tokens as f64));
    let geometry = orchestration::resolve_compaction_geometry(window as f64, &settings, policy.last_yield);
    let tokens = usage.as_ref().and_then(|usage| usage.tokens).map(|tokens| tokens as f64);
    let breaker_fallback = !policy.lane_owns_compaction
        && policy.breaker_tripped
        && tokens.is_some_and(|tokens| tokens >= window as f64 * crate::policy::compute_effective_threshold(window as f64, policy.last_yield));
    if breaker_fallback {
        (options.log)(context, "breaker_deterministic_fallback", json!({"route":"context-event","tokens":tokens.unwrap_or(0.)}));
    }
    let raw = messages.iter().map(serde_json::to_value).collect::<Result<Vec<_>, _>>().map_err(|error| ExtensionFailure::new(error.to_string()))?;
    let native = openai_remote_model::is_openai_remote_compaction_model(context.model.as_ref());
    // Only the provider-native replay boundary reads the branch; skip the full branch copy otherwise.
    let branch_entries = if native { speculative::branch_values(context) } else { Vec::new() };
    let mut live_latch = options.emergency_prune_latch.lock().unwrap_or_else(PoisonError::into_inner);
    let mut scratch_latch = *live_latch;
    let latch = if live { &mut *live_latch } else { &mut scratch_latch };
    let output = build_compaction_context(CompactionContextInput {
        messages: &raw, entry_ids, model: context.model.as_ref(), branch_entries: &branch_entries, context_window: window,
        prompt_context_window: prompt_window as u64, usage_tokens: tokens, provider_native_path: native,
        tool_admission_enabled: settings.ideal.tool_admission_enabled != Some(false), breaker_fallback,
        lane_owns_compaction: policy.lane_owns_compaction, context_reduction_state: Some(working), unreduced_messages: None,
        context_overhead_tokens: overhead_tokens,
        reduction_ceiling_tokens: Some(prompt_window.min(window as f64 - geometry.reserve_tokens).min(geometry.threshold_tokens).max(0.) as u64),
        reduction_block_budget_ratio: None, append_only_transcript: policy.append_only_transcript,
        emergency_prune_latch: latch, reminder: None, now: chrono::Utc::now().timestamp_millis(),
    }).map_err(|error| ExtensionFailure::new(error.to_string()))?;
    drop(live_latch);
    if let Some((tokens_before, tokens)) = output.emergency_prune_tokens {
        (options.log)(context, "emergency_prune", json!({"route":"context-event","tokensBefore":tokens_before,"tokens":tokens}));
    }
    let result = EventResult::Context { messages: Some(output.messages.into_iter().map(AgentMessage::from).collect()) };
    if !live {
        return Ok(result);
    }
    let managed = !policy.lane_owns_compaction && !policy.append_only_transcript && !native;
    if !managed {
        inner.pending = None;
        return Ok(result);
    }
    if inner.state.compaction_required {
        inner.pending = None;
        persist(api, &inner.state, &before)?;
        drop(inner);
        // Never dispatch a prompt that is legal only because pruning changed its lineage. Only an
        // accepted, current handoff can resume work.
        handoff.request(context)?;
    } else {
        persist(api, &inner.state, &before)?;
        inner.pending = Some((
            ContextReductionRequest { count: messages.len(), cut: inner.state.cut_index, hash: inner.state.prefix_hash.clone() },
            context.session_manager.get_leaf_id(),
        ));
    }
    Ok(result)
}

fn message_end_handler(api: &ExtensionApi, inner: &Mutex<Inner>, event: &ExtensionEvent, context: &ExtensionContext) -> Result<(), ExtensionFailure> {
    let ExtensionEvent::MessageEnd { message } = event else { return Ok(()) };
    let Some(assistant) = message.as_assistant() else { return Ok(()) };
    let mut inner = inner.lock().unwrap_or_else(PoisonError::into_inner);
    let Some((request, leaf)) = inner.pending.clone() else { return Ok(()) };
    if inner.session_id.as_deref() != Some(context.session_manager.session_id())
        || matches!(assistant.stop_reason, maho_ai::types::StopReason::Aborted | maho_ai::types::StopReason::Error)
        || leaf.as_ref().is_some_and(|leaf| !context.session_manager.get_branch().iter().any(|entry| &entry.id == leaf))
    {
        return Ok(());
    }
    let before = snapshot_reduction_state(&inner.state);
    let usage = &assistant.usage;
    let tokens = match usage.context_usage {
        None => usage.input + usage.cache_read + usage.cache_write,
        Some(measured) => measured.map_or(0, |measured| measured.input),
    };
    record_reduction_usage(&mut inner.state, &request, tokens as f64);
    inner.pending = None;
    persist(api, &inner.state, &before)
}

/// Registers the context-reduction lifecycle on the extension; the handlers keep the order upstream
/// registers them in (session resets, message_end anchor, context).
pub fn register_context_reduction_lifecycle(
    api: &mut ExtensionApi,
    live_api: Arc<ExtensionApi>,
    options: ContextReductionLifecycleOptions,
) -> ContextReductionLifecycle {
    let lifecycle = ContextReductionLifecycle { inner: Arc::default() };
    let handoff = create_reduction_handoff(Arc::clone(&live_api), &options.emergency_instructions);
    // Navigation reloads the destination branch's own record.
    for kind in [EventKind::SessionStart, EventKind::SessionTree, EventKind::SessionShutdown] {
        let (lifecycle, handoff) = (lifecycle.clone(), handoff.clone());
        api.on(kind, Arc::new(move |_, _| {
            handoff.reset();
            lifecycle.reset();
            Box::pin(async { Ok(EventResult::None) })
        }));
    }
    {
        let (live_api, inner) = (Arc::clone(&live_api), Arc::clone(&lifecycle.inner));
        api.on(EventKind::MessageEnd, Arc::new(move |event, context| {
            let result = message_end_handler(&live_api, &inner, event, context).map(|()| EventResult::None);
            Box::pin(async move { result })
        }));
    }
    let options = Arc::new(options);
    let inner = Arc::clone(&lifecycle.inner);
    api.on_with_options(EventKind::Context, Arc::new(move |event, context| {
        let result = context_handler(&live_api, &handoff, &inner, &options, event, context);
        Box::pin(async move { result })
    }), ExtensionHandlerOptions { mutates_messages: Some(false), ..Default::default() });
    lifecycle
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engaged_state() -> ContextReductionState {
        ContextReductionState { engaged: true, cut_index: 2, prefix_hash: "h".into(), compaction_required: true, ..Default::default() }
    }

    #[test]
    fn projection_works_on_a_scratch_copy_without_anchor_or_request_latch() {
        // Mirrors the context handler's prologue: a projection never touches the live state.
        let live = ContextReductionState { anchor_count: Some(3), anchor_cut: Some(2), anchor_hash: Some("h".into()), anchor_tokens: Some(9), ..engaged_state() };
        let mut scratch = live.clone();
        clear_reduction_anchor(&mut scratch);
        scratch.compaction_required = false;
        assert_eq!(scratch.anchor_tokens, None);
        assert!(live.compaction_required && live.anchor_tokens == Some(9));
    }

    #[test]
    fn reset_clears_state_session_and_pending_request() {
        let lifecycle = ContextReductionLifecycle { inner: Arc::default() };
        {
            let mut inner = lifecycle.inner.lock().unwrap_or_else(PoisonError::into_inner);
            inner.state = engaged_state();
            inner.session_id = Some("s".into());
            inner.pending = Some((ContextReductionRequest { count: 1, cut: 2, hash: "h".into() }, None));
        }
        lifecycle.reset();
        let inner = lifecycle.inner.lock().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(inner.state, ContextReductionState::default());
        assert!(inner.session_id.is_none() && inner.pending.is_none());
    }
}
