//! Port of senpi `anthropic-subscription/auth-attempt.ts` (pin 903b8336).
use maho_core::subscription_limits::{
    limit_store::{SubscriptionLimitStore, subscription_limits},
    limit_windows::claude_windows_from_sdk_event,
};
use serde_json::Value;
/// `recordingLimitWindows`: the SDK reports the account's limit windows as `rate_limit_event` messages, kept for
/// the account the attempt authenticated with (senpi#3370).
pub fn record_limit_windows(message: &Value, account_name: &str) {
    record_limit_windows_in(
        subscription_limits(),
        message,
        account_name,
        chrono::DateTime::from_timestamp_millis(maho_ai::utils::diagnostics::now_ms())
            .unwrap_or_default(),
    );
}
/// [`record_limit_windows`] against an explicit limit store and time.
pub fn record_limit_windows_in(
    store: &SubscriptionLimitStore,
    message: &Value,
    account_name: &str,
    now: chrono::DateTime<chrono::Utc>,
) {
    if message["type"] == "rate_limit_event" {
        store.record(
            crate::account_management::ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,
            account_name,
            &claude_windows_from_sdk_event(message),
            now,
        );
    }
}
/// Owns the cleanup rule while a native attempt's stream is being consumed.
pub struct AttemptGuard<F: FnOnce()> {
    cleanup: Option<F>,
    retain_on_success: bool,
}
impl<F: FnOnce()> AttemptGuard<F> {
    pub fn retainable(discard: F) -> Self {
        Self {
            cleanup: Some(discard),
            retain_on_success: true,
        }
    }
    pub fn closing(close: F) -> Self {
        Self {
            cleanup: Some(close),
            retain_on_success: false,
        }
    }
    /// Call only after the stream reaches successful EOF, never on a result frame.
    pub fn complete(&mut self) {
        if self.retain_on_success {
            self.cleanup = None;
        }
    }
}
impl<F: FnOnce()> Drop for AttemptGuard<F> {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    #[test]
    fn rate_limit_events_record_the_attempt_account_windows() {
        let store = SubscriptionLimitStore::new();
        let now = chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("time");
        record_limit_windows_in(
            &store,
            &serde_json::json!({"type":"stream_event"}),
            "work",
            now,
        );
        assert!(store.snapshot().is_empty());
        record_limit_windows_in(
            &store,
            &serde_json::json!({"type":"rate_limit_event","rate_limit_info":{"rateLimitType":"five_hour","utilization":0.25,"resetsAt":1_700_003_600}}),
            "work",
            now,
        );
        let limits = store.snapshot();
        assert_eq!(limits.len(), 1);
        assert_eq!(
            (limits[0].provider.as_str(), limits[0].account.as_str()),
            ("anthropic-subscription", "work")
        );
        assert_eq!(limits[0].windows[0].id, "claude:5h");
        assert_eq!(limits[0].windows[0].used_percent, 25.0);
    }
    #[test]
    fn only_successful_consumption_retains_attempt() {
        let calls = Arc::new(AtomicUsize::new(0));
        let callback = || {
            let calls = calls.clone();
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
            }
        };
        {
            let _abandoned = AttemptGuard::retainable(callback());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        {
            let mut retained = AttemptGuard::retainable(callback());
            retained.complete();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        {
            let mut closing = AttemptGuard::closing(callback());
            closing.complete();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
