//! Port of senpi packages/coding-agent/src/core/provider-timeout-retry.ts.

use maho_agent::agent::AgentContinuationOptions;
use maho_ai::types::AssistantMessage;
use maho_ai::utils::retry::is_provider_timeout_error;

#[derive(Clone, Default)]
pub struct ProviderTimeoutRetryPlan {
    pub options: AgentContinuationOptions,
    pub watchdog_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderTimeoutRetryPlanInput {
    pub stream_retry_timeout_ms: Option<u64>,
    pub timeout_ms: Option<u64>,
    pub stream_start_timeout_ms: Option<u64>,
}

/// Raises the retry-continuation liveness cap to the stream-start budget granted to the same retry,
/// so the watchdog can never cancel an attempt on a deadline the attempt was never given. An
/// explicitly disabled cap stays disabled; a cap that already outlasts the guard is unchanged.
pub fn reconcile_watchdog_timeout_ms(stream_retry_timeout_ms: Option<u64>, stream_start_timeout_ms: Option<u64>) -> Option<u64> {
    let stream_retry = stream_retry_timeout_ms.filter(|value| *value > 0)?;
    let Some(stream_start) = stream_start_timeout_ms else { return Some(stream_retry) };
    Some(stream_retry.max((stream_start as f64 * 1.1).ceil() as u64))
}

pub fn create_provider_timeout_retry_plan(
    message: &AssistantMessage,
    input: ProviderTimeoutRetryPlanInput,
) -> ProviderTimeoutRetryPlan {
    if !is_provider_timeout_error(message) {
        return ProviderTimeoutRetryPlan::default();
    }
    ProviderTimeoutRetryPlan {
        options: AgentContinuationOptions {
            defer_queued_messages: Some(true),
            timeout_ms: input.timeout_ms,
            stream_start_timeout_ms: input.stream_start_timeout_ms,
        },
        watchdog_timeout_ms: reconcile_watchdog_timeout_ms(input.stream_retry_timeout_ms, input.stream_start_timeout_ms),
    }
}

/// Runs a retry continuation under a bounded watchdog: when timeout_ms elapses and the active
/// signal is still the one owned by this continuation, abort_active() is called.
pub async fn run_bounded_retry_continuation<C, S, A>(
    timeout_ms: Option<u64>,
    mut continue_run: C,
    mut get_active_signal: S,
    mut abort_active: A,
) where
    C: FnMut() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
    S: FnMut() -> Option<u64>,
    A: FnMut(),
{
    let owned_signal = get_active_signal();
    let continuation = continue_run();
    match (timeout_ms, owned_signal) {
        (Some(timeout_ms), Some(owned)) => {
            let watch = async {
                tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)).await;
                if get_active_signal() == Some(owned) {
                    abort_active();
                }
            };
            tokio::select! {
                _ = continuation => {}
                _ = watch => {}
            }
        }
        _ => continuation.await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant(stop_reason: &str, error_message: Option<&str>) -> AssistantMessage {
        serde_json::from_value(json!({
            "content": [], "api": "faux", "provider": "faux", "model": "faux-1",
            "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
            "stopReason": stop_reason, "timestamp": 0,
            "errorMessage": error_message
        }))
        .expect("assistant")
    }

    #[test]
    fn a_disabled_cap_stays_disabled() {
        assert_eq!(reconcile_watchdog_timeout_ms(Some(0), Some(1000)), None);
        assert_eq!(reconcile_watchdog_timeout_ms(None, Some(1000)), None);
    }

    #[test]
    fn a_missing_stream_start_guard_keeps_the_cap() {
        assert_eq!(reconcile_watchdog_timeout_ms(Some(30000), None), Some(30000));
    }

    #[test]
    fn the_cap_absorbs_the_stream_start_guard_with_grace() {
        assert_eq!(reconcile_watchdog_timeout_ms(Some(30000), Some(90000)), Some(99001));
        assert_eq!(reconcile_watchdog_timeout_ms(Some(120000), Some(1000)), Some(120000));
    }

    #[test]
    fn a_non_timeout_message_produces_an_empty_plan() {
        let plan = create_provider_timeout_retry_plan(
            &assistant("stop", None),
            ProviderTimeoutRetryPlanInput { stream_retry_timeout_ms: Some(1), timeout_ms: Some(2), stream_start_timeout_ms: Some(3) },
        );
        assert_eq!(plan.watchdog_timeout_ms, None);
        assert_eq!(plan.options.defer_queued_messages, None);
    }

    #[tokio::test]
    async fn the_watchdog_aborts_a_wedged_continuation() {
        let aborted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&aborted);
        run_bounded_retry_continuation(
            Some(1),
            || Box::pin(std::future::pending::<()>()) as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
            || Some(1u64),
            move || flag.store(true, std::sync::atomic::Ordering::SeqCst),
        )
        .await;
        assert!(aborted.load(std::sync::atomic::Ordering::SeqCst));
    }
}
