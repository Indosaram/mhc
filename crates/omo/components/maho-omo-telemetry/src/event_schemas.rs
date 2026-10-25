use crate::model_vocabulary::{KNOWN_MODELS, KNOWN_PROVIDERS};
pub use crate::product_identity::BUILTIN_SKILL_NAMES;
pub use crate::session_role::TELEMETRY_SESSION_ROLES;
/// The handoff-hint actions the builtin producer can publish.
pub const HANDOFF_HINT_ACTIONS: [&str; 3] = ["shown", "accepted", "dismissed"];
pub const EVENT_PROPERTY_ALLOWLISTS: &[(&str, &[&str])] = &[
    ("daily_active", &["$session_id", "day_utc", "reason"]),
    ("session_started", &["$session_id", "$os", "$os_version", "arch", "configured_provider_count", "connected_account_count", "cpu_count", "default_model", "default_provider", "memory_bucket", "model_count", "provider_count", "providers", "reason", "session_role", "signed_in_provider_count", "timezone"]),
    ("prompt_submitted", &["$session_id", "input_source", "invocation_stage", "is_effective_ultrawork_invocation", "is_real_user_prompt", "is_turn_start", "keyword_any", "keyword_occurrence_bucket", "keyword_ultrawork_full", "keyword_ulw_abbrev", "keyword_variant", "prompt_length_bucket", "queue_mode", "real_prompt_ordinal_bucket", "suppression_reason"]),
    ("turn_completed", &["$session_id", "cache_read_tokens", "cache_write_tokens", "cost_1e4", "cost_usd", "input_tokens", "model_id", "output_tokens", "provider", "reasoning_tokens", "sample_rate", "session_role", "total_tokens", "turn_index"]),
    ("category_config", &["$session_id", "builtin_overridden_count", "cat_architect", "cat_artistry", "cat_deep_high", "cat_deep_low", "cat_quick", "cat_ultrabrain", "cat_unspecified_high", "cat_unspecified_low", "cat_visual_engineering", "cat_writing", "combo_fingerprint", "config_generation", "source", "user_category_count"]),
    ("process_crashed", &["$os", "arch", "crashed_bun_version", "crashed_engine_version", "crashed_omo_version", "detection", "exit_code", "process_kind", "shard_kind", "signal", "uptime_bucket", "uptime_ms"]),
    ("kibitzer_summary", &["$session_id", "buffered_cooldown", "buffered_no_new_candidate", "cache_read_tokens", "cache_write_tokens", "candidates_total", "first_nudge_wake", "generations", "input_tokens", "model_top", "nudge_gap_ms_median", "nudge_gap_ms_p90", "nudges_delivered", "offers_total", "output_tokens", "slot_wait_ms_total", "tool_calls_total", "wake_duration_ms_total", "wake_span_ms", "wakes_deadline", "wakes_failed", "wakes_tool_budget", "wakes_total", "wakes_with_nudge"]),
    ("delegation_completed", &["$session_id", "agent_type", "background_mode", "cache_read_tokens", "cache_write_tokens", "category", "config_generation", "cost_status", "cost_usd", "duration_ms", "duration_status", "execution_mode", "fallback_attempts", "input_tokens", "model_id", "model_source", "output_tokens", "owner_kind", "provider", "reasoning_effort", "run_epoch", "start_reason", "stats_status", "status", "task_send_queued_count", "task_send_running_count", "task_seq", "token_status", "tool_calls", "total_tokens", "turns"]),
    ("session_summary", &["$session_id", "cost_1e4", "dropped_events", "ended_by", "is_final", "raw_turns_sampled", "session_role", "summary_epoch", "summary_seq", "sample_rate", "turns", "first_turn_index", "last_turn_index", "window_ms", "total_tokens_p50", "total_tokens_p95", "total_tokens_max", "model_count", "other_turns", "other_total_tokens", "other_cost_usd", "input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens", "reasoning_tokens", "total_tokens", "cost_usd", "tool_result_count", "tool_error_count", "invalid_usage_turns", "error_turns", "aborted_turns", "custom_model_turns", "m1_provider", "m1_model", "m1_turns", "m1_input_tokens", "m1_output_tokens", "m1_total_tokens", "m1_cost_usd", "m2_provider", "m2_model", "m2_turns", "m2_input_tokens", "m2_output_tokens", "m2_total_tokens", "m2_cost_usd", "m3_provider", "m3_model", "m3_turns", "m3_input_tokens", "m3_output_tokens", "m3_total_tokens", "m3_cost_usd"]),
    ("handoff_hint", &["$session_id", "action", "context_tokens", "fresh_usd", "model_id", "next_reply_usd", "provider", "saving_usd", "subscription"]),
    ("skill_loaded", &["$session_id", "skill_name"]),
    ("delegation_started", &["$session_id", "background", "batch_size_bucket", "kind", "name"]),
    ("feature_used", &["$session_id", "feature"]),
];
pub const BUILTIN_CATEGORY_NAMES: &[&str] = &["visual-engineering", "artistry", "ultrabrain", "deep-low", "deep-high", "quick", "unspecified-low", "unspecified-high", "architect", "writing"];
pub fn builtin_category_names()->Vec<&'static str> { BUILTIN_CATEGORY_NAMES.to_vec() }
pub fn curated_agents()->std::collections::BTreeSet<&'static str> {senpi_task::agents::curated_readonly_agent_names()}
pub fn omo_native_event_schemas()->serde_json::Value {
    use serde_json::{Value,json};
    let mut events=serde_json::Map::new();
    for &(event,keys) in EVENT_PROPERTY_ALLOWLISTS {
        let mut properties=serde_json::Map::new();
        for &key in keys {
            let values:Option<Vec<&str>>=match (event,key) {
                (_,"default_provider"|"provider")=>Some(KNOWN_MODELS.iter().map(|(p,_)|*p).chain(["custom"]).collect()),
                (_,"session_role")=>Some(TELEMETRY_SESSION_ROLES.to_vec()),
                (_,"action")=>Some(HANDOFF_HINT_ACTIONS.to_vec()),
                (_,"default_model"|"model_id")=>{let mut models=Vec::new();for (_,names) in KNOWN_MODELS {for &name in *names {if !models.contains(&name) {models.push(name);}}}models.push("custom");Some(models)},
                ("daily_active","reason")=>Some(vec!["session_start"]),
                ("session_started","reason")=>Some(vec!["startup","reload","new","resume","fork"]),
                (_,"memory_bucket")=>Some(vec!["lt_8_gb","8_15_gb","16_31_gb","32_63_gb","64_plus_gb"]),
                (_,"input_source")=>Some(vec!["interactive","rpc","extension"]),
                (_,"invocation_stage")=>Some(vec!["none","first_arm","remention","post_compact_rearm"]),
                (_,"keyword_occurrence_bucket")=>Some(vec!["1","2","3_5","6_plus"]),
                (_,"keyword_variant")=>Some(vec!["none","ulw","ultrawork","both"]),
                (_,"prompt_length_bucket")=>Some(vec!["lt_100","100_500","500_2000","gte_2000"]),
                (_,"queue_mode")=>Some(vec!["immediate","follow_up","steer","other"]),
                (_,"real_prompt_ordinal_bucket")=>Some(vec!["1","2_3","4_10","11_25","26_plus"]),
                (_,"suppression_reason")=>Some(vec!["none","no_keyword","identifier_reference","negated_mention","extension_source","child_session","embedded_directive","skill_expansion","skill_name_only"]),
                (_,"skill_name")=>Some(BUILTIN_SKILL_NAMES.to_vec()),
                (_,"batch_size_bucket")=>Some(vec!["1","2_4","5_plus"]),
                (_,"kind")=>Some(vec!["category","subagent"]),
                (_,"name")=>Some(builtin_category_names().into_iter().chain(curated_agents()).chain(["custom"]).collect()),
                (_,"feature")=>Some(vec!["goal_tool","team_create","memory_tool"]),
                _=>None,
            };
            let property=if let Some(values)=values {json!({"type":"string","values":values})} else {let kind=if key.starts_with("is_") || matches!(key,"keyword_any"|"keyword_ultrawork_full"|"keyword_ulw_abbrev"|"background"|"subscription") {"boolean"} else if key=="connected_account_count" { "string" } else if key.ends_with("_count") || key.ends_with("_tokens") || matches!(key,"cost_usd"|"cost_1e4"|"turn_index"|"sample_rate"|"fresh_usd"|"next_reply_usd"|"saving_usd") {"number"} else {"string"};json!({"type":kind})};
            properties.insert(key.into(),property);
        }
        events.insert(event.into(),Value::Object(properties));
    }
    events.insert("parallelism_summary".into(),Value::Object(crate::parallelism_schema::PARALLELISM_SUMMARY_SCHEMA.iter().map(|(key,kind)|((*key).into(),if *key=="schema_kind" {json!({"type":"string","values":["parallelism_v1","parallelism_v2"]})} else {json!({"type":kind})})).collect()));
    events.insert("category_config".into(), crate::category_config_schema::CATEGORY_CONFIG_SCHEMA.clone());
    events.insert("process_crashed".into(), crate::crash_schema::PROCESS_CRASHED_SCHEMA.clone());
    events.insert("kibitzer_summary".into(), crate::kibitzer_schema::KIBITZER_SUMMARY_SCHEMA.clone());
    events.insert("session_summary".into(), crate::session_summary_schema::SESSION_SUMMARY_SCHEMA.clone());
    let providers:Vec<_> = KNOWN_PROVIDERS.iter().copied().chain(["custom"]).collect();
    let mut models=Vec::new(); for (_,names) in KNOWN_MODELS { for &name in *names {if !models.contains(&name) {models.push(name);}} } models.push("custom");
    events.insert("delegation_completed".into(), crate::delegation_schema::build_delegation_completed_schema(&providers,&models));
    Value::Object(events)
}

