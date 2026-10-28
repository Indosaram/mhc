use chrono::DateTime;
use senpi_task::state::{BackgroundMode, CostReportStatus, DurationSourceStatus, ResolvedModelRecord, TaskRecord, TaskRunStats, TaskSpawnSpec, TaskStatus, TokenCoverageStatus};
use serde_json::{Map, Value, json};

use crate::delegation_schema::DELEGATION_REASONING_EFFORTS;
use crate::product_identity::{builtin_category_names, curated_agents, mask_provider_and_model};

pub type DelegationStartReason = &'static str;
#[derive(Clone, Copy, Default)]
pub struct DelegationSteerCounts { pub running: f64, pub queued: f64 }
pub struct TaskTerminalEdge<'a> { pub record: &'a TaskRecord, pub previous_status: Option<TaskStatus> }
pub struct DelegationProjectionInput<'a> {
    pub edge: TaskTerminalEdge<'a>,
    pub session_hash: &'a str,
    pub start_reason: DelegationStartReason,
    pub steer_counts: DelegationSteerCounts,
}
pub type DelegationProperties = Map<String, Value>;

/// Explicit scalar allowlist; record text is never copied into the payload.
pub fn project_delegation_completed(input: &DelegationProjectionInput<'_>) -> DelegationProperties {
    let record = input.edge.record;
    let model = record.resolved_model.as_ref();
    let (provider, model_id) = mask_provider_and_model(model.map_or("", |m| m.provider.as_str()), model.map_or("", |m| m.model_id.as_str()));
    let (duration_ms, duration_status) = duration_of(record);
    let stats = record.run_stats.as_ref();
    let background = match record.background_mode {
        Some(BackgroundMode::Foreground) => "foreground",
        Some(BackgroundMode::Background) => "background",
        Some(BackgroundMode::Promoted) => "promoted",
        None => "unknown",
    };
    let mut props = Map::from_iter([
        ("$session_id".into(), json!(input.session_hash)),
        ("task_seq".into(), json!(record.task_seq.unwrap_or(0))),
        ("run_epoch".into(), json!(record.notification.run_epoch.max(0))),
        ("status".into(), json!(record.status.as_str())),
        ("start_reason".into(), json!(input.start_reason)),
        ("category".into(), json!(closed_name(record.category.as_deref(), builtin_category_names()))),
        ("agent_type".into(), json!(closed_name(record.agent_type.as_deref(), curated_agents()))),
        ("owner_kind".into(), json!(owner_kind_of(record))),
        ("background_mode".into(), json!(background)),
        ("execution_mode".into(), json!(if record.execution_mode == "process" { "process" } else { "in-process" })),
        ("provider".into(), json!(provider)),
        ("model_id".into(), json!(model_id)),
        ("reasoning_effort".into(), json!(reasoning_effort_of(model))),
        ("model_source".into(), json!(model.map_or("none", |m| m.source.as_str()))),
        ("fallback_attempts".into(), json!(record.fallback_attempts.as_ref().map_or(0, Vec::len))),
        ("config_generation".into(), json!(record.config_generation.unwrap_or(0))),
        ("duration_ms".into(), json!(duration_ms)),
        ("duration_status".into(), json!(duration_status)),
        ("turns".into(), json!(stats.map_or(0, |s| s.turns))),
        ("tool_calls".into(), json!(stats.map_or(0, |s| s.tool_calls))),
        ("token_status".into(), json!(match stats.and_then(|s| s.token_status) {
            Some(TokenCoverageStatus::Complete) => "complete",
            Some(TokenCoverageStatus::Partial) => "partial",
            Some(TokenCoverageStatus::Unavailable) | None => "unavailable",
        })),
        ("stats_status".into(), json!(stats_status_of(record))),
        ("task_send_running_count".into(), json!(finite_count(input.steer_counts.running))),
        ("task_send_queued_count".into(), json!(finite_count(input.steer_counts.queued))),
    ]);
    token_totals(&mut props, stats);
    cost_of(&mut props, stats);
    props
}

fn token_totals(props: &mut DelegationProperties, stats: Option<&TaskRunStats>) {
    if let Some(stats) = stats {
        for (key, value) in [("input_tokens", stats.input_tokens), ("output_tokens", stats.output_tokens), ("cache_read_tokens", stats.cache_read_tokens), ("cache_write_tokens", stats.cache_write_tokens), ("total_tokens", stats.total_tokens)] {
            if let Some(value) = value { props.insert(key.into(), json!(value)); }
        }
    }
}

