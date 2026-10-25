//! Ports of `otlp-retry.test.ts`, `otlp-retry-sleep.test.ts`, and the retry rules of `otlp-http.ts`.

use std::sync::Arc;
use std::sync::Mutex;

use futures::FutureExt;
use telemetry_core::AbortSignal;
use telemetry_core::AttemptGate;
use telemetry_core::MAX_BODY_BYTES;
use telemetry_core::OtlpFetch;
use telemetry_core::OtlpResponse;
use telemetry_core::PostDeps;
use telemetry_core::PostOutcome;
use telemetry_core::Signal;
use telemetry_core::max_send_ms;
use telemetry_core::parse_retry_after;
use telemetry_core::post_with_retry;

fn deps(fetch: OtlpFetch, delays: Arc<Mutex<Vec<u64>>>) -> PostDeps {
    PostDeps {
        fetch,
        sleep: Arc::new(move |ms: u64, _abort: AbortSignal| {
            delays.lock().unwrap().push(ms);
            futures::future::ready(()).boxed()
        }),
        random: Arc::new(|| 0.0),
        endpoint: "https://collector.test".to_string(),
        ingest_key: "test-ingest-key".to_string(),
        abort: AbortSignal::new(),
        request_timeout_ms: 10_000,
        before_attempt: None,
    }
}

fn status_fetch(status: u16, retry_after: Option<&str>, calls: Arc<Mutex<usize>>) -> OtlpFetch {
    let retry_after = retry_after.map(str::to_string);
    Arc::new(move |_url: String, _request| {
        *calls.lock().unwrap() += 1;
        let retry_after = retry_after.clone();
        async move {
            Ok::<OtlpResponse, String>(OtlpResponse {
                status,
                retry_after,
                body: Vec::new(),
            })
        }
        .boxed()
    })
}

#[test]
fn retry_after_reads_seconds_and_caps_at_a_minute() {
    assert_eq!(parse_retry_after(Some("2"), 0), Some(2_000));
    assert_eq!(parse_retry_after(Some("   "), 0), None);
    assert_eq!(parse_retry_after(None, 0), None);
    assert_eq!(parse_retry_after(Some("not-a-date"), 0), None);
    assert_eq!(parse_retry_after(Some("999"), 0), Some(60_000));
}

#[test]
fn the_longest_send_covers_every_attempt_and_every_pause() {
    assert_eq!(max_send_ms(10_000), 6 * 10_000 + 5 * 60_000);
}

#[tokio::test]
async fn a_retryable_status_retries_five_times_and_then_reports_retry_later() {
    let calls = Arc::new(Mutex::new(0usize));
    let delays = Arc::new(Mutex::new(Vec::new()));
    let deps = deps(status_fetch(503, None, Arc::clone(&calls)), Arc::clone(&delays));

    let outcome = post_with_retry(&deps, Signal::Logs, b"body").await;

    assert_eq!(outcome, PostOutcome::RetryLater);
    assert_eq!(*calls.lock().unwrap(), 6);
    assert_eq!(*delays.lock().unwrap(), vec![1_000, 2_000, 4_000, 8_000, 16_000]);
}

#[tokio::test]
async fn a_429_retry_after_wins_over_the_backoff() {
    let calls = Arc::new(Mutex::new(0usize));
    let delays = Arc::new(Mutex::new(Vec::new()));
    let deps = deps(
        status_fetch(429, Some("7"), Arc::clone(&calls)),
        Arc::clone(&delays),
    );

    let outcome = post_with_retry(&deps, Signal::Logs, b"body").await;

    assert_eq!(outcome, PostOutcome::RetryLater);
    assert_eq!(*delays.lock().unwrap(), vec![7_000; 5]);
}

#[tokio::test]
async fn a_non_retryable_status_is_rejected_without_a_second_attempt() {
    let calls = Arc::new(Mutex::new(0usize));
    let delays = Arc::new(Mutex::new(Vec::new()));
    let deps = deps(status_fetch(400, None, Arc::clone(&calls)), Arc::clone(&delays));

    let outcome = post_with_retry(&deps, Signal::Logs, b"body").await;

    assert_eq!(outcome, PostOutcome::Rejected { status: 400 });
    assert_eq!(*calls.lock().unwrap(), 1);
    assert!(delays.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_body_over_the_limit_is_rejected_before_any_request() {
    let calls = Arc::new(Mutex::new(0usize));
    let delays = Arc::new(Mutex::new(Vec::new()));
    let deps = deps(status_fetch(200, None, Arc::clone(&calls)), Arc::clone(&delays));
    let body = vec![0u8; MAX_BODY_BYTES + 1];

    let outcome = post_with_retry(&deps, Signal::Logs, &body).await;

    assert_eq!(outcome, PostOutcome::Rejected { status: 413 });
    assert_eq!(*calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn the_attempt_gate_decides_whether_a_batch_is_still_ours_to_send() {
    for (gate, expected) in [
        (AttemptGate::Lost, PostOutcome::Lost),
        (AttemptGate::Withdrawn, PostOutcome::Withdrawn),
    ] {
        let calls = Arc::new(Mutex::new(0usize));
        let delays = Arc::new(Mutex::new(Vec::new()));
        let mut deps = deps(status_fetch(200, None, Arc::clone(&calls)), Arc::clone(&delays));
        deps.before_attempt = Some(Arc::new(move || gate));

        let outcome = post_with_retry(&deps, Signal::Logs, b"body").await;

        assert_eq!(outcome, expected);
        assert_eq!(*calls.lock().unwrap(), 0);
    }
}

#[tokio::test]
async fn an_aborted_signal_stops_before_the_first_request() {
    let calls = Arc::new(Mutex::new(0usize));
    let delays = Arc::new(Mutex::new(Vec::new()));
    let deps = deps(status_fetch(200, None, Arc::clone(&calls)), Arc::clone(&delays));
    deps.abort.abort();

    let outcome = post_with_retry(&deps, Signal::Logs, b"body").await;

    assert_eq!(outcome, PostOutcome::Aborted);
    assert_eq!(*calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn a_2xx_settles_the_batch() {
    let calls = Arc::new(Mutex::new(0usize));
    let delays = Arc::new(Mutex::new(Vec::new()));
    let deps = deps(status_fetch(204, None, Arc::clone(&calls)), Arc::clone(&delays));

    let outcome = post_with_retry(&deps, Signal::Traces, b"body").await;

    assert_eq!(outcome, PostOutcome::Accepted { partial: None });
    assert_eq!(*calls.lock().unwrap(), 1);
}
