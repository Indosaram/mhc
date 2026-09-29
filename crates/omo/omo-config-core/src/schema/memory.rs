use serde_json::{Value, json};

use crate::internal::validate::{
    Field, Node, boolean, defaulted, enumeration, integer, integer_between, non_empty_string,
    optional, positive_integer, record, strict_object,
};

fn default_reflection_trigger() -> Value {
    json!({ "step_count": 25, "on_compaction": true })
}

fn default_reflection() -> Value {
    json!({
        "enabled": true,
        "trigger": { "step_count": 25, "on_compaction": true },
        "merge": "auto",
        "category": "quick",
        "timeout_minutes": 15,
        "sandbox": "auto",
    })
}

fn default_sync() -> Value {
    json!({ "enabled": true })
}

fn default_search() -> Value {
    json!({ "enabled": true })
}

fn default_nudge() -> Value {
    json!({ "enabled": true, "every_user_turns": 10 })
}

fn default_facts() -> Value {
    json!({ "enabled": true, "debounce_settles": 4 })
}

fn default_dream() -> Value {
    json!({
        "enabled": true,
        "idle_minutes": 30,
        "min_hours_between": 24,
        "shutdown_launch": true,
        "auto_select_max": 5,
        "auto_select_max_chars": 150000,
    })
}

fn default_people() -> Value {
    json!({ "enabled": true, "max_entries": 40, "max_entry_chars": 200 })
}

fn default_soul() -> Value {
    json!({ "edit_notice": true })
}

fn default_true() -> Value {
    json!(true)
}

fn default_auto() -> Value {
    json!("auto")
}

fn default_direct() -> Value {
    json!("direct")
}

fn default_quick() -> Value {
    json!("quick")
}

fn default_compile_warn_tokens() -> Value {
    json!(30000)
}

fn default_empty_object() -> Value {
    json!({})
}

fn optional_field(key: &'static str, node: Node) -> Field {
    optional(key, node)
}

pub fn omo_memory_reflection_trigger_layer_schema() -> Node {
    strict_object(vec![
        optional_field("step_count", integer().with_min(0.0)),
        optional_field("on_compaction", boolean()),
    ])
}

pub fn omo_memory_reflection_trigger_schema() -> Node {
    strict_object(vec![
        defaulted(
            "step_count",
            integer().with_min(0.0),
            default_reflection_trigger_step_count,
        ),
        defaulted("on_compaction", boolean(), default_true),
    ])
}

fn default_reflection_trigger_step_count() -> Value {
    json!(25)
}

pub fn omo_memory_reflection_layer_schema() -> Node {
    strict_object(vec![
        optional_field("enabled", boolean()),
        optional_field("trigger", omo_memory_reflection_trigger_layer_schema()),
        optional_field("merge", enumeration(&["auto", "integration"])),
        optional_field("category", non_empty_string()),
        optional_field("timeout_minutes", positive_integer()),
        optional_field("sandbox", enumeration(&["auto", "required", "off"])),
    ])
}

pub fn omo_memory_reflection_schema() -> Node {
    strict_object(vec![
        defaulted("enabled", boolean(), default_true),
        defaulted(
            "trigger",
            omo_memory_reflection_trigger_schema(),
            default_reflection_trigger,
        ),
        defaulted("merge", enumeration(&["auto", "integration"]), default_auto),
        defaulted("category", non_empty_string(), default_quick),
        defaulted("timeout_minutes", positive_integer(), || json!(15)),
        defaulted(
            "sandbox",
            enumeration(&["auto", "required", "off"]),
            default_auto,
        ),
    ])
}

pub fn omo_memory_sync_layer_schema() -> Node {
    strict_object(vec![
        optional_field("remote", non_empty_string()),
        optional_field("enabled", boolean()),
    ])
}

pub fn omo_memory_sync_schema() -> Node {
    strict_object(vec![
        optional("remote", non_empty_string()),
        defaulted("enabled", boolean(), default_true),
    ])
}

pub fn omo_memory_search_layer_schema() -> Node {
    strict_object(vec![optional_field("enabled", boolean())])
}

pub fn omo_memory_search_schema() -> Node {
    strict_object(vec![defaulted("enabled", boolean(), default_true)])
}

pub fn omo_memory_nudge_layer_schema() -> Node {
    strict_object(vec![
        optional_field("enabled", boolean()),
        optional_field("every_user_turns", integer().with_min(1.0)),
    ])
}

pub fn omo_memory_nudge_schema() -> Node {
    strict_object(vec![
        defaulted("enabled", boolean(), default_true),
        defaulted("every_user_turns", integer().with_min(1.0), || json!(10)),
    ])
}

pub fn omo_memory_facts_layer_schema() -> Node {
    strict_object(vec![
        optional_field("enabled", boolean()),
        optional_field("debounce_settles", integer().with_min(1.0)),
    ])
}

