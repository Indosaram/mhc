use serde_json::{Value, json};
pub static SESSION_SUMMARY_SCHEMA: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
    let mut schema = serde_json::Map::new();
    schema.insert("$session_id".into(), json!({"type":"string"}));
    schema.insert("ended_by".into(), json!({"type":"string","values":["checkpoint","shutdown","reload","recovered"]}));
    for key in ["is_final", "raw_turns_sampled"] { schema.insert(key.into(), json!({"type":"boolean"})); }
    schema.insert("session_role".into(), json!({"type":"string","values":["main","child","dag_child","member"]}));
    for key in ["summary_seq","sample_rate","turns","first_turn_index","last_turn_index","window_ms","total_tokens_p50","total_tokens_p95","total_tokens_max","model_count","other_turns","other_total_tokens","other_cost_usd","input_tokens","output_tokens","cache_read_tokens","cache_write_tokens","reasoning_tokens","total_tokens","cost_usd","tool_result_count","tool_error_count","invalid_usage_turns","error_turns","aborted_turns","custom_model_turns"] { schema.insert(key.into(), json!({"type":"number"})); }
    let providers: Vec<_> = crate::model_vocabulary::KNOWN_PROVIDERS.iter().copied().chain(["custom","none"]).collect();
    let mut models = Vec::new();
    for (_, names) in crate::model_vocabulary::KNOWN_MODELS { for &name in *names { if !models.contains(&name) { models.push(name); } } }
    models.extend(["custom", "none"]);
    for slot in 1..=3 {
        schema.insert(format!("m{slot}_provider"), json!({"type":"string","values":providers}));
        schema.insert(format!("m{slot}_model"), json!({"type":"string","values":models}));
        for key in ["turns","input_tokens","output_tokens","total_tokens","cost_usd"] { schema.insert(format!("m{slot}_{key}"), json!({"type":"number"})); }
    }
    Value::Object(schema)
});
