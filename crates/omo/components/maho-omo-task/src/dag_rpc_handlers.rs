use std::sync::Arc;
use maho_ext_api::{ExtensionApi, ExtensionFailure};
use serde_json::{Value, json};
use senpi_task::dag::{manager::{DagManager, DagManagerError, DagManagerErrorCode, DagHistoryParams}, types::{DagEventLane, DagRunEventType}};

pub const DAG_EVENT_CHANNEL: &str = "omo.dag.event";
pub const DAG_RPC_ERROR_CODES: [&str; 4] = ["invalid_arguments", "run_not_found", "run_not_owned", "history_unavailable"];
const STATUSES: [&str; 6] = ["pending", "running", "paused", "completed", "failed", "cancelled"];
fn fail(code: &str, message: impl Into<String>) -> Value { json!({"ok":false,"error":{"code":code,"message":message.into()}}) }
fn attempt(result: Result<Value, DagManagerError>) -> Value {
    match result {
        Ok(value) => json!({"ok":true,"value":value}),
        Err(error) => { let code = match error.code { DagManagerErrorCode::RunNotFound => "run_not_found", DagManagerErrorCode::RunNotOwned => "run_not_owned", DagManagerErrorCode::InvalidArguments => "invalid_arguments", _ => "history_unavailable" }; fail(code, error.message) }
    }
}
fn attempt_history(result: Result<Value, DagManagerError>) -> Value {
    match result {
        Err(error) if error.code == DagManagerErrorCode::InvalidArguments => fail("history_unavailable", error.message),
        result => attempt(result),
    }
}
fn limit(value: Option<&Value>, default: usize, max: usize) -> Result<usize, String> {
    match value { None => Ok(default), Some(value) => value.as_u64().filter(|limit| *limit > 0).map(|limit| usize::try_from(limit).unwrap_or(usize::MAX).min(max)).ok_or_else(|| "limit must be a positive integer.".into()) }
}
fn seq(value: Option<&Value>, field: &str, default: u64) -> Result<u64, String> { value.map_or(Ok(default), |value| value.as_u64().ok_or_else(|| format!("{field} must be a non-negative integer."))) }
fn run_id(value: &Value) -> Result<String, String> {
    if !value.is_object() { return Err("Request must be an object.".into()); }
    value.get("runId").and_then(Value::as_str).filter(|id| !id.trim().is_empty()).map(str::to_owned).ok_or_else(|| "runId is required.".into())
}
fn event_type(name: &str) -> Option<DagRunEventType> {
    use DagRunEventType::*;
    [RunCreated, RunStarted, RunPaused, RunResumed, RunCompleted, RunFailed, RunCancelled, WaveStarted, WaveCompleted, NodeTransitioned, NodeTaskAttached, NodeReused, DiagnosticAdded, StreamOverflow].into_iter().find(|kind| kind.as_str() == name)
}
fn history(value: &Value, session: String) -> Result<DagHistoryParams, String> {
    let run_id = run_id(value)?;
    let since_seq = seq(value.get("sinceSeq"), "sinceSeq", 0)?;
    let limit = limit(value.get("limit"), 256, 1000)?;
    let lane = match value.get("lane") { None => None, Some(Value::String(lane)) if lane == "activity" => Some(DagEventLane::Activity), Some(Value::String(lane)) if lane == "boundary" => Some(DagEventLane::Boundary), _ => return Err("lane must be one of activity, boundary.".into()) };
    let types = match value.get("types") {
        None => None,
        Some(Value::Array(names)) => {
            let mut types = Vec::new();
            for name in names { let name = name.as_str().filter(|name| !name.trim().is_empty()).ok_or("types must contain only event types.")?; if let Some(kind) = event_type(name) { types.push(kind); } }
            Some(types)
        },
        _ => return Err("types must be an array of event types.".into()),
    };
    let through_seq = value.get("throughSeq").map(|value| seq(Some(value), "throughSeq", 0)).transpose()?;
    Ok(DagHistoryParams { run_id, parent_session_id: session, since_seq: Some(since_seq), limit: Some(limit), lane, types, through_seq })
}
pub fn query_dag_rpc(manager: &DagManager, session: Option<String>, name: &str, value: &Value) -> Value {
    if name == "omo.dag.list" {
        if !value.is_object() { return fail("invalid_arguments", "Request must be an object."); }
        let limit = match limit(value.get("limit"), 100, 256) { Ok(limit) => limit, Err(error) => return fail("invalid_arguments", error) };
        let statuses = match value.get("statuses") {
            None => None,
            Some(Value::Array(statuses)) => { if statuses.iter().any(|status| !status.as_str().is_some_and(|status| STATUSES.contains(&status))) { return fail("invalid_arguments", format!("statuses must contain only {}.", STATUSES.join(", "))); } Some(statuses) },
            _ => return fail("invalid_arguments", "statuses must be an array of run statuses."),
        };
        let Some(session) = session else { return json!({"ok":true,"value":{"runs":[],"limit":limit}}); };
        return attempt(manager.list(&session, Some(256)).map(|runs| {
            let runs: Vec<_> = runs.into_iter().filter(|run| statuses.is_none_or(|statuses| statuses.iter().any(|status| status.as_str() == Some(run.status.as_str())))).take(limit).map(|run| json!({"runId":run.run_id,"runKey":run.run_key,"name":run.name,"parentSessionId":run.parent_session_id,"status":run.status,"createdAt":run.created_at,"updatedAt":run.updated_at,"counts":run.counts})).collect(); json!({"runs":runs,"limit":limit})
        }));
    }
    let id = match run_id(value) { Ok(id) => id, Err(error) => return fail("invalid_arguments", error) };
    let params = if name == "omo.dag.snapshot" { None } else { match history(value, session.clone().unwrap_or_default()) { Ok(params) => Some(params), Err(error) => return fail("invalid_arguments", error) } };
    let Some(session) = session else { return fail("run_not_owned", format!("dag run \"{id}\" belongs to another session")); };
    match name {
        "omo.dag.snapshot" => attempt(manager.snapshot(&id, &session).map(|snapshot| json!(snapshot))),
        "omo.dag.history" => match params { Some(params) => match manager.snapshot(&id,&session) { Ok(_) => attempt_history(manager.history(params).map(|page| json!(page))), Err(error) => attempt(Err(error)) }, None => fail("invalid_arguments", "runId is required.") },
        "omo.dag.subscribe" => match params {
            Some(mut params) => match manager.snapshot(&id, &session) { Ok(snapshot) => { let high_water = snapshot.last_seq; params.through_seq = Some(params.through_seq.map_or(high_water, |through| through.min(high_water))); attempt_history(manager.history(params).map(|page| json!({"schemaVersion":1,"eventName":DAG_EVENT_CHANNEL,"snapshot":snapshot,"highWaterSeq":high_water,"page":page}))) }, Err(error) => attempt(Err(error)) },
            None => fail("invalid_arguments", "runId is required."),
        },
        _ => fail("invalid_arguments", "Unknown dag query."),
    }
}
pub fn register_dag_rpc_handlers(api: &mut ExtensionApi, manager: DagManager, session: Arc<dyn Fn() -> Option<String> + Send + Sync>) -> Result<(), ExtensionFailure> {
    for name in ["omo.dag.list", "omo.dag.snapshot", "omo.dag.history", "omo.dag.subscribe"] {
        let manager = manager.clone(); let session = session.clone();
        api.rpc_handle(name, Arc::new(move |value| { let response = query_dag_rpc(&manager, session(), name, &value); Box::pin(async move { Ok(response) }) }))?;
    }
    Ok(())
}
