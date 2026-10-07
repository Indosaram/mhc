use serde_json::{Map, Value};

use crate::shared::{DagOwnerKind, DagTaskOwner};
use crate::state::{
    DeliverAs, HostSessionIdentity, IsolationRecord, PendingSteeringEntry, RESIDENCY_STATES,
    RESOLVED_MODEL_SOURCES, ResidencyState, ResolvedModelRecord, ResolvedModelSource, RunnerKind,
    SpawnSpecV1, TASK_STATUSES, TaskIsolationSpec, TaskNotification, TaskRecord, TaskRunStats,
    TaskSpawnSpec, TaskStatus, parse_task_id,
};

type Object = Map<String, Value>;
type ParseResult<T> = Result<T, String>;

/// Parses one persisted record. Malformed pending-steering entries are dropped with a warning
/// instead of rejecting the record, so one bad entry never orphans a live child.
pub fn parse_task_record(
    value: &Value,
    path: &str,
    warnings: &mut Vec<String>,
) -> ParseResult<TaskRecord> {
    let Some(record) = value.as_object() else {
        return Err(format!("JSON record at {path} is not an object"));
    };
    let name = read_optional_string(record, "name")?;
    let task_summary = read_optional_string(record, "task_summary")?;
    let description = read_optional_string(record, "description")?;
    let agent_type = read_optional_string(record, "agent_type")?;
    let category = read_optional_string(record, "category")?;
    let tool_allow = read_optional_string_array(record, "tool_allow")?;
    let tool_deny = read_optional_string_array(record, "tool_deny")?;
    let pid = read_optional_integer(record, "pid")?;
    let host_pid = read_optional_integer(record, "host_pid")?;
    let child_session_id = read_optional_string(record, "child_session_id")?;
    let final_response = read_optional_string(record, "final_response")?;
    let error_message = read_optional_string(record, "error_message")?;
    let killed = read_optional_bool(record, "killed")?;
    let notify_on_terminal = read_optional_bool(record, "notify_on_terminal")?.unwrap_or(false);
    let requested_model = read_optional_resolved_model(record, "requested_model")?;
    let fallback_models = read_optional_resolved_model_array(record, "fallback_models")?;
    let fallback_attempts = read_optional_resolved_model_array(record, "fallback_attempts")?;
    let resolved_model = read_optional_resolved_model(record, "resolved_model")?;
    let spawn_spec = read_optional_spawn_spec(record)?;
    let owner = read_optional_owner(record)?;
    let pending_steering = read_optional_pending_steering(record, path, warnings)?;
    let run_stats = read_optional_run_stats(record)?;
    let isolation = read_optional_isolation(record)?;
    let runner_kind = read_optional_runner_kind(record)?;
    let host_session = read_optional_host_session(record)?;

    let task_id =
        parse_task_id(&read_string(record, "task_id")?).map_err(|error| error.to_string())?;
    Ok(TaskRecord {
        task_id: task_id.to_string(),
        status: read_task_status(record)?,
        residency_state: read_residency_state(record)?,
        parent_session_id: read_string(record, "parent_session_id")?,
        root_session_id: read_string(record, "root_session_id")?,
        depth: u32::try_from(read_integer(record, "depth")?)
            .map_err(|_| "depth is not a number".to_string())?,
        execution_mode: read_string(record, "execution_mode")?,
        model: read_string(record, "model")?,
        notify_on_terminal,
        created_at: read_string(record, "created_at")?,
        updated_at: read_string(record, "updated_at")?,
        notification: read_notification(record)?,
        name,
        task_summary,
        description,
        agent_type,
        category,
        tool_allow,
        tool_deny,
        requested_model,
        fallback_models,
        fallback_attempts,
        resolved_model,
        spawn_spec,
        owner,
        pending_steering: pending_steering.filter(|entries| !entries.is_empty()),
        pid,
        host_pid,
        child_session_id,
        final_response,
        error_message,
        killed,
        run_stats,
        isolation,
        runner_kind,
        host_session,
    })
}

fn read_optional_owner(record: &Object) -> ParseResult<Option<DagTaskOwner>> {
    let Some(value) = record.get("owner") else {
        return Ok(None);
    };
    let owner = value.as_object().ok_or("owner is not an object")?;
    if read_string(owner, "kind")? != "dag" {
        return Err("owner.kind is not dag".into());
    }
    Ok(Some(DagTaskOwner {
        kind: DagOwnerKind::Dag,
        run_id: read_string(owner, "runId")?,
        node_id: read_string(owner, "nodeId")?,
        fingerprint: read_string(owner, "fingerprint")?,
    }))
}

fn read_optional_isolation_spec(record: &Object) -> ParseResult<Option<TaskIsolationSpec>> {
    let Some(value) = record.get("isolation") else {
        return Ok(None);
    };
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|error| format!("isolation is invalid: {error}"))
}

