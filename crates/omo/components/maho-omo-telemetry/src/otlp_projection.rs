//! Port of `packages/omo-senpi/src/components/telemetry/otlp-projection.ts`.
//!
//! Derived from omo-telemetry origin/main 13c6e61b40c49141868e7430e02a7b4b949d0c5d:
//! `packages/schema-registry/artifacts/omo-senpi.json` (schema v4, proto v1.7.0) and `semconv-map.json`.
//! No unlisted event or attribute has a fallback. For registered semconv keys the map's input name is used:
//! `omo.prompt.is_real_user` comes from `is_real_user_prompt`.

use std::collections::HashMap;

use serde_json::{Map, Value};
use telemetry_core::{OtlpWireProjection, SpanAllowlist, WireAttributes};

pub use crate::otlp_events::is_otlp_event;
use crate::pinned_model_ids::pinned_model_id;

type Fields = &'static [(&'static str, &'static str)];

const EVENT_FIELDS: &[(&str, Fields)] = &[
    (
        "category_config",
        &[
            ("builtin_overridden_count", "omo.builtin_overridden_count"),
            ("cat_architect", "omo.cat_architect"),
            ("cat_artistry", "omo.cat_artistry"),
            ("cat_deep_high", "omo.cat_deep_high"),
            ("cat_deep_low", "omo.cat_deep_low"),
            ("cat_quick", "omo.cat_quick"),
            ("cat_ultrabrain", "omo.cat_ultrabrain"),
            ("cat_unspecified_high", "omo.cat_unspecified_high"),
            ("cat_unspecified_low", "omo.cat_unspecified_low"),
            ("cat_visual_engineering", "omo.cat_visual_engineering"),
            ("cat_writing", "omo.cat_writing"),
            ("combo_fingerprint", "omo.combo_fingerprint"),
            ("config_generation", "omo.config_generation"),
            ("user_category_count", "omo.custom_category_count"),
            ("source", "omo.source"),
            ("$session_id", "session.id"),
        ],
    ),
    (
        "computer_use_activation",
        &[
            ("active", "omo.active"),
            ("backend", "omo.backend"),
            ("host_platform", "omo.host_platform"),
            ("source", "omo.source"),
            ("$session_id", "session.id"),
        ],
    ),
    (
        "computer_use_engine_error",
        &[
            ("backend", "omo.backend"),
            ("code", "omo.code"),
            ("host_platform", "omo.host_platform"),
            ("$session_id", "session.id"),
        ],
    ),
    (
        "computer_use_permission_denied",
        &[
            ("backend", "omo.backend"),
            ("host_platform", "omo.host_platform"),
            ("permission", "omo.permission"),
            ("scope", "omo.scope"),
            ("$session_id", "session.id"),
        ],
    ),
    (
        "daily_active",
        &[
            ("day_utc", "omo.day_utc"),
            ("reason", "omo.start_trigger"),
            ("$session_id", "session.id"),
        ],
    ),
    (
        "delegation_completed",
        &[
            ("agent_type", "omo.agent_type"),
            ("background_mode", "omo.background_mode"),
            ("cache_read_tokens", "omo.cache_read_tokens"),
            ("cache_write_tokens", "omo.cache_write_tokens"),
            ("category", "omo.category"),
            ("config_generation", "omo.config_generation"),
            ("cost_status", "omo.cost_status"),
            ("cost_usd", "omo.cost_usd"),
            ("duration_ms", "omo.duration_ms"),
            ("duration_status", "omo.duration_status"),
            ("execution_mode", "omo.execution_mode"),
            ("fallback_attempts", "omo.fallback_attempts"),
            ("input_tokens", "omo.input_tokens"),
            ("model_id", "omo.model_id"),
            ("model_source", "omo.model_source"),
            ("output_tokens", "omo.output_tokens"),
            ("owner_kind", "omo.owner_kind"),
            ("provider", "omo.provider"),
            ("reasoning_effort", "omo.reasoning_effort"),
            ("run_epoch", "omo.run_epoch"),
            ("start_reason", "omo.start_trigger"),
            ("stats_status", "omo.stats_status"),
            ("status", "omo.status"),
            ("task_send_queued_count", "omo.task_send_queued_count"),
            ("task_send_running_count", "omo.task_send_running_count"),
            ("task_seq", "omo.task_seq"),
            ("token_status", "omo.token_status"),
            ("tool_calls", "omo.tool_calls"),
            ("total_tokens", "omo.total_tokens"),
            ("turns", "omo.turns"),
            ("$session_id", "session.id"),
        ],
    ),
    (
        "delegation_started",
        &[
            ("background", "omo.background"),
            ("batch_size_bucket", "omo.batch_size_bucket"),
            ("kind", "omo.kind"),
            ("name", "omo.name"),
            ("$session_id", "session.id"),
        ],
    ),
    (
        "feature_used",
        &[("feature", "omo.feature"), ("$session_id", "session.id")],
    ),
    (
        "handoff_hint",
        &[
            ("action", "omo.action"),
            ("context_tokens", "omo.context_tokens"),
            ("fresh_usd", "omo.fresh_usd"),
            ("model_id", "omo.model_id"),
            ("next_reply_usd", "omo.next_reply_usd"),
            ("provider", "omo.provider"),
            ("saving_usd", "omo.saving_usd"),
            ("subscription", "omo.subscription"),
            ("$session_id", "session.id"),
        ],
    ),
    (
        "kibitzer_summary",
        &[
            ("buffered_cooldown", "omo.buffered_cooldown"),
            ("buffered_no_new_candidate", "omo.buffered_no_new_candidate"),
            ("cache_read_tokens", "omo.cache_read_tokens"),
            ("cache_write_tokens", "omo.cache_write_tokens"),
            ("candidates_total", "omo.candidates_total"),
            ("first_nudge_wake", "omo.first_nudge_wake"),
            ("generations", "omo.generations"),
            ("input_tokens", "omo.input_tokens"),
            ("model_top", "omo.model_top"),
            ("nudge_gap_ms_median", "omo.nudge_gap_ms_median"),
            ("nudge_gap_ms_p90", "omo.nudge_gap_ms_p90"),
            ("nudges_delivered", "omo.nudges_delivered"),
            ("offers_total", "omo.offers_total"),
            ("output_tokens", "omo.output_tokens"),
            ("slot_wait_ms_total", "omo.slot_wait_ms_total"),
            ("tool_calls_total", "omo.tool_calls_total"),
            ("wake_duration_ms_total", "omo.wake_duration_ms_total"),
            ("wake_span_ms", "omo.wake_span_ms"),
            ("wakes_deadline", "omo.wakes_deadline"),
            ("wakes_failed", "omo.wakes_failed"),
            ("wakes_tool_budget", "omo.wakes_tool_budget"),
            ("wakes_total", "omo.wakes_total"),
            ("wakes_with_nudge", "omo.wakes_with_nudge"),
            ("$session_id", "session.id"),
        ],
    ),
    (
        "parallelism_summary",
        &[
            ("clock_anomalies", "omo.clock_anomalies"),
            ("dropped_calls", "omo.dropped_calls"),
            (
                "eval_execution_detached_count",
                "omo.eval_execution_detached_count",
            ),
            (
                "eval_execution_event_bus_available",
                "omo.eval_execution_event_bus_available",
            ),
            (
                "eval_execution_event_count",
                "omo.eval_execution_event_count",
            ),
            (
                "eval_execution_event_rejected_count",
                "omo.eval_execution_event_rejected_count",
            ),
            ("eval_execution_ok_count", "omo.eval_execution_ok_count"),
            (
                "eval_nested_tool_call_count",
                "omo.eval_nested_tool_call_count",
            ),
            (
                "eval_nested_tool_call_error_count",
                "omo.eval_nested_tool_call_error_count",
            ),
            (
                "eval_nested_tool_call_ok_count",
                "omo.eval_nested_tool_call_ok_count",
            ),
            (
                "eval_nested_tool_call_pending_count",
                "omo.eval_nested_tool_call_pending_count",
            ),
            ("eval_only_duration_ms", "omo.eval_only_duration_ms"),
            ("eval_only_waves", "omo.eval_only_waves"),
            ("eval_outer_joined_calls", "omo.eval_outer_joined_calls"),
            (
                "eval_tool_aggregate_truncated_execution_count",
                "omo.eval_tool_aggregate_truncated_execution_count",
            ),
            ("incomplete_calls", "omo.incomplete_calls"),
            (
                "measured_eval_execution_duration_ms_sum",
                "omo.measured_eval_execution_duration_ms_sum",
            ),
            (
                "measured_eval_nested_tool_duration_ms_sum",
                "omo.measured_eval_nested_tool_duration_ms_sum",
            ),
            (
                "measured_turn_duration_ms_total",
                "omo.measured_turn_duration_ms_total",
            ),
            (
                "mixed_non_eval_joined_calls",
                "omo.mixed_non_eval_joined_calls",
            ),
            ("mixed_waves", "omo.mixed_waves"),
            (
                "modeled_wallclock_saved_ms",
                "omo.modeled_wallclock_saved_ms",
            ),
            ("non_eval_joined_calls", "omo.non_eval_joined_calls"),
            (
                "non_eval_saved_round_trips",
                "omo.non_eval_saved_round_trips",
            ),
            ("non_eval_waves_multi", "omo.non_eval_waves_multi"),
            ("non_eval_waves_total", "omo.non_eval_waves_total"),
            ("schema_kind", "omo.schema_kind"),
            ("upper_bound_saved_ms", "omo.upper_bound_saved_ms"),
            ("$session_id", "session.id"),
        ],
    ),
    (
        "process_crashed",
        &[
            ("crashed_bun_version", "omo.crashed_bun_version"),
            ("crashed_engine_version", "omo.crashed_engine_version"),
            ("crashed_omo_version", "omo.crashed_omo_version"),
            ("detection", "omo.detection"),
            ("process_kind", "omo.process_kind"),
            ("shard_kind", "omo.shard_kind"),
            ("signal", "omo.signal"),
            ("uptime_bucket", "omo.uptime_bucket"),
            ("uptime_ms", "omo.uptime_ms"),
            ("exit_code", "process.exit.code"),
        ],
    ),
    (
        "prompt_submitted",
        &[
            ("input_source", "omo.input_source"),
            ("invocation_stage", "omo.invocation_stage"),
            (
                "is_effective_ultrawork_invocation",
                "omo.is_effective_ultrawork_invocation",
            ),
            ("is_turn_start", "omo.is_turn_start"),
            ("keyword_any", "omo.keyword_any"),
            ("keyword_occurrence_bucket", "omo.keyword_occurrence_bucket"),
            ("keyword_ultrawork_full", "omo.keyword_ultrawork_full"),
            ("keyword_ulw_abbrev", "omo.keyword_ulw_abbrev"),
            ("keyword_variant", "omo.keyword_variant"),
            ("is_real_user_prompt", "omo.prompt.is_real_user"),
            ("prompt_length_bucket", "omo.prompt_length_bucket"),
            ("queue_mode", "omo.queue_mode"),
            (
                "real_prompt_ordinal_bucket",
                "omo.real_prompt_ordinal_bucket",
            ),
            ("suppression_reason", "omo.suppression_cause"),
            ("$session_id", "session.id"),
        ],
    ),
    (
        "session_started",
        &[
            ("configured_provider_count", "omo.configured_provider_count"),
            ("cpu_count", "omo.cpu_count"),
            ("default_model", "omo.default_model"),
            ("default_provider", "omo.default_provider"),
            ("memory_bucket", "omo.memory_bucket"),
            ("model_count", "omo.model_count"),
            ("provider_count", "omo.provider_count"),
            ("session_role", "omo.session.role"),
            ("signed_in_provider_count", "omo.signed_in_provider_count"),
            ("reason", "omo.start_trigger"),
            ("$session_id", "session.id"),
        ],
    ),
    (
        "session_summary",
        &[
            ("aborted_turns", "omo.aborted_turns"),
            ("cache_read_tokens", "omo.cache_read_tokens"),
            ("cache_write_tokens", "omo.cache_write_tokens"),
            ("custom_model_turns", "omo.custom_model_turns"),
            ("dropped_events", "omo.dropped_events"),
            ("ended_by", "omo.ended_by"),
            ("error_turns", "omo.error_turns"),
            ("first_turn_index", "omo.first_turn_index"),
            ("input_tokens", "omo.input_tokens"),
            ("invalid_usage_turns", "omo.invalid_usage_turns"),
            ("is_final", "omo.is_final"),
            ("last_turn_index", "omo.last_turn_index"),
            ("m1_cost_usd", "omo.m1_cost_usd"),
            ("m1_input_tokens", "omo.m1_input_tokens"),
            ("m1_model", "omo.m1_model"),
            ("m1_output_tokens", "omo.m1_output_tokens"),
            ("m1_provider", "omo.m1_provider"),
            ("m1_total_tokens", "omo.m1_total_tokens"),
            ("m1_turns", "omo.m1_turns"),
            ("m2_cost_usd", "omo.m2_cost_usd"),
            ("m2_input_tokens", "omo.m2_input_tokens"),
            ("m2_model", "omo.m2_model"),
            ("m2_output_tokens", "omo.m2_output_tokens"),
            ("m2_provider", "omo.m2_provider"),
            ("m2_total_tokens", "omo.m2_total_tokens"),
            ("m2_turns", "omo.m2_turns"),
            ("m3_cost_usd", "omo.m3_cost_usd"),
            ("m3_input_tokens", "omo.m3_input_tokens"),
            ("m3_model", "omo.m3_model"),
            ("m3_output_tokens", "omo.m3_output_tokens"),
            ("m3_provider", "omo.m3_provider"),
            ("m3_total_tokens", "omo.m3_total_tokens"),
            ("m3_turns", "omo.m3_turns"),
            ("model_count", "omo.model_count"),
            ("other_cost_usd", "omo.other_cost_usd"),
            ("other_total_tokens", "omo.other_total_tokens"),
            ("other_turns", "omo.other_turns"),
            ("output_tokens", "omo.output_tokens"),
            ("raw_turns_sampled", "omo.raw_turns_sampled"),
            ("reasoning_tokens", "omo.reasoning_tokens"),
            ("sample_rate", "omo.sample_rate"),
            ("session_role", "omo.session.role"),
            ("summary_epoch", "omo.summary_epoch"),
            ("summary_seq", "omo.summary_seq"),
            ("tool_error_count", "omo.tool_error_count"),
            ("tool_result_count", "omo.tool_result_count"),
            ("total_tokens", "omo.total_tokens"),
            ("total_tokens_max", "omo.total_tokens_max"),
            ("total_tokens_p50", "omo.total_tokens_p50"),
            ("total_tokens_p95", "omo.total_tokens_p95"),
            ("turns", "omo.turns"),
            ("window_ms", "omo.window_ms"),
            ("$session_id", "session.id"),
        ],
    ),
    (
        "skill_loaded",
        &[
            ("skill_name", "omo.skill_name"),
            ("$session_id", "session.id"),
        ],
    ),
    (
        "turn_completed",
        &[
            ("request_model_id", "gen_ai.request.model"),
            ("response_model_id", "gen_ai.response.model"),
            (
                "cache_write_tokens",
                "gen_ai.usage.cache_creation.input_tokens",
            ),
            ("cache_read_tokens", "gen_ai.usage.cache_read.input_tokens"),
            ("input_tokens", "gen_ai.usage.input_tokens"),
            ("output_tokens", "gen_ai.usage.output_tokens"),
            ("reasoning_tokens", "gen_ai.usage.reasoning.output_tokens"),
            ("model_id", "omo.model_id"),
            ("provider", "omo.provider"),
            ("sample_rate", "omo.sample_rate"),
            ("session_role", "omo.session.role"),
            ("turn_index", "omo.turn.index"),
            ("total_tokens", "omo.usage.total_tokens"),
            ("$session_id", "session.id"),
            ("cost_1e4", "omo.cost.usd_1e4"),
        ],
    ),
];

