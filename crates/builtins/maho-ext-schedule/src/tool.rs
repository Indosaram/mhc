//! `schedule_prompt`: the model-callable surface for durable scheduled prompts.
//!
//! Unlike `/loop` (in-process timers, interactive only) this works in every run mode, including
//! `--print`: the tool only WRITES a job file and returns. Firing happens later in a separate
//! `schedule run` process, or in the open session itself (`live_delivery`).
//!
//! The schema is a flat object with no root union (provider schema conversions rebuild tools from
//! top-level `properties`); per-action requirements are enforced in `execute`.
use crate::live_delivery::LiveDelivery;
use maho_agent::types::AgentToolResult;
use maho_ai::types::{ContentBlock, TextContent};
use maho_core::schedule::{
    native_probes::{NativeProbes, NowFn},
    occurrences::cancel_scheduled_job,
    runner_lease::is_watch_runner_available,
    store::{
        JobRecord, NewScheduledJob, ScheduledJobState, create_scheduled_job, list_scheduled_jobs,
        schedule_dir,
    },
    types::{
        MAX_PENDING_JOBS_PER_SESSION, MAX_PENDING_JOBS_TOTAL, MAX_PROMPT_BYTES,
        MAX_SCHEDULE_AHEAD_MS, MIN_EVERY_SECONDS, ScheduledJob,
    },
};
use maho_ext_api::{
    ExtensionApi, ExtensionContext, ExtensionFailure, ToolDefinition, ToolError, ToolExecutionMode,
    ToolExposure,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub const SCHEDULE_PROMPT_TOOL: &str = "schedule_prompt";

/// A due time this far in the past is almost certainly a mistake (wrong date or time zone).
const PAST_TOLERANCE_MS: u64 = 60_000;

pub fn schedule_prompt_description() -> String {
    format!(
        "Schedule a prompt to run later in THIS session: a reminder, a follow-up check, or a recurring task. The job is stored on disk and survives this process exiting (works in --print/headless runs). When it is due, a `schedule run` runner delivers it back to this session as a new message that starts with \"[Scheduled prompt <id> ...]\".\n\n- action \"create\": pass `prompt` (what to do or say when it fires, written as an instruction to your future self) and exactly one of `delaySeconds` or `at` (ISO 8601 date-time; include the UTC offset, e.g. 2026-09-28T09:00:00+09:00). Optional `everySeconds` (>= {MIN_EVERY_SECONDS}) repeats it.\n- action \"list\": this session's scheduled jobs.\n- action \"cancel\": pass `id`. A delivery that has already started cannot be recalled; nothing after it runs.\n\nIf this session is open in another process when a job is due, the default runner waits until it closes. Limits: prompt {MAX_PROMPT_BYTES} bytes, {MAX_PENDING_JOBS_PER_SESSION} pending jobs per session."
    )
}

pub fn schedule_prompt_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ["create", "list", "cancel"], "description": "create a job, list this session's jobs, or cancel one by id."},
            "prompt": {"type": "string", "description": "create: the prompt delivered to this session when the job fires."},
            "delaySeconds": {"type": "integer", "description": "create: fire this many seconds from now."},
            "at": {"type": "string", "description": "create: absolute ISO 8601 date-time with UTC offset to fire at."},
            "everySeconds": {"type": "integer", "description": format!("create: repeat every N seconds (minimum {MIN_EVERY_SECONDS}).")},
            "id": {"type": "string", "description": "cancel: the job id (sch_...)."}
        },
        "required": ["action"],
        "additionalProperties": false
    })
}

fn fail(message: impl Into<String>) -> ExtensionFailure {
    ExtensionFailure::new(message.into())
}