fn read_optional_isolation(record: &Object) -> ParseResult<Option<IsolationRecord>> {
    let Some(value) = record.get("isolation") else {
        return Ok(None);
    };
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|error| format!("isolation is invalid: {error}"))
}

fn read_optional_runner_kind(record: &Object) -> ParseResult<Option<RunnerKind>> {
    let Some(value) = record.get("runner_kind") else {
        return Ok(None);
    };
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|error| format!("runner_kind is invalid: {error}"))
}

fn read_optional_host_session(record: &Object) -> ParseResult<Option<HostSessionIdentity>> {
    let Some(value) = record.get("host_session") else {
        return Ok(None);
    };
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|error| format!("host_session is invalid: {error}"))
}

fn read_optional_run_stats(record: &Object) -> ParseResult<Option<TaskRunStats>> {
    let Some(value) = record.get("run_stats") else {
        return Ok(None);
    };
    let stats = value.as_object().ok_or("run_stats is not an object")?;
    let cache_hit_rate_run = read_optional_number(stats, "cache_hit_rate_run")?;
    let legacy_cache_hit_rate = read_optional_number(stats, "cache_hit_rate")?;
    Ok(Some(TaskRunStats {
        runtime_ms: read_count(stats, "runtime_ms")?,
        turns: read_count(stats, "turns")?,
        tool_calls: read_count(stats, "tool_calls")?,
        output_tokens: read_optional_count(stats, "output_tokens")?,
        total_tokens: read_optional_count(stats, "total_tokens")?,
        generation_ms: read_optional_count(stats, "generation_ms")?,
        tokens_per_second: read_optional_number(stats, "tokens_per_second")?,
        cost_usd: read_optional_number(stats, "cost_usd")?,
        cache_hit_rate_last: read_optional_number(stats, "cache_hit_rate_last")?,
        cache_hit_rate_run: cache_hit_rate_run.or(legacy_cache_hit_rate),
    }))
}

fn read_optional_spawn_spec(record: &Object) -> ParseResult<Option<TaskSpawnSpec>> {
    let Some(value) = record.get("spawn_spec") else {
        return Ok(None);
    };
    let spec = value.as_object().ok_or("spawn_spec is not an object")?;
    if spec.get("version").and_then(Value::as_f64) == Some(1.0) {
        return Ok(Some(TaskSpawnSpec::V1(SpawnSpecV1 {
            cwd: read_string(spec, "cwd")?,
            prompt: read_string(spec, "prompt")?,
            instructions: read_optional_string(spec, "instructions")?,
            member_scoped_tool_names: read_optional_string_array(spec, "member_scoped_tool_names")?,
            isolation: read_optional_isolation_spec(spec)?,
        })));
    }
    Ok(Some(TaskSpawnSpec::LegacyProcess {
        cwd: read_string(spec, "cwd")?,
        extensions: None,
        member_env: None,
    }))
}

fn read_optional_pending_steering(
    record: &Object,
    path: &str,
    warnings: &mut Vec<String>,
) -> ParseResult<Option<Vec<PendingSteeringEntry>>> {
    let Some(value) = record.get("pending_steering") else {
        return Ok(None);
    };
    let entries = value.as_array().ok_or("pending_steering is not an array")?;
    let mut parsed = Vec::new();
    for (index, candidate) in entries.iter().enumerate() {
        let Some(entry) = candidate.as_object() else {
            warnings.push(format!(
                "pending_steering[{index}] at {path}: entry is not an object, dropped"
            ));
            continue;
        };
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            warnings.push(format!(
                "pending_steering[{index}] at {path}: entry missing string id, dropped"
            ));
            continue;
        };
        let Some(message) = entry.get("message").and_then(Value::as_str) else {
            warnings.push(format!(
                "pending_steering[{index}] at {path}: entry missing string message, dropped"
            ));
            continue;
        };
        let deliver_as = match entry.get("deliver_as").and_then(Value::as_str) {
            Some("steer") => DeliverAs::Steer,
            Some("followUp") => DeliverAs::FollowUp,
            _ => {
                warnings.push(format!(
                    "pending_steering[{index}] at {path}: entry has invalid deliver_as, dropped"
                ));
                continue;
            }
        };
        parsed.push(PendingSteeringEntry {
            id: id.to_string(),
            message: message.to_string(),
            deliver_as,
        });
    }
    Ok(Some(parsed))
}

fn read_optional_resolved_model(
    record: &Object,
    key: &str,
) -> ParseResult<Option<ResolvedModelRecord>> {
    let Some(value) = record.get(key) else {
        return Ok(None);
    };
    let model = value
        .as_object()
        .ok_or_else(|| format!("{key} is not an object"))?;
    read_resolved_model(model).map(Some)
}