pub fn omo_memory_facts_schema() -> Node {
    strict_object(vec![
        defaulted("enabled", boolean(), default_true),
        defaulted("debounce_settles", integer().with_min(1.0), || json!(4)),
    ])
}

pub fn omo_memory_dream_layer_schema() -> Node {
    strict_object(vec![
        optional_field("enabled", boolean()),
        optional_field("idle_minutes", integer().with_min(0.0)),
        optional_field("min_hours_between", integer().with_min(1.0)),
        optional_field("shutdown_launch", boolean()),
        optional_field("auto_select_max", integer_between(1, 10)),
        optional_field("auto_select_max_chars", integer().with_min(10000.0)),
    ])
}

pub fn omo_memory_dream_schema() -> Node {
    strict_object(vec![
        defaulted("enabled", boolean(), default_true),
        defaulted("idle_minutes", integer().with_min(0.0), || json!(30)),
        defaulted("min_hours_between", integer().with_min(1.0), || json!(24)),
        defaulted("shutdown_launch", boolean(), default_true),
        defaulted("auto_select_max", integer_between(1, 10), || json!(5)),
        defaulted("auto_select_max_chars", integer().with_min(10000.0), || {
            json!(150000)
        }),
    ])
}

pub fn omo_memory_people_layer_schema() -> Node {
    strict_object(vec![
        optional_field("enabled", boolean()),
        optional_field("max_entries", integer_between(1, 100)),
        optional_field("max_entry_chars", integer_between(50, 500)),
    ])
}

pub fn omo_memory_people_schema() -> Node {
    strict_object(vec![
        defaulted("enabled", boolean(), default_true),
        defaulted("max_entries", integer_between(1, 100), || json!(40)),
        defaulted("max_entry_chars", integer_between(50, 500), || json!(200)),
    ])
}

pub fn omo_memory_soul_layer_schema() -> Node {
    strict_object(vec![optional_field("edit_notice", boolean())])
}

pub fn omo_memory_soul_schema() -> Node {
    strict_object(vec![defaulted("edit_notice", boolean(), default_true)])
}

pub fn omo_memory_agent_overrides_schema() -> Node {
    strict_object(vec![
        optional_field("enabled", boolean()),
        optional_field("agent", non_empty_string()),
        optional_field("reflection", omo_memory_reflection_layer_schema()),
        optional_field("nudge", omo_memory_nudge_layer_schema()),
        optional_field("facts", omo_memory_facts_layer_schema()),
        optional_field("dream", omo_memory_dream_layer_schema()),
        optional_field("people", omo_memory_people_layer_schema()),
        optional_field("soul", omo_memory_soul_layer_schema()),
        optional_field("sync", omo_memory_sync_layer_schema()),
        optional_field("search", omo_memory_search_layer_schema()),
        optional_field("compile_warn_tokens", positive_integer()),
    ])
}

pub fn omo_memory_settings_schema() -> Node {
    strict_object(vec![
        defaulted("enabled", boolean(), default_true),
        defaulted("agent", non_empty_string(), default_auto),
        defaulted(
            "tool_exposure",
            enumeration(&["direct", "search"]),
            default_direct,
        ),
        defaulted(
            "reflection",
            omo_memory_reflection_schema(),
            default_reflection,
        ),
        defaulted("nudge", omo_memory_nudge_schema(), default_nudge),
        defaulted("facts", omo_memory_facts_schema(), default_facts),
        defaulted("dream", omo_memory_dream_schema(), default_dream),
        defaulted("people", omo_memory_people_schema(), default_people),
        defaulted("soul", omo_memory_soul_schema(), default_soul),
        defaulted("sync", omo_memory_sync_schema(), default_sync),
        defaulted("search", omo_memory_search_schema(), default_search),
        defaulted(
            "compile_warn_tokens",
            positive_integer(),
            default_compile_warn_tokens,
        ),
        defaulted(
            "agents",
            record(omo_memory_agent_overrides_schema()),
            default_empty_object,
        ),
    ])
}

pub fn omo_memory_settings_layer_schema() -> Node {
    strict_object(vec![
        optional_field("enabled", boolean()),
        optional_field("agent", non_empty_string()),
        optional_field("tool_exposure", enumeration(&["direct", "search"])),
        optional_field("reflection", omo_memory_reflection_layer_schema()),
        optional_field("nudge", omo_memory_nudge_layer_schema()),
        optional_field("facts", omo_memory_facts_layer_schema()),
        optional_field("dream", omo_memory_dream_layer_schema()),
        optional_field("people", omo_memory_people_layer_schema()),
        optional_field("soul", omo_memory_soul_layer_schema()),
        optional_field("sync", omo_memory_sync_layer_schema()),
        optional_field("search", omo_memory_search_layer_schema()),
        optional_field("compile_warn_tokens", positive_integer()),
        optional_field("agents", record(omo_memory_agent_overrides_schema())),
    ])
}