pub static OMO_NATIVE_COMMON_PROPERTY_SCHEMAS: std::sync::LazyLock<serde_json::Value> = std::sync::LazyLock::new(|| serde_json::json!({
    "$process_person_profile": {"type":"boolean"},
    "ci": {"type":"boolean"},
    "event_id": {"type":"string"},
    "install_hash": {"type":"string"},
    "machine_hash": {"type":"string"},
    "package_version": {"type":"string"},
    "platform": {"type":"string","values":["omo-senpi"]},
    "product_name": {"type":"string","values":["omo-native"]},
    "schema_version": {"type":"number"},
    "surface": {"type":"string","values":["cli","desktop"]}
}));

pub static OMO_NATIVE_EVENT_SCHEMAS: std::sync::LazyLock<serde_json::Value> = std::sync::LazyLock::new(omo_native_event_schemas);
pub static OMO_NATIVE_PROPERTY_ALLOWLISTS: std::sync::LazyLock<std::collections::BTreeMap<String, Vec<String>>> = std::sync::LazyLock::new(|| {
    OMO_NATIVE_EVENT_SCHEMAS.as_object().into_iter().flat_map(|events| events.iter()).map(|(name, schema)| {
        let keys = schema.as_object().into_iter().flat_map(|properties| properties.keys().cloned()).collect();
        (name.clone(), keys)
    }).collect()
});