fn iso(ms: u64) -> String {
    i64::try_from(ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map_or_else(
            || "Invalid Date".into(),
            |date| date.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        )
}

fn local(ms: u64) -> String {
    i64::try_from(ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map_or_else(
            || "Invalid Date".into(),
            |date| {
                date.with_timezone(&chrono::Local)
                    .format("%a %b %d %Y %H:%M:%S GMT%z")
                    .to_string()
            },
        )
}

fn state_name(state: ScheduledJobState) -> &'static str {
    state.directory()
}

fn describe_job(job: &ScheduledJob, state: ScheduledJobState) -> String {
    let every = job.every_ms.map_or_else(String::new, |every| {
        format!(", every {}s", (every as f64 / 1000.0).round())
    });
    let error = job
        .last_error
        .as_ref()
        .map_or_else(String::new, |error| format!(", last error: {error}"));
    let prompt = if job.prompt.chars().count() > 120 {
        format!("{}...", job.prompt.chars().take(117).collect::<String>())
    } else {
        job.prompt.clone()
    };
    format!(
        "{} [{}] due {} ({}){every}{error}: {prompt}",
        job.id,
        state_name(state),
        iso(job.due_at),
        local(job.due_at)
    )
}

/// ISO 8601 date-time that names its instant: seconds optional, `Z` or a `+HH:MM` offset required.
fn parse_absolute(at: &str) -> Result<Option<u64>, ExtensionFailure> {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"(?i)^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2})(:\d{2}(?:\.\d{1,3})?)?(Z|[+-]\d{2}:\d{2})$",
        )
        .unwrap_or_else(|error| panic!("invalid static date pattern: {error}"))
    });
    let Some(captures) = pattern.captures(at) else {
        return Err(fail(format!(
            "at must be an ISO 8601 date-time with Z or a UTC offset (e.g. 2026-09-28T09:00:00+09:00): {at}"
        )));
    };
    let seconds = captures.get(2).map_or(":00", |seconds| seconds.as_str());
    let text = format!("{}{seconds}{}", &captures[1], &captures[3]);
    let parsed = chrono::DateTime::parse_from_rfc3339(&text)
        .map_err(|_| fail(format!("at is not a valid date-time: {at}")))?;
    Ok(u64::try_from(parsed.timestamp_millis()).ok())
}

fn resolve_due_at(
    delay_seconds: Option<&Value>,
    at: Option<&str>,
    now: u64,
) -> Result<u64, ExtensionFailure> {
    let trimmed = at.map_or("", str::trim);
    if delay_seconds.is_none() == trimmed.is_empty() {
        return Err(fail("create needs exactly one of delaySeconds or at."));
    }
    if let Some(delay) = delay_seconds {
        let seconds = delay
            .as_u64()
            .ok_or_else(|| fail("delaySeconds must be a non-negative integer."))?;
        return Ok(now.saturating_add(seconds.saturating_mul(1000)));
    }
    let due_at = parse_absolute(trimmed)?;
    match due_at {
        Some(due_at) if due_at >= now.saturating_sub(PAST_TOLERANCE_MS) => Ok(due_at),
        _ => Err(fail(format!(
            "at ({}) is in the past; now is {}.",
            iso(due_at.unwrap_or(0)),
            iso(now)
        ))),
    }
}

fn check_capacity(jobs: &[JobRecord], session_id: &str) -> Result<(), ExtensionFailure> {
    let pending = jobs
        .iter()
        .filter(|record| record.state == ScheduledJobState::Pending)
        .collect::<Vec<_>>();
    if pending.len() >= MAX_PENDING_JOBS_TOTAL {
        return Err(fail(format!(
            "the schedule already holds {MAX_PENDING_JOBS_TOTAL} pending jobs; cancel some first."
        )));
    }
    if pending
        .iter()
        .filter(|record| record.job.session_id == session_id)
        .count()
        >= MAX_PENDING_JOBS_PER_SESSION
    {
        return Err(fail(format!(
            "this session already has {MAX_PENDING_JOBS_PER_SESSION} pending jobs; cancel some first."
        )));
    }
    Ok(())
}

fn text_result(text: String, details: Value) -> AgentToolResult {
    AgentToolResult {
        content: vec![ContentBlock::Text(TextContent {
            text,
            audience: None,
            text_signature: None,
        })],
        details,
        structured_content: None,
        usage: None,
        nested_calls: None,
        added_tool_names: None,
        terminate: None,
        is_error: None,
    }
}

fn job_details(state: ScheduledJobState, job: &ScheduledJob) -> Value {
    json!({"state": state_name(state), "job": job})
}

