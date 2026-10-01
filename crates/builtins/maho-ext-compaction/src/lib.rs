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
                        _ => previous,
                    };
                    Ok(maho_ext_api::EventResult::None)
                })
            }));
        }
    }
}
