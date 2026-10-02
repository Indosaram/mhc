//! Compaction policy primitives ported from senpi's builtin compaction extension.
pub mod policy;
pub mod speculation_lead;
pub mod idle;
pub mod idle_retry;
pub mod summarization_retry;
pub mod task_intent;
pub mod token_budget_reminder;
pub mod tool_truncation;
pub mod state;
pub mod circuit_breaker;
pub mod per_turn_cap;
pub mod degradation_monitor;
pub mod repair_tool_pairs;
pub mod summarization_turn_order;
pub mod overflow_retry;
pub mod tool_admission;
pub mod context_reduction;
pub mod lane_policy;
pub mod r#yield;
pub mod openai_remote_schema;
pub mod fallback_failed_turn_normalization;
pub mod orchestration;
pub mod retained_message_safety;
pub mod emergency_prune;
pub mod model_usability_budget;
pub mod switch_admission;
pub mod resume_admission;
pub mod resume_slice;
pub mod restoration_tracker;
pub mod checkpoint_state;
pub mod model_selection;
pub mod extension_wiring;
pub mod openai_remote_model;
pub mod openai_remote_convert;
pub mod transient_failure;
pub mod openai_remote_timeout;
pub mod log;
pub mod prompts;
pub mod openai_remote_responses_v2;
pub mod speculative_summary;
pub mod speculative_job;
pub mod speculative;
pub mod todo_bridge;
pub mod openai_remote_dependencies;
pub mod context_pipeline;
pub mod deterministic_fallback;
pub mod openai_remote;

#[derive(Default)]
pub struct CompactionExtension;