fn event_fields(event: &str) -> Fields {
    EVENT_FIELDS
        .iter()
        .find(|(name, _)| *name == event)
        .map_or(&[], |(_, fields)| *fields)
}

/// The `omo.turn` span allowlist.
pub fn otlp_spans() -> SpanAllowlist {
    HashMap::from([(
        "omo.turn".to_string(),
        [
            "gen_ai.operation.name",
            "gen_ai.provider.name",
            "gen_ai.request.model",
            "gen_ai.response.model",
            "omo.model_id",
            "omo.provider",
            "omo.session.role",
            "omo.turn.index",
            "session.id",
        ]
        .map(String::from)
        .to_vec(),
    )])
}

const MODEL_WIRE_KEYS: [&str; 15] = [
    "gen_ai.request.model",
    "gen_ai.response.model",
    "omo.cat_architect",
    "omo.cat_artistry",
    "omo.cat_deep_high",
    "omo.cat_deep_low",
    "omo.cat_quick",
    "omo.cat_ultrabrain",
    "omo.cat_unspecified_high",
    "omo.cat_unspecified_low",
    "omo.cat_visual_engineering",
    "omo.cat_writing",
    "omo.default_model",
    "omo.model_id",
    "omo.model_top",
];

/// session_summary's top-model keys use the same vocabulary plus "none" (no model in that rank).
const RANKED_MODEL_WIRE_KEYS: [&str; 3] = ["omo.m1_model", "omo.m2_model", "omo.m3_model"];