async fn create(
    args: &Value,
    ctx: &ExtensionContext,
    now_fn: &NowFn,
    live: &LiveDelivery,
) -> Result<AgentToolResult, ExtensionFailure> {
    let now = now_fn();
    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .map_or("", str::trim)
        .to_owned();
    if prompt.is_empty() {
        return Err(fail("create needs a non-empty prompt."));
    }
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err(fail(format!(
            "prompt must be at most {MAX_PROMPT_BYTES} bytes."
        )));
    }
    let due_at = resolve_due_at(
        args.get("delaySeconds").filter(|value| !value.is_null()),
        args.get("at").and_then(Value::as_str),
        now,
    )?;
    if due_at.saturating_sub(now) > MAX_SCHEDULE_AHEAD_MS {
        return Err(fail("a job can be scheduled at most 366 days ahead."));
    }
    let mut every_ms = None;
    if let Some(every) = args.get("everySeconds").filter(|value| !value.is_null()) {
        // A product past u64 is past Number.MAX_SAFE_INTEGER too, which upstream rejects with the
        // integer error.
        let ms = every
            .as_u64()
            .filter(|seconds| *seconds >= MIN_EVERY_SECONDS)
            .and_then(|seconds| seconds.checked_mul(1000))
            .ok_or_else(|| {
                fail(format!(
                    "everySeconds must be an integer of at least {MIN_EVERY_SECONDS}."
                ))
            })?;
        if ms > MAX_SCHEDULE_AHEAD_MS {
            return Err(fail(format!(
                "everySeconds must be at most {} (366 days).",
                MAX_SCHEDULE_AHEAD_MS / 1000
            )));
        }
        every_ms = Some(ms);
    }
    let dir = schedule_dir(&ctx.agent_dir);
    let session_id = ctx.session_manager.session_id().to_owned();
    let listing = list_scheduled_jobs(&dir).map_err(|error| fail(error.to_string()))?;
    check_capacity(&listing.jobs, &session_id)?;
    let job = create_scheduled_job(
        &dir,
        NewScheduledJob {
            session_id,
            session_file: ctx
                .session_manager
                .session_file()
                .map(|path| path.to_string_lossy().into_owned()),
            cwd: ctx.cwd.to_string_lossy().into_owned(),
            prompt,
            due_at,
            every_ms,
        },
        now,
    )
    .map_err(|error| fail(error.to_string()))?;
    let probes = NativeProbes::new(now_fn.clone()).map_err(|error| fail(error.to_string()))?;
    let runner_available = is_watch_runner_available(&dir, &probes)
        .await
        .map_err(|error| fail(error.to_string()))?;
    let mut lines = vec![format!(
        "Scheduled {}",
        describe_job(&job, ScheduledJobState::Pending)
    )];
    if live.is_live() {
        live.replan().await;
        lines.push("While this session stays open it delivers the prompt itself when due, steered into the current turn if one is running.".into());
        lines.push(
            if runner_available {
                "After it closes, the active schedule runner delivers it."
            } else {
                "If it closes first, the job fires the next time a `schedule run` runner runs."
            }
            .into(),
        );
    } else {
        lines.push(
            if runner_available {
                "A schedule runner is active; it will deliver the prompt to this session when due."
            } else {
                "No `schedule run --watch` runner is active right now; the job fires the next time a runner runs."
            }
            .into(),
        );
    }
    Ok(text_result(
        lines.join("\n"),
        json!({"action": "create", "jobs": [job_details(ScheduledJobState::Pending, &job)], "runnerAvailable": runner_available}),
    ))
}