/// A reported zero cost is a fact; an absent cost is not a zero. `cost_usd` therefore ships only
/// alongside `cost_status: "reported"` or `"partial"` (a lower bound: some turns reported no cost),
/// and a non-finite figure degrades to `invalid`.
fn cost_of(props: &mut DelegationProperties, stats: Option<&TaskRunStats>) {
    let status = match stats.and_then(|s| s.cost_status) {
        Some(CostReportStatus::Reported) => match stats.and_then(|s| s.cost_usd) {
            Some(cost) if cost.is_finite() && cost >= 0.0 => { props.insert("cost_usd".into(), json!(cost)); "reported" }
            Some(_) | None => "invalid",
        },
        Some(CostReportStatus::Partial) => match stats.and_then(|s| s.cost_usd) {
            Some(cost) if cost.is_finite() && cost >= 0.0 => { props.insert("cost_usd".into(), json!(cost)); "partial" }
            Some(_) | None => "invalid",
        },
        Some(CostReportStatus::Invalid) => "invalid",
        Some(CostReportStatus::Unavailable) | None => "unavailable",
    };
    props.insert("cost_status".into(), json!(status));
}

fn duration_of(record: &TaskRecord) -> (u64, &'static str) {
    if let Some(stats) = &record.run_stats
        && stats.duration_status == Some(DurationSourceStatus::Monotonic) {
        return (stats.runtime_ms, "monotonic");
    }
    let elapsed = DateTime::parse_from_rfc3339(&record.updated_at).ok().zip(DateTime::parse_from_rfc3339(&record.created_at).ok())
        .and_then(|(end, start)| u64::try_from(end.signed_duration_since(start).num_milliseconds()).ok());
    elapsed.map_or((0, "unavailable"), |ms| (ms, "wall_clock"))
}

fn stats_status_of(record: &TaskRecord) -> &'static str {
    match &record.run_stats {
        None => "unavailable",
        Some(stats) if stats.token_status == Some(TokenCoverageStatus::Complete) && stats.duration_status == Some(DurationSourceStatus::Monotonic) => "complete",
        Some(_) => "partial",
    }
}

fn owner_kind_of(record: &TaskRecord) -> &'static str {
    if record.owner.is_some() { return "dag_node"; }
    match &record.spawn_spec {
        None => "unknown",
        Some(TaskSpawnSpec::V1(spec)) => if spec.member_scoped_tool_names.is_some() { "team_member" } else { "plain_child" },
        Some(TaskSpawnSpec::LegacyProcess { member_env, .. }) => if member_env.is_some() { "team_member" } else { "plain_child" },
    }
}

fn reasoning_effort_of(model: Option<&ResolvedModelRecord>) -> &str {
    match model.and_then(|m| m.reasoning.as_deref().or(m.reasoning_effort.as_deref()).or(m.variant.as_deref())) {
        None => "none",
        Some(level) => if DELEGATION_REASONING_EFFORTS.contains(&level) { level } else { "other" },
    }
}

fn closed_name(value: Option<&str>, known: impl IntoIterator<Item = &'static str>) -> &str {
    match value {
        None | Some("") => "none",
        Some(value) => if known.into_iter().any(|name| name == value) { value } else { "custom" },
    }
}

fn finite_count(value: f64) -> f64 { if value.is_finite() && value >= 0.0 { value } else { 0.0 } }

#[cfg(test)]
mod tests {
    use super::*;
    use senpi_task::state::{TaskRecordInput, ResolvedModelSource, SpawnSpecV1, create_task_record};

