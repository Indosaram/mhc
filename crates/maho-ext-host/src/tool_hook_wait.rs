//! How long the host waits on one tool hook handler (`senpi#7069` / `oh-my-openagent#14354`).
//!
//! A `tool_call` / `tool_result` handler runs in-process. One that never settles used to hold the
//! tool loop forever: the await had no deadline and ignored the turn's abort signal, so neither Esc
//! nor an RPC interrupt ended it. The wait now ends when the handler settles, when the turn is
//! aborted, or (for `tool_result`) at a deadline.
//!
//! Native difference: a JavaScript handler keeps running detached after the host stops waiting on
//! it. A native handler future borrows the emission's event and context, so dropping it on timeout
//! or abort cancels it at its current await point.

use maho_ext_api::AbortSignal;
use std::future::Future;
use std::time::Duration;
use tokio::time::Instant;

/// The builtin hooks extension runs user command hooks under their own configured timeout, so it
/// gets no host deadline.
pub const SELF_BOUNDED_TOOL_HOOK_PATH: &str = "<builtin:hooks>";

/// Default deadline for one `tool_result` handler of an extension that does not bound itself.
pub const DEFAULT_TOOL_RESULT_HANDLER_TIMEOUT_MS: u64 = 120_000;

pub const TOOL_RESULT_HANDLER_TIMEOUT_ENV: &str = "SENPI_TOOL_RESULT_HANDLER_TIMEOUT_MS";
pub const MAHO_TOOL_RESULT_HANDLER_TIMEOUT_ENV: &str = "MAHO_TOOL_RESULT_HANDLER_TIMEOUT_MS";

/// The `tool_result` deadline for an extension: 0 (none) for a self-bounded one, else the env
/// override or the default.
pub fn tool_result_handler_timeout_ms(extension_path: &str) -> u64 {
    let raw = std::env::var(MAHO_TOOL_RESULT_HANDLER_TIMEOUT_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var(TOOL_RESULT_HANDLER_TIMEOUT_ENV)
                .ok()
                .filter(|value| !value.trim().is_empty())
        });
    tool_result_handler_timeout_ms_from(extension_path, raw.as_deref())
}

pub fn tool_result_handler_timeout_ms_from(extension_path: &str, env_value: Option<&str>) -> u64 {
    if extension_path == SELF_BOUNDED_TOOL_HOOK_PATH {
        return 0;
    }
    let Some(raw) = env_value.map(str::trim).filter(|value| !value.is_empty()) else {
        return DEFAULT_TOOL_RESULT_HANDLER_TIMEOUT_MS;
    };
    match raw.parse::<f64>() {
        Ok(parsed) if parsed.is_finite() && parsed >= 0.0 => {
            if parsed >= u64::MAX as f64 {
                u64::MAX
            } else {
                parsed as u64
            }
        }
        _ => DEFAULT_TOOL_RESULT_HANDLER_TIMEOUT_MS,
    }
}

pub enum ToolHookWaitOutcome<T> {
    Settled(T),
    TimedOut { elapsed_ms: u64 },
    Aborted,
}

/// Waits on one handler until it settles, the signal aborts, or the deadline passes, whichever
/// comes first. `timeout_ms == 0` waits without a deadline.
pub async fn wait_for_tool_hook<T>(
    handler: impl Future<Output = T>,
    signal: Option<&AbortSignal>,
    timeout_ms: u64,
) -> ToolHookWaitOutcome<T> {
    if signal.is_some_and(AbortSignal::is_aborted) {
        return ToolHookWaitOutcome::Aborted;
    }
    if signal.is_none() && timeout_ms == 0 {
        return ToolHookWaitOutcome::Settled(handler.await);
    }
    let started_at = Instant::now();
    let aborted = async {
        match signal {
            Some(signal) => signal.cancelled().await,
            None => std::future::pending::<()>().await,
        }
    };
    let timeout = async {
        if timeout_ms > 0 {
            tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    tokio::pin!(handler);
    tokio::pin!(aborted);
    tokio::pin!(timeout);
    tokio::select! {
        biased;
        value = &mut handler => ToolHookWaitOutcome::Settled(value),
        () = &mut aborted => ToolHookWaitOutcome::Aborted,
        () = &mut timeout => ToolHookWaitOutcome::TimedOut {
            elapsed_ms: u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_resolution_respects_self_bounded_and_env() {
        assert_eq!(
            tool_result_handler_timeout_ms_from("<builtin:hooks>", Some("500")),
            0
        );
        assert_eq!(
            tool_result_handler_timeout_ms_from("<inline:1>", None),
            DEFAULT_TOOL_RESULT_HANDLER_TIMEOUT_MS
        );
        assert_eq!(
            tool_result_handler_timeout_ms_from("<inline:1>", Some("  ")),
            DEFAULT_TOOL_RESULT_HANDLER_TIMEOUT_MS
        );
        assert_eq!(
            tool_result_handler_timeout_ms_from("<inline:1>", Some("250")),
            250
        );
        assert_eq!(
            tool_result_handler_timeout_ms_from("<inline:1>", Some("0")),
            0
        );
        assert_eq!(
            tool_result_handler_timeout_ms_from("<inline:1>", Some("-5")),
            DEFAULT_TOOL_RESULT_HANDLER_TIMEOUT_MS
        );
        assert_eq!(
            tool_result_handler_timeout_ms_from("<inline:1>", Some("not-a-number")),
            DEFAULT_TOOL_RESULT_HANDLER_TIMEOUT_MS
        );
    }

    #[tokio::test(start_paused = true)]
    async fn wait_returns_aborted_when_signal_already_aborted() {
        let signal = AbortSignal::default();
        signal.abort();
        let outcome = wait_for_tool_hook(async { 1 }, Some(&signal), 1_000).await;
        assert!(matches!(outcome, ToolHookWaitOutcome::Aborted));
    }

    #[tokio::test(start_paused = true)]
    async fn wait_times_out_pending_handler() {
        let outcome = wait_for_tool_hook(std::future::pending::<i32>(), None, 500).await;
        assert!(matches!(
            outcome,
            ToolHookWaitOutcome::TimedOut { elapsed_ms: 500 }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn wait_aborts_pending_handler_on_signal() {
        let signal = AbortSignal::default();
        let trigger = signal.clone();
        let waiter = tokio::spawn(async move {
            wait_for_tool_hook(std::future::pending::<i32>(), Some(&signal), 60_000).await
        });
        tokio::task::yield_now().await;
        trigger.abort();
        let outcome = waiter.await.expect("join");
        assert!(matches!(outcome, ToolHookWaitOutcome::Aborted));
    }
}
