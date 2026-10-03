//! Inverse of `session_binding::session_event_record`. The `agent_end` and `message_update` tags
//! exist in both the agent and session unions, so those collisions are resolved explicitly.
use serde::de::DeserializeOwned;
use serde_json::Value;

use maho_ext_api::{
    AbortSource, AgentEvent, AgentSessionEvent, CompactionReason, CompactionRejectionCause,
    ModelBudget, ModelSelectSource, QueuedInput, ServiceTier, SessionEntry, SkillInvocation,
    StreamingBehavior, ToolHookLifecycleEvent, ToolHookName, ToolHookPhase, ToolHookStatus,
};

fn get<T: DeserializeOwned>(record: &Value, key: &str) -> Option<T> {
    serde_json::from_value(record.get(key)?.clone()).ok()
}

fn agent(record: &Value) -> Option<AgentSessionEvent> {
    serde_json::from_value::<AgentEvent>(record.clone()).ok().map(AgentSessionEvent::Agent)
}

fn reason(value: &str) -> Option<CompactionReason> {
    Some(match value {
        "manual" => CompactionReason::Manual,
        "threshold" => CompactionReason::Threshold,
        "overflow" => CompactionReason::Overflow,
        "pre_prompt" => CompactionReason::PrePrompt,
        "branch" => CompactionReason::Branch,
        "extension" => CompactionReason::Extension,
        _ => return None,
    })
}

fn cause(value: &str) -> Option<CompactionRejectionCause> {
    Some(match value {
        "cancelled-by-extension" => CompactionRejectionCause::CancelledByExtension,
        "external-owner" => CompactionRejectionCause::ExternalOwner,
        "would-overflow" => CompactionRejectionCause::WouldOverflow,
        "circuit-breaker" => CompactionRejectionCause::CircuitBreaker,
        "per-turn-cap" => CompactionRejectionCause::PerTurnCap,
        "stale-revision" => CompactionRejectionCause::StaleRevision,
        _ => return None,
    })
}

fn model_source(value: &str) -> Option<ModelSelectSource> {
    Some(match value {
        "set" => ModelSelectSource::Set,
        "cycle" => ModelSelectSource::Cycle,
        "restore" => ModelSelectSource::Restore,
        "fallback" => ModelSelectSource::Fallback,
        "fallback_revert" => ModelSelectSource::FallbackRevert,
        _ => return None,
    })
}

fn service_tier(value: &str) -> Option<ServiceTier> {
    Some(match value {
        "auto" => ServiceTier::Auto,
        "flex" => ServiceTier::Flex,
        "priority" => ServiceTier::Priority,
        _ => return None,
    })
}

fn streaming_behavior(value: &str) -> Option<StreamingBehavior> {
    Some(match value {
        "steer" => StreamingBehavior::Steer,
        "followUp" => StreamingBehavior::FollowUp,
        _ => return None,
    })
}

fn abort_source(value: &str) -> Option<AbortSource> {
    Some(match value {
        "user" => AbortSource::User,
        "system" => AbortSource::System,
        "provider" => AbortSource::Provider,
        _ => return None,
    })
}

fn budget(value: &Value) -> Option<ModelBudget> {
    Some(ModelBudget {
        context_window: value.get("contextWindow")?.as_u64()?,
        live_context_tokens: value.get("liveContextTokens")?.as_u64()?,
        required_tokens: value.get("requiredTokens")?.as_u64()?,
        shortfall_tokens: value.get("shortfallTokens")?.as_u64()?,
        safety_margin_profile: value.get("safetyMarginProfile").and_then(Value::as_str).map(str::to_owned),
    })
}

fn compaction_result(value: &Value) -> Option<maho_ext_api::CompactionResult> {
    Some(maho_ext_api::CompactionResult {
        summary: get(value, "summary")?,
        first_kept_entry_id: get(value, "firstKeptEntryId")?,
        tokens_before: get(value, "tokensBefore")?,
        details: value.get("details").cloned(),
    })
}

fn entry(value: &Value) -> Option<SessionEntry> {
    let mut data = value.clone();
    if let Some(object) = data.as_object_mut() {
        object.remove("id");
        object.remove("parentId");
        object.remove("timestamp");
        object.remove("type");
    }
    Some(SessionEntry { id: get(value, "id")?, parent_id: get(value, "parentId"), timestamp: get(value, "timestamp")?, kind: get(value, "type")?, data })
}

fn skill(value: &Value) -> Option<SkillInvocation> {
    Some(SkillInvocation { name: get(value, "name")?, path: get(value, "path")?, syntax: get(value, "syntax")? })
}

