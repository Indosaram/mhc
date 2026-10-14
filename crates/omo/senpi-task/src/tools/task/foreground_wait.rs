//! `tools/task/foreground-wait.ts`: foreground waits bounded by the prompt-cache-safe budget.
//! The TS promise race becomes a bounded `wait_for`: on deadline the task is promoted to background.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::manager::{AbortSignal, TaskManager, WaitError};
use crate::state::TaskRecord;

pub const PROMPT_CACHE_SAFE_WAIT_ENV: &str = "PI_PROMPT_CACHE_SAFE_WAIT_SECONDS";

pub type PromptCacheSafeWaitSeconds = Arc<dyn Fn() -> Option<i64> + Send + Sync>;

/// Minimal host tool context seen by the task tool.
#[derive(Clone, Default)]
pub struct TaskToolContext {
    pub cwd: String,
    pub session_id: String,
    pub get_prompt_cache_safe_wait_seconds: Option<PromptCacheSafeWaitSeconds>,
}

#[derive(Debug, Clone, Default)]
pub struct ForegroundWaitOptions {
    /// Replaces the process environment when set.
    pub env: Option<HashMap<String, String>>,
    /// Overrides the real deadline (defaults to the budget in seconds); the `scheduleDeadline` seam.
    pub deadline: Option<Duration>,
}

pub enum ForegroundWaitResult {
    Completed { record: Box<TaskRecord> },
    Promoted { budget_seconds: i64 },
}

pub struct ForegroundWaitInput<'a> {
    pub manager: &'a TaskManager,
    pub task_id: &'a str,
    pub signal: Option<&'a AbortSignal>,
    pub ctx: &'a TaskToolContext,
    pub options: ForegroundWaitOptions,
}

/// `Number.parseInt(raw, 10)`: leading whitespace, optional sign, leading digits; trailing text ignored.
fn parse_int(raw: &str) -> Option<i64> {
    let trimmed = raw.trim_start();
    let (negative, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    let value = digits[..end].bytes().fold(0_i64, |acc, byte| {
        acc.saturating_mul(10).saturating_add(i64::from(byte - b'0'))
    });
    Some(if negative { -value } else { value })
}

pub fn resolve_prompt_cache_safe_wait_seconds(
    ctx: &TaskToolContext,
    env: Option<&HashMap<String, String>>,
) -> Option<i64> {
    if let Some(getter) = &ctx.get_prompt_cache_safe_wait_seconds {
        return getter();
    }
    let raw = match env {
        Some(env) => env.get(PROMPT_CACHE_SAFE_WAIT_ENV).cloned(),
        None => std::env::var(PROMPT_CACHE_SAFE_WAIT_ENV).ok(),
    }?;
    parse_int(&raw)
}

pub fn wait_for_foreground_task(
    input: &ForegroundWaitInput<'_>,
) -> Result<ForegroundWaitResult, WaitError> {
    let budget = resolve_prompt_cache_safe_wait_seconds(input.ctx, input.options.env.as_ref());
    let Some(budget_seconds) = budget else {
        return input
            .manager
            .wait_for(input.task_id, input.signal, None)
            .map(|record| ForegroundWaitResult::Completed {
                record: Box::new(record),
            });
    };
    let deadline = input
        .options
        .deadline
        .unwrap_or_else(|| Duration::from_secs(u64::try_from(budget_seconds).unwrap_or(0)));
    let began = Instant::now();
    match input.manager.wait_for(input.task_id, input.signal, Some(deadline)) {
        Ok(record) => Ok(ForegroundWaitResult::Completed {
            record: Box::new(record),
        }),
        Err(error) => {
            let aborted = input.signal.is_some_and(AbortSignal::aborted);
            if !aborted && began.elapsed() >= deadline {
                input.manager.promote_to_background(input.task_id);
                Ok(ForegroundWaitResult::Promoted { budget_seconds })
            } else {
                Err(error)
            }
        }
    }
}
