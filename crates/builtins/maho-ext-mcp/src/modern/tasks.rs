//! Port of senpi mcp/modern/tasks.ts: follow an `io.modelcontextprotocol/tasks` handle for its originating request.
use super::{
    elicitation::MrtrInputResponse,
    errors::{McpMrtrError, McpMrtrErrorCode},
    inputs::parse_inputs,
};
use crate::wrap::{McpClock, McpLinkedSignal, mcp_abortable_delay, mcp_interrupted};
use futures::future::BoxFuture;
use maho_ext_api::AbortSignal;
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

pub const MCP_TASK_EXTENSION: &str = "io.modelcontextprotocol/tasks";
pub const MCP_TASK_TIMEOUT_ENV: &str = "SENPI_MCP_TASK_TIMEOUT_MS";
const DEFAULT_TASK_TIMEOUT_MS: f64 = 30.0 * 60_000.0;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("SENPI_MCP_TASK_TIMEOUT_MS must be a positive integer no greater than 2147483647.")]
pub struct McpTaskTimeoutEnvError;

pub fn mcp_task_timeout_ms() -> Result<f64, McpTaskTimeoutEnvError> {
    let raw = std::env::var(MCP_TASK_TIMEOUT_ENV).ok();
    let Some(raw) = raw
        .as_deref()
        .map(maho_ai::utils::js::trim)
        .filter(|raw| !raw.is_empty())
    else {
        return Ok(DEFAULT_TASK_TIMEOUT_MS);
    };
    let value = maho_ai::utils::js::string_to_number(raw);
    if !value.is_finite() || value.fract() != 0.0 || value <= 0.0 || value > 2_147_483_647.0 {
        return Err(McpTaskTimeoutEnvError);
    }
    Ok(value)
}

const STATUSES: [&str; 5] = [
    "working",
    "input_required",
    "completed",
    "cancelled",
    "failed",
];
fn safe_non_negative(value: &Value) -> bool {
    value.as_f64().is_some_and(|value| {
        value >= 0.0 && value.fract() == 0.0 && value <= 9_007_199_254_740_991.0
    })
}
fn parse_task(
    value: &Map<String, Value>,
    id: Option<&str>,
) -> Result<Map<String, Value>, McpMrtrError> {
    let task_id = value
        .get("taskId")
        .and_then(Value::as_str)
        .filter(|task| !task.is_empty());
    let valid = task_id.is_some()
        && id.is_none_or(|id| task_id == Some(id))
        && value
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| STATUSES.contains(&status))
        && value.get("createdAt").is_some_and(Value::is_string)
        && value.get("lastUpdatedAt").is_some_and(Value::is_string)
        && value
            .get("ttlMs")
            .is_some_and(|ttl| ttl.is_null() || safe_non_negative(ttl))
        && value.get("pollIntervalMs").is_none_or(safe_non_negative)
        && value.get("statusMessage").is_none_or(Value::is_string);
    if valid {
        Ok(value.clone())
    } else {
        Err(McpMrtrError::new(McpMrtrErrorCode::InvalidResult))
    }
}

pub type McpTaskSend<'a> = dyn Fn(
    &'static str,
    Value,
    Option<AbortSignal>,
) -> BoxFuture<'static, Result<Map<String, Value>, McpMrtrError>> + Send + Sync + 'a;
pub type McpTaskInputs<'a> = dyn Fn(
    Map<String, Value>,
    AbortSignal,
) -> BoxFuture<'static, Result<Map<String, Value>, McpMrtrError>> + Send + Sync + 'a;
pub type McpTaskProgress<'a> =
    dyn Fn(&str, &str, Option<&str>) -> Result<(), McpMrtrError> + Send + Sync + 'a;

/// What the originating request lends the follower.
pub struct McpTaskOptions<'a> {
    pub clock: &'a dyn McpClock,
    pub signal: AbortSignal,
    pub timeout_ms: f64,
    /// `signal: None` is the cooperative `tasks/cancel`, which has its own lifetime.
    pub send: &'a McpTaskSend<'a>,
    pub inputs: &'a McpTaskInputs<'a>,
    pub progress: &'a McpTaskProgress<'a>,
}