fn list(ctx: &ExtensionContext) -> Result<AgentToolResult, ExtensionFailure> {
    let session_id = ctx.session_manager.session_id();
    let listing = list_scheduled_jobs(&schedule_dir(&ctx.agent_dir))
        .map_err(|error| fail(error.to_string()))?;
    let own = listing
        .jobs
        .iter()
        .filter(|record| record.job.session_id == session_id)
        .collect::<Vec<_>>();
    let text = if own.is_empty() {
        "No scheduled jobs for this session.".to_owned()
    } else {
        own.iter()
            .map(|record| describe_job(&record.job, record.state))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let jobs = own
        .iter()
        .map(|record| job_details(record.state, &record.job))
        .collect::<Vec<_>>();
    Ok(text_result(text, json!({"action": "list", "jobs": jobs})))
}

async fn cancel(
    args: &Value,
    ctx: &ExtensionContext,
    live: &LiveDelivery,
) -> Result<AgentToolResult, ExtensionFailure> {
    let id = args.get("id").and_then(Value::as_str).map_or("", str::trim);
    if id.is_empty() {
        return Err(fail("cancel needs the job id."));
    }
    let removed = cancel_scheduled_job(
        &schedule_dir(&ctx.agent_dir),
        id,
        Some(ctx.session_manager.session_id()),
    )
    .map_err(|error| fail(error.to_string()))?
    .ok_or_else(|| fail(format!("No scheduled job {id} in this session.")))?;
    live.replan().await;
    let text = if removed.in_flight {
        format!(
            "Cancelled {id}. One delivery had already started and may still arrive; nothing after it will run."
        )
    } else {
        format!(
            "Cancelled {}",
            describe_job(&removed.job, ScheduledJobState::Pending)
        )
    };
    Ok(text_result(
        text,
        json!({"action": "cancel", "jobs": [job_details(ScheduledJobState::Pending, &removed.job)], "inFlight": removed.in_flight}),
    ))
}

pub fn register_schedule_tool(api: &mut ExtensionApi, now: NowFn, live: LiveDelivery) {
    let mut definition = ToolDefinition::new(
        SCHEDULE_PROMPT_TOOL,
        &schedule_prompt_description(),
        schedule_prompt_schema(),
        Arc::new(|_| {
            Box::pin(async { Err(ToolError::Message("Extension context required".into())) })
        }),
    );
    definition.label = "Schedule Prompt".into();
    definition.exposure = Some(ToolExposure::Search);
    definition.search_keywords = Some(
        [
            "reminder",
            "remind",
            "later",
            "schedule",
            "cron",
            "timer",
            "alarm",
            "follow up",
            "recurring",
        ]
        .map(String::from)
        .to_vec(),
    );
    definition.execution_mode = Some(ToolExecutionMode::Sequential);
    if let Err(error) = api.register_tool_with_extension_context(
        definition,
        Arc::new(move |_id, args, _signal, _update, ctx| {
            let (now, live) = (now.clone(), live.clone());
            Box::pin(async move {
                match args.get("action").and_then(Value::as_str) {
                    Some("create") => create(&args, ctx, &now, &live).await,
                    Some("list") => list(ctx),
                    Some("cancel") => cancel(&args, ctx, &live).await,
                    _ => Err(fail("action must be one of create, list, cancel.")),
                }
            })
        }),
    ) {
        std::panic::panic_any(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_flat_without_a_root_union() {
        let schema = schedule_prompt_schema();
        assert!(schema.get("anyOf").is_none() && schema.get("oneOf").is_none());
        assert_eq!(schema["required"], json!(["action"]));
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[test]
    fn absolute_times_need_an_offset_and_may_omit_seconds() {
        assert_eq!(
            parse_absolute("2026-09-28T09:00:00+09:00").expect("parsed"),
            Some(1_790_553_600_000)
        );
        assert_eq!(
            parse_absolute("2026-09-28T09:00+09:00").expect("parsed"),
            Some(1_790_553_600_000)
        );
        assert_eq!(
            parse_absolute("2026-09-28T00:00:00.500Z").expect("parsed"),
            Some(1_790_553_600_500)
        );
        assert!(parse_absolute("2026-09-28T09:00:00").is_err());
        assert!(parse_absolute("2026-13-28T09:00:00Z").is_err());
    }

    #[test]
    fn create_needs_exactly_one_of_delay_or_at() {
        assert!(resolve_due_at(None, None, 10).is_err());
        assert!(resolve_due_at(Some(&json!(5)), Some("2026-09-28T09:00:00Z"), 10).is_err());
        assert_eq!(
            resolve_due_at(Some(&json!(5)), None, 10).expect("due"),
            5_010
        );
        assert!(resolve_due_at(Some(&json!(-1)), None, 10).is_err());
        assert!(resolve_due_at(None, Some("2020-01-01T00:00:00Z"), 1_790_553_600_000).is_err());
    }
}