const PROVIDER_VALUES: [(&str, &str); 28] = [
    ("alibaba-token-plan", "alibaba-token-plan"),
    ("alibaba-token-plan-cn", "alibaba-token-plan-cn"),
    ("anthropic", "anthropic"),
    ("anthropic-api", "anthropic-api"),
    ("anthropic-subscription", "anthropic-subscription"),
    ("bailian-coding-plan", "bailian-coding-plan"),
    ("chatgpt-subscription", "chatgpt-subscription"),
    ("claude-sdk-oauth", "claude-sdk-oauth"),
    ("deepseek", "deepseek"),
    ("google", "gcp.gemini"),
    ("google-vertex", "gcp.vertex_ai"),
    ("github-copilot", "github-copilot"),
    ("kimi-coding", "kimi-coding"),
    ("kimi-for-coding", "kimi-for-coding"),
    ("moonshotai", "moonshotai"),
    ("openai", "openai"),
    ("openai-codex", "openai-codex"),
    ("opencode", "opencode"),
    ("opencode-go", "opencode-go"),
    ("qwen-token-plan", "qwen-token-plan"),
    ("qwen-token-plan-cn", "qwen-token-plan-cn"),
    ("vercel", "vercel"),
    ("xai", "x_ai"),
    ("xiaomi", "xiaomi"),
    ("zai", "zai"),
    ("zai-coding-cn", "zai-coding-cn"),
    ("zai-coding-plan", "zai-coding-plan"),
    ("custom", "custom"),
];