/// Polling belongs to the originating request, never to the shared physical client.
pub async fn follow_mcp_task(
    initial: &Map<String, Value>,
    options: McpTaskOptions<'_>,
) -> Result<Map<String, Value>, McpMrtrError> {
    let mut task = parse_task(initial, None)?;
    let id = task["taskId"].as_str().unwrap_or_default().to_owned();
    let timeout_error = McpMrtrError::with(McpMrtrErrorCode::TaskTimeout, None, Some(&id));
    let deadline = options.clock.now() + options.timeout_ms;
    let cap = AbortSignal::default();
    let timer = {
        let cap = cap.clone();
        let sleep = options.clock.sleep(options.timeout_ms);
        tokio::spawn(async move {
            sleep.await;
            cap.abort();
        })
    };
    struct Timer(tokio::task::JoinHandle<()>);
    impl Drop for Timer {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _timer = Timer(timer);
    let linked = McpLinkedSignal::new(vec![options.signal.clone(), cap.clone()]);
    let signal = linked.signal.clone();
    let aborted = || {
        if cap.is_aborted() {
            timeout_error.clone()
        } else {
            McpMrtrError::new(McpMrtrErrorCode::Cancelled)
        }
    };
    let mut answered = BTreeSet::new();
    let mut terminal = false;
    let mut backoff = 1000.0_f64;
    let result = async {
        loop {
            if signal.is_aborted() {
                return Err(aborted());
            }
            match task["status"].as_str().unwrap_or_default() {
                "completed" => {
                    let Some(result) = task.get("result").and_then(Value::as_object) else {
                        return Err(McpMrtrError::new(McpMrtrErrorCode::InvalidResult));
                    };
                    terminal = true;
                    return Ok(result.clone());
                }
                "failed" => {
                    let error = task.get("error");
                    let code = error
                        .and_then(|error| error.get("code"))
                        .and_then(Value::as_f64)
                        .filter(|code| code.fract() == 0.0);
                    let (Some(code), true) = (
                        code,
                        error
                            .and_then(|error| error.get("message"))
                            .is_some_and(Value::is_string),
                    ) else {
                        return Err(McpMrtrError::new(McpMrtrErrorCode::InvalidResult));
                    };
                    terminal = true;
                    return Err(McpMrtrError::with(
                        McpMrtrErrorCode::TaskFailed,
                        Some(code as i64),
                        Some(&id),
                    ));
                }
                "cancelled" => {
                    terminal = true;
                    return Err(McpMrtrError::with(
                        McpMrtrErrorCode::TaskCancelled,
                        None,
                        Some(&id),
                    ));
                }
                _ => {}
            }
            (options.progress)(
                &id,
                task["status"].as_str().unwrap_or_default(),
                task.get("statusMessage").and_then(Value::as_str),
            )?;
            tokio::task::yield_now().await;
            // CreateTaskResult is only the seed; inputRequests are authoritative on tasks/get.
            if task["status"] == "input_required"
                && task.get("resultType").and_then(Value::as_str) != Some("task")
            {
                let mut probe = Map::new();
                probe.insert(
                    "inputRequests".into(),
                    task.get("inputRequests").cloned().unwrap_or(Value::Null),
                );
                parse_inputs(&probe)?;
                let Some(requests) = task.get("inputRequests").and_then(Value::as_object) else {
                    return Err(McpMrtrError::new(McpMrtrErrorCode::InvalidInput));
                };
                let outstanding = requests
                    .iter()
                    .filter(|(key, _)| !answered.contains(*key))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<Map<_, _>>();
                if !outstanding.is_empty() {
                    let mut batch = Map::new();
                    batch.insert("inputRequests".into(), Value::Object(outstanding.clone()));
                    let responses =
                        mcp_interrupted((options.inputs)(batch, signal.clone()), &signal)
                            .await
                            .map_err(|_| aborted())??;
                    if signal.is_aborted() {
                        return Err(aborted());
                    }
                    (options.send)(
                        "tasks/update",
                        json!({"taskId": id, "inputResponses": responses}),
                        Some(signal.clone()),
                    )
                    .await?;
                    answered.extend(outstanding.keys().cloned());
                }
            }
            let poll = task
                .get("pollIntervalMs")
                .and_then(Value::as_f64)
                .unwrap_or(backoff);
            mcp_abortable_delay(
                options.clock,
                poll.min(deadline - options.clock.now()),
                &signal,
            )
            .await
            .map_err(|_| aborted())?;
            // Upstream's cap timer fires synchronously inside the same clock turn as a due poll: a due cap wins.
            if options.clock.now() >= deadline {
                cap.abort();
                return Err(aborted());
            }
            let next =
                (options.send)("tasks/get", json!({"taskId": id}), Some(signal.clone())).await?;
            if next
                .get("resultType")
                .is_some_and(|kind| kind.as_str() != Some("complete"))
            {
                return Err(McpMrtrError::new(McpMrtrErrorCode::InvalidResult));
            }
            task = parse_task(&next, Some(&id))?;
            backoff = (backoff * 2.0).min(30_000.0);
        }
    }
    .await;
    answered.clear();
    match result {
        Ok(result) => Ok(result),
        Err(error) => {
            // A changed owner/credential must never send a task operation with somebody else's authority.
            if !terminal && error.code != McpMrtrErrorCode::ContextChanged {
                // Cancellation is cooperative; retain the original failure even if disconnected.
                let _ = (options.send)("tasks/cancel", json!({"taskId": id}), None).await;
            }
            if cap.is_aborted() {
                Err(timeout_error)
            } else {
                Err(error)
            }
        }
    }
}

/// Upstream `MrtrInputResponse` map as JSON (task `inputResponses`).
pub fn input_responses_json(responses: &[(String, MrtrInputResponse)]) -> Map<String, Value> {
    responses
        .iter()
        .map(|(key, response)| {
            (
                key.clone(),
                serde_json::to_value(response).unwrap_or(Value::Null),
            )
        })
        .collect()
}