impl maho_ext_api::Extension for CompactionExtension {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        let state = std::sync::Arc::new(std::sync::Mutex::new(state::create_initial_state()));
        let restoration = std::sync::Arc::new(std::sync::Mutex::new(restoration_tracker::RestorationTrackerState::default()));
        let pending = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, (serde_json::Value, serde_json::Value))>::new()));
        let live_api = std::sync::Arc::new(maho_ext_api::ExtensionApi::new(api.registered.clone(), api.profile.clone(), api.events.clone(), api.runtime.clone()));
        {
            let latch = std::sync::Arc::new(std::sync::Mutex::new(emergency_prune::EmergencyPruneLatch::default()));
            let state = std::sync::Arc::clone(&state);
            api.on(maho_ext_api::EventKind::Context, std::sync::Arc::new(move |event, context| {
                let result = (|| {
                    let maho_ext_api::ExtensionEvent::Context { messages } = event else { return Ok(maho_ext_api::EventResult::None); };
                    // Todo 24 owns the resolved SDK resume-mode loader. Do not prune
                    // a provider-owned history until that binding is available.
                    if context.model.as_ref().is_some_and(|model| model.provider == "anthropic-subscription") {
                        return Ok(maho_ext_api::EventResult::None);
                    }
                    let usage = context.get_context_usage()?;
                    let window = usage.as_ref().map_or_else(||context.model.as_ref().map_or(200_000, |model|model.context_window), |usage|usage.context_window);
                    let tokens = usage.and_then(|usage|usage.tokens).map(|tokens|tokens as f64);
                    let state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let breaker_fallback = circuit_breaker::is_tripped(&state, chrono::Utc::now().timestamp_millis() as f64)
                        && tokens.is_some_and(|tokens|tokens >= window as f64 * policy::compute_effective_threshold(window as f64, state.last_yield));
                    let raw = messages.iter().map(serde_json::to_value).collect::<Result<Vec<_>, _>>().map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                    let output = context_pipeline::build_compaction_context(context_pipeline::CompactionContextInput {
                        messages: &raw, context_window: window,
                        prompt_context_window: extension_wiring::get_prompt_context_window(window as f64, context.model.as_ref().map(|model|model.max_tokens as f64)) as u64,
                        usage_tokens: tokens, provider_native_path: openai_remote_model::is_openai_remote_compaction_model(context.model.as_ref()),
                        tool_admission_enabled: true, breaker_fallback, lane_owns_compaction: false,
                        emergency_prune_latch: &mut latch.lock().unwrap_or_else(std::sync::PoisonError::into_inner), reminder: None,
                        now: chrono::Utc::now().timestamp_millis(),
                    }).map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                    let messages = output.messages.into_iter().map(|message|serde_json::to_value(message).and_then(serde_json::from_value)).collect::<Result<Vec<_>, _>>().map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                    Ok(maho_ext_api::EventResult::Context { messages: Some(messages) })
                })();
                Box::pin(async move { result })
            }));
        }
        {
            let restoration = std::sync::Arc::clone(&restoration);
            api.on(maho_ext_api::EventKind::ToolCall, std::sync::Arc::new(move |event, _context| {
                if let maho_ext_api::ExtensionEvent::ToolCall(event) = event {
                    restoration_tracker::track_tool_call(&mut restoration.lock().unwrap_or_else(std::sync::PoisonError::into_inner), &event.tool_name, &event.input);
                }
                Box::pin(async { Ok(maho_ext_api::EventResult::None) })
            }));
        }
        {
            let pending = std::sync::Arc::clone(&pending);
            let live_api = std::sync::Arc::clone(&live_api);
            let state = std::sync::Arc::clone(&state);
            api.on(maho_ext_api::EventKind::SessionBeforeCompact, std::sync::Arc::new(move |event, context| {
                let gate = if let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event {
                    let state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !event.signal.is_aborted() && per_turn_cap::should_reject_by_cap(&state).cancel {
                        Some(maho_ext_api::SessionBeforeEventResult { cancel: Some(true), rejection_cause: Some(maho_ext_api::CompactionRejectionCause::PerTurnCap), reason: Some("absolute compaction cap reached for this session".into()), ..Default::default() })
                    } else if !event.signal.is_aborted() && circuit_breaker::is_tripped(&state, chrono::Utc::now().timestamp_millis() as f64) && !circuit_breaker::should_bypass(&state, false, Some(event.reason)) {
                        Some(maho_ext_api::SessionBeforeEventResult { cancel: Some(true), rejection_cause: Some(maho_ext_api::CompactionRejectionCause::CircuitBreaker), ..Default::default() })
                    } else { None }
                } else { None };
                if let Some(gate) = gate { return Box::pin(async move { Ok(maho_ext_api::EventResult::SessionBefore(gate)) }); }
                let result = if let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event {
                    if event.signal.is_aborted() { Ok(()) } else {
                        checkpoint_state::capture_live_agent_checkpoint(&live_api, context).map(|checkpoint| {
                            let mut pending = pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            let metadata = (checkpoint, todo_bridge::create_todo_snapshot(context));
                            if let Some((_, value)) = pending.iter_mut().find(|(id, _)| id == &event.request_id) {
                                *value = metadata;
                            } else {
                                pending.push((event.request_id.clone(), metadata));
                            }
                            if pending.len() > 8 { pending.remove(0); }
                        })
                    }
                } else { Ok(()) };
                let live_api = std::sync::Arc::clone(&live_api);
                let pending = std::sync::Arc::clone(&pending);
                Box::pin(async move {
                    result?;
                    let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else { return Ok(maho_ext_api::EventResult::None); };
                    let result = extension_wiring::generate_core_route_compaction(&live_api, context, event).await;
                    if !matches!(&result, Ok(maho_ext_api::EventResult::SessionBefore(result)) if result.compaction.is_some()) {
                        pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|(id, _)|id != &event.request_id);
                    }
                    result
                })
            }));
        }
        {
            let pending = std::sync::Arc::clone(&pending);
            let live_api = std::sync::Arc::clone(&live_api);
            api.on(maho_ext_api::EventKind::SessionCompact, std::sync::Arc::new(move |event, context| {
                let result = if let maho_ext_api::ExtensionEvent::SessionCompact(maho_ext_api::SessionCompactEvent::Accepted { request_id, .. }) = event {
                    let metadata = {
                        let mut pending = pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        pending.iter().position(|(id, _)| id == request_id).map(|index| pending.remove(index).1)
                    };
                    if let Some((checkpoint, todos)) = metadata {
                        checkpoint_state::persist_checkpoint(&live_api, &checkpoint)
                            .and_then(|()| live_api.append_entry(todo_bridge::TODO_SNAPSHOT_CUSTOM_TYPE, Some(todos)))
                            .and_then(|()| todo_bridge::restore_todos_if_missing(&live_api, context))
                    } else { Ok(()) }
                } else { Ok(()) };
                Box::pin(async move { result?; Ok(maho_ext_api::EventResult::None) })
            }));
        }
        {
            let directive = std::sync::Arc::new(std::sync::Mutex::new(checkpoint_state::RestorationDirectiveState::default()));
            let restoration = std::sync::Arc::clone(&restoration);
            api.on(maho_ext_api::EventKind::BeforeAgentStart, std::sync::Arc::new(move |_event, context| {
                let entries: Vec<_> = context.session_manager.get_entries().into_iter().map(|entry| {
                    let mut value = entry.data;
                    value["type"] = serde_json::json!(entry.kind);
                    value["id"] = serde_json::json!(entry.id);
                    value
                }).collect();
                let checkpoint = extension_wiring::recent_checkpoint(&entries, chrono::Utc::now().timestamp_millis() as f64);
                let pending = restoration_tracker::consume_pending_payload(&mut restoration.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
                let message = checkpoint_state::attach_restoration_directive(&mut directive.lock().unwrap_or_else(std::sync::PoisonError::into_inner), checkpoint.as_ref(), pending);
                let result = message.map(|message| Ok(maho_ext_api::CustomMessage {
                    custom_type: message["customType"].as_str().unwrap_or_default().to_owned(),
                    content: vec![maho_ext_api::ToolContent::text(message["content"].as_str().unwrap_or_default())],
                    display: message["display"].as_bool().unwrap_or(false), details: message.get("details").cloned(),
                })).transpose().map(|message| maho_ext_api::EventResult::BeforeAgentStart(maho_ext_api::BeforeAgentStartEventResult { message, system_prompt: None }));
                Box::pin(async move { result })
            }));
        }
        for kind in [maho_ext_api::EventKind::TurnEnd, maho_ext_api::EventKind::AgentEnd,
            maho_ext_api::EventKind::SessionCompact] {
            let state = std::sync::Arc::clone(&state);
            api.on(kind, std::sync::Arc::new(move |event, _context| {
                let state = std::sync::Arc::clone(&state);
                Box::pin(async move {
                    let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let previous = std::mem::take(&mut *state);
                    *state = match event {
                        maho_ext_api::ExtensionEvent::TurnEnd { .. } | maho_ext_api::ExtensionEvent::AgentEnd { .. } => state::reset_turn_counter(previous, ""),
                        maho_ext_api::ExtensionEvent::SessionCompact(maho_ext_api::SessionCompactEvent::Accepted { compaction_entry, .. }) => {
                            let mut next = circuit_breaker::record_success(per_turn_cap::increment_accepted(previous));
                            if let Some(yield_data) = compaction_entry.data.get("details").and_then(|details| details.get("structuralYield"))
                                && let (Some(saved_tokens), Some(savings_ratio), Some(tokens_before)) = (
                                    yield_data.get("savedTokens").and_then(serde_json::Value::as_f64),
                                    yield_data.get("savingsRatio").and_then(serde_json::Value::as_f64),
                                    compaction_entry.data.get("tokensBefore").and_then(serde_json::Value::as_f64)) {
                                next.last_yield = Some(policy::CompactionYield { saved_tokens, tokens_before });
                                if r#yield::is_ineffective_compaction(r#yield::StructuralYield { saved_tokens, savings_ratio, tokens_before }) {
                                    next = per_turn_cap::increment_ineffective(next);
                                }
                            }
                            next
                        }
                        maho_ext_api::ExtensionEvent::SessionCompact(maho_ext_api::SessionCompactEvent::Rejected { reason, rejection_cause, .. })
                            if *rejection_cause != maho_ext_api::CompactionRejectionCause::ExternalOwner => {
                                circuit_breaker::record_failure(previous, chrono::Utc::now().timestamp_millis() as f64, Some(*reason), None)
                            }
                        _ => previous,
                    };
                    Ok(maho_ext_api::EventResult::None)
                })
            }));
        }
    }
}
