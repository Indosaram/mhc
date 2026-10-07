use serde_json::{Value, json};

use crate::internal::validate::{
    Node, boolean, defaulted, enumeration, integer_between, literal, nonnegative_integer, optional,
    positive_integer, record, strict_object, string, union,
};
use crate::issue::Issues;

fn default_wait() -> Value {
    json!({ "min_ms": 5000, "default_ms": 60000, "max_ms": 600000 })
}

fn default_team() -> Value {
    json!({ "max_members": 8, "max_parallel_members": 4, "max_wall_clock_minutes": 120 })
}

fn default_warnings() -> Value {
    json!({ "unavailable_categories": true })
}

pub const ISOLATION_BACKEND_KINDS: [&str; 8] = [
    "auto",
    "apfs",
    "btrfs",
    "zfs",
    "reflink",
    "overlayfs",
    "block-clone",
    "rcopy",
];

fn default_isolation() -> Value {
    json!({ "enabled": false, "backend": "auto", "apply": true, "merge": "patch", "commits": "generic" })
}

fn default_execution_mode() -> Value {
    json!("in-process")
}

fn default_concurrency() -> Value {
    json!(5)
}

fn default_max_depth() -> Value {
    json!(1)
}

fn default_residency_max_children() -> Value {
    json!(8)
}

fn default_ttl_ms() -> Value {
    json!(86400000)
}

fn default_resume_children() -> Value {
    json!(true)
}

fn default_true() -> Value {
    json!(true)
}

fn default_dag_max_nodes_per_run() -> Value {
    json!(64)
}

fn default_dag_max_runs_per_session() -> Value {
    json!(16)
}

fn default_dag_subscriber_ring() -> Value {
    json!(1000)
}

fn default_dag_heartbeat_ms() -> Value {
    json!(15000)
}

fn default_dag_history_default_limit() -> Value {
    json!(256)
}

fn default_dag_history_max_limit() -> Value {
    json!(1000)
}

fn default_dag_retention_days() -> Value {
    json!(7)
}

fn default_dag_max_prompt_bytes() -> Value {
    json!(262144)
}

pub fn residency_max_children_schema() -> Node {
    union(vec![positive_integer(), literal("unlimited")])
}

pub fn omo_task_wait_schema() -> Node {
    strict_object(vec![
        defaulted("min_ms", positive_integer(), || json!(5000)),
        defaulted("default_ms", positive_integer(), || json!(60000)),
        defaulted("max_ms", positive_integer(), || json!(600000)),
    ])
}

pub fn omo_task_team_settings_schema() -> Node {
    strict_object(vec![
        defaulted("max_members", integer_between(1, 8), || json!(8)),
        defaulted("max_parallel_members", integer_between(1, 8), || json!(4)),
        defaulted("max_wall_clock_minutes", positive_integer(), || json!(120)),
    ])
}

pub fn omo_task_warnings_schema() -> Node {
    strict_object(vec![defaulted(
        "unavailable_categories",
        boolean(),
        default_true,
    )])
}

pub fn omo_task_dag_settings_schema() -> Node {
    strict_object(vec![
        defaulted(
            "max_nodes_per_run",
            positive_integer(),
            default_dag_max_nodes_per_run,
        ),
        defaulted(
            "max_runs_per_session",
            positive_integer(),
            default_dag_max_runs_per_session,
        ),
        defaulted(
            "subscriber_ring",
            positive_integer(),
            default_dag_subscriber_ring,
        ),
        defaulted("heartbeat_ms", positive_integer(), default_dag_heartbeat_ms),
        defaulted(
            "history_default_limit",
            positive_integer(),
            default_dag_history_default_limit,
        ),
        defaulted(
            "history_max_limit",
            positive_integer(),
            default_dag_history_max_limit,
        ),
        defaulted(
            "retention_days",
            positive_integer(),
            default_dag_retention_days,
        ),
        defaulted(
            "max_prompt_bytes",
            positive_integer(),
            default_dag_max_prompt_bytes,
        ),
    ])
}

pub fn isolation_backend_kind_schema() -> Node {
    enumeration(&ISOLATION_BACKEND_KINDS)
}

pub fn omo_task_isolation_schema() -> Node {
    strict_object(vec![
        defaulted("enabled", boolean(), || json!(false)),
        defaulted("backend", isolation_backend_kind_schema(), || json!("auto")),
        defaulted("apply", boolean(), default_true),
        defaulted("merge", enumeration(&["patch", "branch"]), || {
            json!("patch")
        }),
        defaulted("commits", enumeration(&["generic", "ai"]), || {
            json!("generic")
        }),
    ])
}