    fn record() -> TaskRecord {
        let mut model = ResolvedModelRecord::new(ResolvedModelSource::Category, "anthropic", "claude-opus-5");
        model.reasoning = Some("high".into());
        let mut record = create_task_record(TaskRecordInput {
            execution_mode: "in-process".into(), category: Some("deep-low".into()),
            task_seq: Some(3), config_generation: Some(2), background_mode: Some(BackgroundMode::Foreground),
            resolved_model: Some(model), ..Default::default()
        }, Some(1_000)).expect("fixture task id");
        record.status = TaskStatus::Completed;
        record.created_at = "2026-08-21T00:00:00.000Z".into();
        record.updated_at = "2026-08-21T00:01:00.000Z".into();
        record.spawn_spec = Some(TaskSpawnSpec::V1(SpawnSpecV1 { cwd: "/repo".into(), prompt: "work".into(), instructions: None, member_scoped_tool_names: None, isolation: None }));
        record.run_stats = Some(TaskRunStats {
            runtime_ms: 1234, turns: 4, tool_calls: 9,
            duration_status: Some(DurationSourceStatus::Monotonic), token_status: Some(TokenCoverageStatus::Complete), cost_status: Some(CostReportStatus::Reported),
            input_tokens: Some(100), output_tokens: Some(20), cache_read_tokens: Some(30), cache_write_tokens: Some(40), total_tokens: Some(190), cost_usd: Some(0.5), ..Default::default()
        });
        record
    }
    fn project(record: &TaskRecord) -> DelegationProperties {
        project_delegation_completed(&DelegationProjectionInput {
            edge: TaskTerminalEdge { record, previous_status: None }, session_hash: "hashed-parent", start_reason: "initial_spawn",
            steer_counts: DelegationSteerCounts { running: 2.0, queued: 1.0 },
        })
    }
    #[test]
    fn excludes_record_text_with_exact_allowed_keys() {
        let mut record = record();
        let secret = "PROPRIETARY-PLAN-TEXT";
        record.name = Some(secret.into()); record.description = Some(secret.into()); record.task_summary = Some(secret.into());
        record.final_response = Some(secret.into()); record.error_message = Some(secret.into());
        if let Some(TaskSpawnSpec::V1(spec)) = &mut record.spawn_spec { spec.cwd = secret.into(); spec.prompt = secret.into(); }
        let props = project(&record);
        let expected = &crate::event_schemas::OMO_NATIVE_PROPERTY_ALLOWLISTS["delegation_completed"];
        assert_eq!(props.keys().collect::<std::collections::BTreeSet<_>>(), expected.iter().collect());
        assert!(!json!(props).to_string().contains(secret));
        for value in props.values() {
            if let Some(text) = value.as_str() { assert!(text.len() <= 64); }
            if let Some(number) = value.as_f64() { assert!(number.is_finite()); }
        }
    }
    #[test]
    fn carries_identity_shape_stats_and_counters() {
        let props = project(&record());
        assert_eq!(json!(props), json!({
            "$session_id":"hashed-parent", "task_seq":3,"run_epoch":0,"status":"completed","start_reason":"initial_spawn",
            "category":"deep-low","agent_type":"none","owner_kind":"plain_child","background_mode":"foreground","execution_mode":"in-process",
            "provider":"anthropic","model_id":"claude-opus-5","reasoning_effort":"high","model_source":"category","fallback_attempts":0,"config_generation":2,
            "duration_ms":1234,"duration_status":"monotonic","turns":4,"tool_calls":9,"input_tokens":100,"output_tokens":20,"cache_read_tokens":30,
            "cache_write_tokens":40,"total_tokens":190,"token_status":"complete","cost_usd":0.5,"cost_status":"reported","stats_status":"complete",
            "task_send_running_count":2.0,"task_send_queued_count":1.0
        }));
    }
    #[test]
    fn masks_private_gateway_but_keeps_public_model() {
        let mut record = record();
        let model = record.resolved_model.as_mut().expect("fixture model");
        model.provider = "my-gateway".into(); model.source = ResolvedModelSource::Explicit;
        let props = project(&record);
        assert_eq!(props["provider"], "custom"); assert_eq!(props["model_id"], "claude-opus-5"); assert_eq!(props["model_source"], "explicit");
    }
    #[test]
    fn masks_private_gateway_and_private_model() {
        let mut record = record();
        let model = record.resolved_model.as_mut().expect("fixture model");
        model.provider = "my-gateway".into(); model.model_id = "acme-internal-ft".into(); model.source = ResolvedModelSource::Explicit;
        let props = project(&record);
        assert_eq!(props["provider"], "custom"); assert_eq!(props["model_id"], "custom"); assert_eq!(props["model_source"], "explicit");
    }
    #[test]
    fn absent_cost_and_tokens_are_not_zero() {
        let mut record = record();
        record.run_stats = Some(TaskRunStats { runtime_ms:10, turns:1, duration_status:Some(DurationSourceStatus::Monotonic), token_status:Some(TokenCoverageStatus::Partial), cost_status:Some(CostReportStatus::Unavailable), ..Default::default() });
        let props = project(&record);
        assert!(!props.contains_key("cost_usd")); assert!(!props.contains_key("input_tokens")); assert_eq!(props["token_status"], "partial"); assert_eq!(props["cost_status"], "unavailable");
    }
    #[test]
    fn reported_zero_cost_is_preserved() {
        let mut record = record();
        record.run_stats.as_mut().expect("fixture stats").cost_usd = Some(0.0);
        let props = project(&record);
        assert_eq!(props["cost_usd"], 0.0); assert_eq!(props["cost_status"], "reported");
    }
    #[test]
    fn partial_cost_ships_as_a_lower_bound() {
        let mut record = record();
        {
            let stats = record.run_stats.as_mut().expect("fixture stats");
            stats.cost_status = Some(CostReportStatus::Partial);
            stats.cost_usd = Some(0.25);
        }
        let props = project(&record);
        assert_eq!(props["cost_status"], "partial");
        assert_eq!(props["cost_usd"], 0.25);
        assert!(crate::delegation_schema::DELEGATION_COST_STATUSES.contains(&"partial"));
    }
    #[test]
    fn invalid_partial_figure_degrades_to_invalid() {
        let mut record = record();
        {
            let stats = record.run_stats.as_mut().expect("fixture stats");
            stats.cost_status = Some(CostReportStatus::Partial);
            stats.cost_usd = Some(f64::NAN);
        }
        let props = project(&record);
        assert_eq!(props["cost_status"], "invalid");
        assert!(!props.contains_key("cost_usd"));
        let mut missing = super::tests::record();
        let stats = missing.run_stats.as_mut().expect("fixture stats");
        stats.cost_status = Some(CostReportStatus::Partial);
        stats.cost_usd = None;
        assert_eq!(project(&missing)["cost_status"], "invalid");
    }
    #[test]
    fn missing_stats_falls_back_to_wall_clock() {
        let mut record = record(); record.run_stats = None; record.status = TaskStatus::Lost;
        record.updated_at = "2026-08-21T00:00:05.000Z".into();
        let props = project(&record);
        assert_eq!(props["duration_ms"], 5000); assert_eq!(props["duration_status"], "wall_clock"); assert_eq!(props["status"], "lost");
        for key in ["stats_status","token_status","cost_status"] { assert_eq!(props[key], "unavailable"); }
        assert!(!props.contains_key("cost_usd"));
    }
    #[test]
    fn backwards_clock_is_unavailable() {
        let mut record = record(); record.run_stats = None;
        record.created_at = "2026-08-21T00:00:10.000Z".into(); record.updated_at = "2026-08-21T00:00:05.000Z".into();
        let props = project(&record);
        assert_eq!(props["duration_status"], "unavailable"); assert_eq!(props["duration_ms"], 0);
    }
    #[test]
    fn owner_kinds_and_retry_reason_are_distinct() {
        let mut dag = record();
        dag.owner = Some(senpi_task::shared::DagTaskOwner { kind:senpi_task::shared::DagOwnerKind::Dag, run_id:"dag-1".into(), node_id:"node-1".into(), fingerprint:"fp".into() });
        let props = project_delegation_completed(&DelegationProjectionInput { edge:TaskTerminalEdge { record:&dag, previous_status:Some(TaskStatus::Running) }, session_hash:"hashed-parent", start_reason:"dag_retry", steer_counts:DelegationSteerCounts::default() });
        assert_eq!(props["owner_kind"], "dag_node"); assert_eq!(props["start_reason"], "dag_retry");
        let mut member = record();
        if let Some(TaskSpawnSpec::V1(spec)) = &mut member.spawn_spec { spec.member_scoped_tool_names = Some(vec!["task_send".into()]); }
        assert_eq!(project(&member)["owner_kind"], "team_member");
        member.spawn_spec = None; assert_eq!(project(&member)["owner_kind"], "unknown");
    }
    #[test]
    fn custom_names_and_missing_ordinals_use_safe_defaults() {
        let mut record = record(); record.category = Some("my-private-category".into()); record.agent_type = Some("my-private-agent".into());
        record.task_seq = None; record.config_generation = None; record.background_mode = None;
        let mut model = ResolvedModelRecord::new(ResolvedModelSource::Agent, "openai", "gpt-5.6-sol"); model.reasoning = Some("harness-preset-42".into());
        record.fallback_attempts = Some(vec![model.clone(), model.clone()]); record.resolved_model = Some(model);
        let props = project(&record);
        assert_eq!(props["category"], "custom"); assert_eq!(props["agent_type"], "custom"); assert_eq!(props["task_seq"],0); assert_eq!(props["config_generation"],0);
        assert_eq!(props["background_mode"],"unknown"); assert_eq!(props["fallback_attempts"],2); assert_eq!(props["reasoning_effort"],"other");
    }
    #[test]
    fn absent_targets_read_none() {
        let mut record = record(); record.category = None; record.agent_type = None;
        let props = project(&record);
        assert_eq!(props["category"],"none"); assert_eq!(props["agent_type"],"none"); assert_eq!(props["reasoning_effort"],"high");
    }
}