/// Optional aliases use the collector pin's closed vocabulary, not future client additions.
fn start_triggers(event: &str) -> &'static [&'static str] {
    match event {
        "daily_active" => &["session_start"],
        "session_started" => &["startup", "reload", "new", "resume", "fork"],
        "delegation_completed" => &[
            "initial_spawn",
            "runtime_fallback",
            "session_resume",
            "dag_retry",
            "revive_after_completed",
            "revive_after_error",
            "revive_after_cancelled",
            "revive_after_interrupted",
            "revive_after_lost",
            "unknown",
        ],
        _ => &[],
    }
}

const SUPPRESSION_CAUSES: [&str; 9] = [
    "none",
    "no_keyword",
    "identifier_reference",
    "negated_mention",
    "extension_source",
    "child_session",
    "embedded_directive",
    "skill_expansion",
    "skill_name_only",
];

fn finite(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
}

/// A JS number: integral values stay integers on the wire.
fn number(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 9.0e15 {
        Value::from(value as i64)
    } else {
        Value::from(value)
    }
}

pub fn project_otlp_event(event: &str, properties: &Map<String, Value>) -> WireAttributes {
    let mut attributes = WireAttributes::new();
    if !is_otlp_event(event) {
        return attributes;
    }
    for (property, wire) in event_fields(event) {
        let value = properties.get(*property);
        if *wire == "omo.start_trigger"
            && !value
                .and_then(Value::as_str)
                .is_some_and(|v| start_triggers(event).contains(&v))
        {
            continue;
        }
        if *wire == "omo.suppression_cause"
            && !value
                .and_then(Value::as_str)
                .is_some_and(|v| SUPPRESSION_CAUSES.contains(&v))
        {
            continue;
        }
        if *wire == "omo.custom_category_count"
            && !value
                .and_then(Value::as_f64)
                .is_some_and(|v| (0.0..=1e9).contains(&v))
        {
            continue;
        }
        match value {
            Some(Value::String(text)) if MODEL_WIRE_KEYS.contains(wire) => {
                attributes.insert((*wire).into(), pinned_model_id(text).into());
            }
            Some(Value::String(text)) if RANKED_MODEL_WIRE_KEYS.contains(wire) => {
                let kept = if text == "none" {
                    text.as_str()
                } else {
                    pinned_model_id(text)
                };
                attributes.insert((*wire).into(), kept.into());
            }
            Some(kept @ (Value::String(_) | Value::Bool(_))) => {
                attributes.insert((*wire).into(), kept.clone());
            }
            Some(kept @ Value::Number(_)) if finite(Some(kept)).is_some() => {
                attributes.insert((*wire).into(), kept.clone());
            }
            _ => {}
        }
    }
    if event == "session_summary"
        && let Some(usd) = finite(properties.get("cost_usd"))
        && (0.0..=1e6).contains(&usd)
    {
        attributes.insert(
            "omo.cost.usd_1e4".into(),
            Value::from((usd * 10_000.0).round() as i64),
        );
    }
    if event == "turn_completed" {
        if let Some(input) = finite(properties.get("input_tokens")) {
            let inclusive = input
                + finite(properties.get("cache_read_tokens")).unwrap_or(0.0)
                + finite(properties.get("cache_write_tokens")).unwrap_or(0.0);
            if inclusive.is_finite() {
                attributes.insert("gen_ai.usage.input_tokens".into(), number(inclusive));
            }
        }
        if let Some(provider) = properties.get("provider").and_then(Value::as_str)
            && let Some((_, name)) = PROVIDER_VALUES.iter().find(|(key, _)| *key == provider)
        {
            attributes.insert("gen_ai.provider.name".into(), (*name).into());
        }
    }
    attributes
}