pub fn omo_task_isolation_layer_schema() -> Node {
    strict_object(vec![
        optional("enabled", boolean()),
        optional("backend", isolation_backend_kind_schema()),
        optional("apply", boolean()),
        optional("merge", enumeration(&["patch", "branch"])),
        optional("commits", enumeration(&["generic", "ai"])),
    ])
}

pub fn omo_task_settings_schema() -> Node {
    strict_object(vec![
        defaulted(
            "isolation",
            omo_task_isolation_schema(),
            default_isolation,
        ),
        defaulted(
            "default_execution_mode",
            enumeration(&["in-process", "process"]),
            default_execution_mode,
        ),
        defaulted(
            "default_concurrency",
            positive_integer(),
            default_concurrency,
        ),
        optional("provider_concurrency", record(positive_integer())),
        optional("model_concurrency", record(positive_integer())),
        defaulted("max_depth", nonnegative_integer(), default_max_depth),
        defaulted(
            "residency_max_children",
            residency_max_children_schema(),
            default_residency_max_children,
        ),
        defaulted("ttl_ms", positive_integer(), default_ttl_ms),
        optional("state_dir", string()),
        optional("reattach_on_reconcile", boolean()),
        defaulted("resume_children", boolean(), default_resume_children),
        defaulted("warnings", omo_task_warnings_schema(), default_warnings),
        defaulted("wait", omo_task_wait_schema(), default_wait),
        defaulted("team", omo_task_team_settings_schema(), default_team),
        optional("dag", omo_task_dag_settings_schema()),
    ])
}

pub fn omo_task_dag_settings_layer_schema() -> Node {
    strict_object(vec![
        optional("max_nodes_per_run", positive_integer()),
        optional("max_runs_per_session", positive_integer()),
        optional("subscriber_ring", positive_integer()),
        optional("heartbeat_ms", positive_integer()),
        optional("history_default_limit", positive_integer()),
        optional("history_max_limit", positive_integer()),
        optional("retention_days", positive_integer()),
        optional("max_prompt_bytes", positive_integer()),
    ])
}

pub fn omo_task_wait_layer_schema() -> Node {
    strict_object(vec![
        optional("min_ms", positive_integer()),
        optional("default_ms", positive_integer()),
        optional("max_ms", positive_integer()),
    ])
}

pub fn omo_task_team_settings_layer_schema() -> Node {
    strict_object(vec![
        optional("max_members", integer_between(1, 8)),
        optional("max_parallel_members", integer_between(1, 8)),
        optional("max_wall_clock_minutes", positive_integer()),
    ])
}

pub fn omo_task_warnings_layer_schema() -> Node {
    strict_object(vec![optional("unavailable_categories", boolean())])
}

pub fn omo_task_settings_layer_schema() -> Node {
    strict_object(vec![
        optional("isolation", omo_task_isolation_layer_schema()),
        optional(
            "default_execution_mode",
            enumeration(&["in-process", "process"]),
        ),
        optional("default_concurrency", positive_integer()),
        optional("provider_concurrency", record(positive_integer())),
        optional("model_concurrency", record(positive_integer())),
        optional("max_depth", nonnegative_integer()),
        optional("residency_max_children", residency_max_children_schema()),
        optional("ttl_ms", positive_integer()),
        optional("state_dir", string()),
        optional("reattach_on_reconcile", boolean()),
        optional("resume_children", boolean()),
        optional("warnings", omo_task_warnings_layer_schema()),
        optional("wait", omo_task_wait_layer_schema()),
        optional("team", omo_task_team_settings_layer_schema()),
        optional("dag", omo_task_dag_settings_layer_schema()),
    ])
}

pub fn resolve_omo_task_settings(
    input: &Value,
    resolve_parallelism: impl Fn() -> usize,
) -> Result<Value, Issues> {
    let record_value =
        crate::internal::validate::safe_parse(&record(crate::internal::validate::any()), input)?;
    let Value::Object(mut merged) = record_value else {
        return Err(vec![crate::issue::invalid_type(&[], "object", input)]);
    };
    if !merged.contains_key("residency_max_children") {
        let parallelism = std::cmp::max(8, resolve_parallelism() * 3);
        merged.insert("residency_max_children".into(), json!(parallelism));
    }
    crate::internal::validate::safe_parse(&omo_task_settings_schema(), &Value::Object(merged))
}

pub fn resolve_omo_task_settings_default(input: &Value) -> Result<Value, Issues> {
    resolve_omo_task_settings(input, || {
        std::thread::available_parallelism()
            .map(|value| value.get())
            .unwrap_or(1)
    })
}