fn queued_input(value: &Value) -> Option<QueuedInput> {
    Some(QueuedInput {
        text: get(value, "text")?,
        mode: streaming_behavior(value.get("mode")?.as_str()?)?,
        enqueue_order: get(value, "enqueueOrder")?,
    })
}

fn tool_hook(value: &Value) -> Option<ToolHookLifecycleEvent> {
    let phase = match value.get("phase")?.as_str()? {
        "start" => ToolHookPhase::Start,
        "update" => ToolHookPhase::Update,
        "end" => ToolHookPhase::End {
            completed_at: get(value, "completedAt")?,
            status: match value.get("status")?.as_str()? {
                "completed" => ToolHookStatus::Completed,
                "blocked" => ToolHookStatus::Blocked,
                "failed" => ToolHookStatus::Failed,
                _ => return None,
            },
            error_message: get(value, "errorMessage"),
        },
        _ => return None,
    };
    Some(ToolHookLifecycleEvent {
        hook_run_id: get(value, "hookRunId")?,
        hook_name: match value.get("hookName")?.as_str()? {
            "PreToolUse" => ToolHookName::PreToolUse,
            "PostToolUse" => ToolHookName::PostToolUse,
            _ => return None,
        },
        tool_name: get(value, "toolName")?,
        tool_call_id: get(value, "toolCallId")?,
        extension_path: get(value, "extensionPath")?,
        status_message: get(value, "statusMessage")?,
        started_at: get(value, "startedAt")?,
        phase,
    })
}