/// `projectOtlpEvent` as the transport input's projection callback.
pub fn otlp_projection() -> OtlpWireProjection {
    std::sync::Arc::new(|event, properties| project_otlp_event(event, properties))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn props(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn maps_properties_to_wire_keys_and_drops_out_of_pin_values() {
        let out = project_otlp_event(
            "session_started",
            &props(
                json!({"reason": "startup", "$session_id": "h", "default_model": "my-finetune", "cpu_count": 8}),
            ),
        );
        assert_eq!(out["omo.start_trigger"], "startup");
        assert_eq!(out["session.id"], "h");
        assert_eq!(out["omo.default_model"], "custom");
        assert_eq!(out["omo.cpu_count"], 8);
        let out = project_otlp_event("daily_active", &props(json!({"reason": "bogus"})));
        assert!(!out.contains_key("omo.start_trigger"));
        let out = project_otlp_event(
            "category_config",
            &props(json!({"user_category_count": -1})),
        );
        assert!(!out.contains_key("omo.custom_category_count"));
        assert!(project_otlp_event("unlisted", &props(json!({"a": 1}))).is_empty());
    }

    #[test]
    fn ranked_models_keep_none_and_summary_cost_scales() {
        let out = project_otlp_event(
            "session_summary",
            &props(json!({"m1_model": "none", "m2_model": "unknown-x", "cost_usd": 1.23456})),
        );
        assert_eq!(out["omo.m1_model"], "none");
        assert_eq!(out["omo.m2_model"], "custom");
        assert_eq!(out["omo.cost.usd_1e4"], 12346);
    }

    #[test]
    fn turn_completed_input_tokens_include_cache_and_provider_is_mapped() {
        let out = project_otlp_event(
            "turn_completed",
            &props(
                json!({"input_tokens": 10, "cache_read_tokens": 5, "cache_write_tokens": 2, "provider": "google"}),
            ),
        );
        assert_eq!(out["gen_ai.usage.input_tokens"], 17);
        assert_eq!(out["gen_ai.provider.name"], "gcp.gemini");
    }
}