fn read_optional_resolved_model_array(
    record: &Object,
    key: &str,
) -> ParseResult<Option<Vec<ResolvedModelRecord>>> {
    let Some(value) = record.get(key) else {
        return Ok(None);
    };
    let entries = value
        .as_array()
        .ok_or_else(|| format!("{key} is not an array"))?;
    entries
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let model = candidate
                .as_object()
                .ok_or_else(|| format!("{key}[{index}] is not an object"))?;
            read_resolved_model(model)
        })
        .collect::<ParseResult<Vec<_>>>()
        .map(Some)
}

fn read_resolved_model(value: &Object) -> ParseResult<ResolvedModelRecord> {
    let variant = read_optional_string(value, "variant")?;
    let reasoning_effort = read_optional_string(value, "reasoning_effort")?;
    let reasoning = read_optional_string(value, "reasoning")?;
    Ok(ResolvedModelRecord {
        provider: read_string(value, "provider")?,
        model_id: read_string(value, "model_id")?,
        display: read_string(value, "display")?,
        source: read_resolved_model_source(value)?,
        variant,
        reasoning_effort,
        reasoning,
    })
}

fn read_notification(record: &Object) -> ParseResult<TaskNotification> {
    let notification = record
        .get("notification")
        .and_then(Value::as_object)
        .ok_or("notification is not an object")?;
    Ok(TaskNotification {
        run_epoch: read_integer(notification, "run_epoch")?,
        notified_epoch: read_integer(notification, "notified_epoch")?,
        notification_failed_epoch: read_optional_integer(
            notification,
            "notification_failed_epoch",
        )?,
        liveness_notified_epoch: read_optional_integer(notification, "liveness_notified_epoch")?,
    })
}

fn read_task_status(record: &Object) -> ParseResult<TaskStatus> {
    let status = read_string(record, "status")?;
    TaskStatus::parse(&status).ok_or_else(|| {
        let expected: Vec<&str> = TASK_STATUSES.iter().map(|status| status.as_str()).collect();
        format!(
            "Invalid task status [REDACTED]; expected one of {}",
            expected.join(", ")
        )
    })
}

fn read_residency_state(record: &Object) -> ParseResult<ResidencyState> {
    let state = read_string(record, "residency_state")?;
    ResidencyState::parse(&state).ok_or_else(|| {
        let expected: Vec<&str> = RESIDENCY_STATES
            .iter()
            .map(|state| state.as_str())
            .collect();
        format!(
            "Invalid residency state [REDACTED]; expected one of {}",
            expected.join(", ")
        )
    })
}

fn read_resolved_model_source(record: &Object) -> ParseResult<ResolvedModelSource> {
    let source = read_string(record, "source")?;
    RESOLVED_MODEL_SOURCES
        .into_iter()
        .find(|candidate| candidate.as_str() == source)
        .ok_or_else(|| {
            let expected: Vec<&str> = RESOLVED_MODEL_SOURCES
                .iter()
                .map(|source| source.as_str())
                .collect();
            format!("resolved_model.source must be {}", expected.join(" or "))
        })
}

fn read_string(record: &Object, key: &str) -> ParseResult<String> {
    record
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("{key} is not a string"))
}

fn read_number(record: &Object, key: &str) -> ParseResult<f64> {
    record
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("{key} is not a number"))
}

fn read_integer(record: &Object, key: &str) -> ParseResult<i64> {
    let number = read_number(record, key)?;
    if number.fract() != 0.0 {
        return Err(format!("{key} is not a number"));
    }
    Ok(number as i64)
}

fn read_count(record: &Object, key: &str) -> ParseResult<u64> {
    u64::try_from(read_integer(record, key)?).map_err(|_| format!("{key} is not a number"))
}

fn read_optional_string(record: &Object, key: &str) -> ParseResult<Option<String>> {
    record
        .get(key)
        .map(|_| read_string(record, key))
        .transpose()
}

fn read_optional_number(record: &Object, key: &str) -> ParseResult<Option<f64>> {
    record
        .get(key)
        .map(|_| read_number(record, key))
        .transpose()
}

fn read_optional_integer(record: &Object, key: &str) -> ParseResult<Option<i64>> {
    record
        .get(key)
        .map(|_| read_integer(record, key))
        .transpose()
}

fn read_optional_count(record: &Object, key: &str) -> ParseResult<Option<u64>> {
    record.get(key).map(|_| read_count(record, key)).transpose()
}

fn read_optional_bool(record: &Object, key: &str) -> ParseResult<Option<bool>> {
    record
        .get(key)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| format!("{key} is not a boolean"))
        })
        .transpose()
}

fn read_optional_string_array(record: &Object, key: &str) -> ParseResult<Option<Vec<String>>> {
    let Some(value) = record.get(key) else {
        return Ok(None);
    };
    value
        .as_array()
        .and_then(|entries| {
            entries
                .iter()
                .map(|entry| entry.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
        })
        .map(Some)
        .ok_or_else(|| format!("{key} is not a string array"))
}