pub fn session_event_from_record(record: &Value) -> Option<AgentSessionEvent> {
    use AgentSessionEvent as E;
    let kind = record.get("type")?.as_str()?;
    match kind {
        "agent_end" => {
            if record.get("aborted").is_some() || record.get("willRetry").is_some() {
                Some(E::AgentEnd {
                    messages: get(record, "messages")?,
                    aborted: record.get("aborted").and_then(Value::as_bool).unwrap_or(false),
                    will_retry: record.get("willRetry").and_then(Value::as_bool).unwrap_or(false),
                    abort_source: record.get("abortSource").and_then(Value::as_str).and_then(abort_source),
                })
            } else {
                agent(record)
            }
        }
        "agent_settled" => Some(E::AgentSettled),
        "agent_idle" => Some(E::AgentIdle),
        "session_abort" => Some(E::SessionAbort),
        "continuation_error" => Some(E::ContinuationError { error_message: get(record, "errorMessage")? }),
        "resume_compaction_required" => Some(E::ResumeCompactionRequired { projection: record.get("projection").cloned().unwrap_or(Value::Null), notice: get(record, "notice")? }),
        "resume_context_reduced" => Some(E::ResumeContextReduced {
            tokens_before: get(record, "tokensBefore")?, tokens_after: get(record, "tokensAfter")?,
            dropped_entries: get(record, "droppedEntries")?, notice: get(record, "notice")?,
        }),
        "compaction_start" => Some(E::CompactionStart { reason: reason(record.get("reason")?.as_str()?)?, request_id: get(record, "requestId") }),
        "compaction_progress" => Some(E::CompactionProgress { reason: reason(record.get("reason")?.as_str()?)?, delta: get(record, "delta"), text: get(record, "text") }),
        "compaction_end" => Some(E::CompactionEnd {
            reason: reason(record.get("reason")?.as_str()?)?,
            result: record.get("result").and_then(compaction_result),
            aborted: record.get("aborted").and_then(Value::as_bool).unwrap_or(false),
            will_retry: record.get("willRetry").and_then(Value::as_bool).unwrap_or(false),
            request_id: get(record, "requestId"),
            accepted: get(record, "accepted"),
            rejection_cause: record.get("rejectionCause").and_then(Value::as_str).and_then(cause),
            error_message: get(record, "errorMessage"),
        }),
        "auto_retry_start" => Some(E::AutoRetryStart { attempt: get(record, "attempt")?, max_attempts: get(record, "maxAttempts")?, delay_ms: get(record, "delayMs")?, error_message: get(record, "errorMessage")? }),
        "auto_retry_end" => Some(E::AutoRetryEnd { success: get(record, "success")?, attempt: get(record, "attempt")?, final_error: get(record, "finalError") }),
        "retry_fallback_applied" => Some(E::RetryFallbackApplied { from: get(record, "from")?, to: get(record, "to")?, chain_key: get(record, "chainKey")?, reason: get(record, "reason")? }),
        "retry_fallback_exhausted" => Some(E::RetryFallbackExhausted { chain_key: get(record, "chainKey")?, last_error: get(record, "lastError")? }),
        "retry_fallback_succeeded" => Some(E::RetryFallbackSucceeded { model: get(record, "model")?, chain_key: get(record, "chainKey")? }),
        "retry_fallback_reverted" => Some(E::RetryFallbackReverted { from: get(record, "from")?, to: get(record, "to")? }),
        "model_changed" => Some(E::ModelChanged { model: get(record, "model")?, thinking_level: get(record, "thinkingLevel")?, source: model_source(record.get("source")?.as_str()?)? }),
        "model_change_pending" => Some(E::ModelChangePending { model: get(record, "model")?, budget: budget(record)?, notice: get(record, "notice")? }),
        "model_change_rejected" => Some(E::ModelChangeRejected { model: get(record, "model")?, reason: get(record, "reason")?, detail: get(record, "detail")?, budget: record.get("contextWindow").is_some().then(|| budget(record)).flatten() }),
        "model_change_skipped" => Some(E::ModelChangeSkipped { model: get(record, "model")?, budget: budget(record)?, direction: get(record, "direction")? }),
        "service_tier_changed" => Some(E::ServiceTierChanged { tier: record.get("tier").and_then(Value::as_str).and_then(service_tier), fast_mode: get(record, "fastMode")? }),
        "thinking_level_changed" => Some(E::ThinkingLevelChanged { level: get(record, "level")? }),
        "high_reasoning_warning" => Some(E::HighReasoningWarning { model_id: get(record, "modelId")?, provider: get(record, "provider")?, thinking_level: get(record, "thinkingLevel")? }),
        "server_fallback_aborted" => Some(E::ServerFallbackAborted { from: get(record, "from")?, to: get(record, "to")?, chain_configured: get(record, "chainConfigured")? }),
        "session_settings_changed" => Some(E::SessionSettingsChanged { steering_mode: get(record, "steeringMode")?, follow_up_mode: get(record, "followUpMode")?, auto_compaction_enabled: get(record, "autoCompactionEnabled")? }),
        "settings_source_selected" => {
            let mut selection = record.clone();
            if let Some(object) = selection.as_object_mut() {
                object.remove("type");
            }
            Some(E::SettingsSourceSelected { selection })
        }
        "session_info_changed" => Some(E::SessionInfoChanged { name: get(record, "name") }),
        "system_prompt_change" => Some(E::SystemPromptChange { system_prompt: get(record, "systemPrompt")?, previous_system_prompt: get(record, "previousSystemPrompt")?, system_prompt_name: get(record, "systemPromptName"), model: get(record, "model")?, previous_model: get(record, "previousModel") }),
        "entry_appended" => Some(E::EntryAppended { entry: entry(record.get("entry")?)? }),
        "skill_invocation" => Some(E::SkillInvocation { skills: record.get("skills")?.as_array()?.iter().map(skill).collect::<Option<Vec<_>>>()? }),
        "command_invocation" => Some(E::CommandInvocation { command: record.get("command").cloned().unwrap_or(Value::Null) }),
        "queue_update" => Some(E::QueueUpdate { steering: get(record, "steering")?, follow_up: get(record, "followUp")?, ordered: record.get("ordered")?.as_array()?.iter().map(queued_input).collect::<Option<Vec<_>>>()? }),
        "tool_hook_status" => Some(E::ToolHookStatus(tool_hook(record)?)),
        "auth_login_url" => Some(E::AuthLoginUrl { provider: get(record, "provider")?, url: get(record, "url")? }),
        "auth_login_end" => Some(E::AuthLoginEnd { provider: get(record, "provider")?, success: get(record, "success")?, error: get(record, "error") }),
        "summarization_retry_scheduled" => Some(E::SummarizationRetryScheduled { attempt: get(record, "attempt")?, max_attempts: get(record, "maxAttempts")?, delay_ms: get(record, "delayMs")?, error_message: get(record, "errorMessage")? }),
        "summarization_retry_attempt_start" => Some(E::SummarizationRetryAttemptStart { source: get(record, "source")?, reason: record.get("reason").and_then(Value::as_str).and_then(reason) }),
        "summarization_retry_finished" => Some(E::SummarizationRetryFinished),
        "retry_probe_scheduled" => Some(E::RetryProbeScheduled { selector: get(record, "selector")?, at_ms: get(record, "atMs")?, probe_index: get(record, "probeIndex")? }),
        "retry_probe_result" => Some(E::RetryProbeResult { selector: get(record, "selector")?, ok: get(record, "ok")?, error_message: get(record, "errorMessage") }),
        "bash_execution_update" => Some(E::BashExecutionUpdate { id: get(record, "id"), delta: get(record, "delta")? }),
        _ => agent(record),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_binding::session_event_record;
    use serde_json::json;

    fn assert_roundtrips(event: &AgentSessionEvent) {
        let record = session_event_record(event).expect("serialize");
        let decoded = session_event_from_record(&record).unwrap_or_else(|| panic!("decode {record}"));
        let again = session_event_record(&decoded).expect("re-serialize");
        assert_eq!(again, record, "roundtrip {event:?}");
    }

    #[test]
    fn the_agent_union_roundtrips_through_serde() {
        assert_roundtrips(&AgentSessionEvent::Agent(AgentEvent::ToolExecutionStart { tool_call_id: "call".into(), tool_name: "read".into(), args: json!({"path": "a"}) }));
        assert_roundtrips(&AgentSessionEvent::Agent(AgentEvent::AgentStart));
    }

    #[test]
    fn agent_end_collision_prefers_the_session_shape_only_when_marked() {
        let session = AgentSessionEvent::AgentEnd { messages: Vec::new(), aborted: true, will_retry: false, abort_source: Some(AbortSource::User) };
        let record = session_event_record(&session).expect("serialize");
        assert_eq!(record["type"], "agent_end");
        assert!(matches!(session_event_from_record(&record), Some(AgentSessionEvent::AgentEnd { aborted: true, will_retry: false, abort_source: Some(AbortSource::User), .. })), "aborted/willRetry present decodes as the session agent_end");
        let agent = json!({"type": "agent_end", "messages": []});
        assert!(matches!(session_event_from_record(&agent), Some(AgentSessionEvent::Agent(AgentEvent::AgentEnd { .. }))), "a bare agent_end decodes as the agent lifecycle event");
    }

    #[test]
    fn session_only_variants_roundtrip_field_by_field() {
        let events = vec![
            AgentSessionEvent::AgentSettled,
            AgentSessionEvent::AgentIdle,
            AgentSessionEvent::SessionAbort,
            AgentSessionEvent::ContinuationError { error_message: "boom".into() },
            AgentSessionEvent::ResumeContextReduced { tokens_before: 10, tokens_after: 4, dropped_entries: 2, notice: "reduced".into() },
            AgentSessionEvent::CompactionStart { reason: CompactionReason::Overflow, request_id: Some("r1".into()) },
            AgentSessionEvent::CompactionEnd { reason: CompactionReason::Manual, result: Some(maho_ext_api::CompactionResult { summary: "s".into(), first_kept_entry_id: "e".into(), tokens_before: 9, details: Some(json!({"k": 1})) }), aborted: false, will_retry: true, request_id: Some("r".into()), accepted: Some(true), rejection_cause: Some(CompactionRejectionCause::WouldOverflow), error_message: Some("warn".into()) },
            AgentSessionEvent::AutoRetryStart { attempt: 2, max_attempts: 5, delay_ms: 250, error_message: "e".into() },
            AgentSessionEvent::AutoRetryEnd { success: false, attempt: 3, final_error: Some("f".into()) },
            AgentSessionEvent::ThinkingLevelChanged { level: maho_ai::types::ThinkingLevel::High },
            AgentSessionEvent::ServiceTierChanged { tier: Some(ServiceTier::Priority), fast_mode: true },
            AgentSessionEvent::ModelChanged { model: model(), thinking_level: maho_ai::types::ThinkingLevel::Off, source: ModelSelectSource::Cycle },
            AgentSessionEvent::SessionInfoChanged { name: Some("named".into()) },
            AgentSessionEvent::QueueUpdate { steering: vec!["s".into()], follow_up: vec!["f".into()], ordered: vec![QueuedInput { text: "t".into(), mode: StreamingBehavior::Steer, enqueue_order: 7 }] },
            AgentSessionEvent::BashExecutionUpdate { id: Some("b".into()), delta: "out".into() },
        ];
        for event in events {
            assert_roundtrips(&event);
        }
    }

    #[test]
    fn unknown_records_are_ignored() {
        assert_eq!(session_event_from_record(&json!({"type": "not_a_real_event"})), None);
        assert_eq!(session_event_from_record(&json!({"no": "type"})), None);
    }

    fn model() -> maho_ext_api::Model {
        serde_json::from_value(json!({
            "id": "faux-1", "name": "faux-1", "provider": "faux", "api": "faux", "baseUrl": "",
            "reasoning": false, "input": [], "contextWindow": 128000, "maxTokens": 4096,
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}
        })).expect("model")
    }
}
