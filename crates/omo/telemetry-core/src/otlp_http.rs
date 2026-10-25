//! Rust port of `packages/telemetry-core/src/otlp-http.ts`.

use std::sync::Arc;
use std::time::Duration;

use flate2::Compression;
use flate2::write::GzEncoder;
use futures::FutureExt;
use futures::future::BoxFuture;
use std::io::Write;

use crate::otlp_proto::collector_logs::decode_export_logs_service_response;
use crate::otlp_proto::collector_trace::decode_export_trace_service_response;
use crate::otlp_sleep::AbortSignal;
use crate::otlp_sleep::SleepFn;
use crate::otlp_spool_file::Signal;

pub const MAX_BODY_BYTES: usize = 256 * 1024;
pub const GZIP_THRESHOLD_BYTES: usize = 8 * 1024;
pub const BACKOFF_MS: [u64; 5] = [1_000, 2_000, 4_000, 8_000, 16_000];
pub const MAX_RETRY_AFTER_MS: u64 = 60_000;
pub const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 10_000;

/// The longest one batch's send can take: every attempt ends by its request timeout and every pause
/// between attempts is at most `MAX_RETRY_AFTER_MS`.
pub fn max_send_ms(request_timeout_ms: u64) -> u64 {
    let attempts = BACKOFF_MS.len() as u64 + 1;
    let pauses = BACKOFF_MS.len() as u64;
    attempts * request_timeout_ms + pauses * MAX_RETRY_AFTER_MS
}

const RETRYABLE: [u16; 4] = [429, 502, 503, 504];
const NON_RETRYABLE: [u16; 3] = [400, 401, 413];

#[derive(Debug, Clone, Default)]
pub struct OtlpRequest {
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct OtlpResponse {
    pub status: u16,
    pub retry_after: Option<String>,
    pub body: Vec<u8>,
}

pub type OtlpFetch = Arc<
    dyn Fn(String, OtlpRequest) -> BoxFuture<'static, Result<OtlpResponse, String>> + Send + Sync,
>;

/// The real transport, built on the crate's `reqwest` client.
pub fn create_reqwest_fetch() -> OtlpFetch {
    let client = reqwest::Client::new();
    Arc::new(move |url, request| {
        let client = client.clone();
        async move {
            let mut builder = client.post(&url).body(request.body);
            for (key, value) in request.headers {
                builder = builder.header(key, value);
            }
            let response = builder.send().await.map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let body = response
                .bytes()
                .await
                .map_err(|error| error.to_string())?
                .to_vec();
            Ok(OtlpResponse {
                status,
                retry_after,
                body,
            })
        }
        .boxed()
    })
}

/// `lost`: another process owns the batch now; `withdrawn`: telemetry consent ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptGate {
    Send,
    Lost,
    Withdrawn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialSuccess {
    pub rejected: i64,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostOutcome {
    Accepted {
        partial: Option<PartialSuccess>,
    },
    Rejected {
        status: u16,
    },
    RetryLater,
    Aborted,
    Lost,
    Withdrawn,
}

pub type AttemptGateFn = Arc<dyn Fn() -> AttemptGate + Send + Sync>;
pub type RandomFn = Arc<dyn Fn() -> f64 + Send + Sync>;

pub struct PostDeps {
    pub fetch: OtlpFetch,
    pub sleep: SleepFn,
    pub random: RandomFn,
    pub endpoint: String,
    pub ingest_key: String,
    pub abort: AbortSignal,
    pub request_timeout_ms: u64,
    /// Runs right before every attempt, the first included.
    pub before_attempt: Option<AttemptGateFn>,
}

enum Attempt {
    Done(PostOutcome),
    Retry(Option<u64>),
}

pub fn parse_retry_after(value: Option<&str>, now_ms: i64) -> Option<u64> {
    let value = value?;
    if value.trim().is_empty() {
        return None;
    }
    let millis = match value.trim().parse::<f64>() {
        Ok(seconds) if seconds.is_finite() => Some(seconds * 1_000.0),
        _ => chrono::DateTime::parse_from_rfc2822(value.trim())
            .ok()
            .map(|date| (date.timestamp_millis() - now_ms) as f64),
    }?;
    if !millis.is_finite() {
        return None;
    }
    Some(millis.clamp(0.0, MAX_RETRY_AFTER_MS as f64) as u64)
}

fn partial_success(signal: Signal, response: &OtlpResponse) -> Option<PartialSuccess> {
    if response.body.is_empty() {
        return None;
    }
    match signal {
        Signal::Logs => {
            let decoded = decode_export_logs_service_response(&response.body).ok()?;
            let partial = decoded.partial_success?;
            Some(PartialSuccess {
                rejected: partial.rejected_log_records,
                message: partial.error_message,
            })
        }
        Signal::Traces => {
            let decoded = decode_export_trace_service_response(&response.body).ok()?;
            let partial = decoded.partial_success?;
            Some(PartialSuccess {
                rejected: partial.rejected_spans,
                message: partial.error_message,
            })
        }
    }
}

fn gzip(body: &[u8]) -> Result<Vec<u8>, String> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(body)
        .map_err(|error| error.to_string())?;
    encoder.finish().map_err(|error| error.to_string())
}

async fn exchange(deps: &PostDeps, signal: Signal, body: &[u8]) -> Attempt {
    let compressed = body.len() > GZIP_THRESHOLD_BYTES;
    let mut headers = vec![
        (
            "content-type".to_string(),
            "application/x-protobuf".to_string(),
        ),
        ("x-omo-ingest-key".to_string(), deps.ingest_key.clone()),
    ];
    let payload = if compressed {
        headers.push(("content-encoding".to_string(), "gzip".to_string()));
        match gzip(body) {
            Ok(payload) => payload,
            Err(_) => return Attempt::Retry(None),
        }
    } else {
        body.to_vec()
    };
    let response = match (deps.fetch)(
        format!("{}/v1/{signal}", deps.endpoint),
        OtlpRequest {
            headers,
            body: payload,
        },
    )
    .await
    {
        Ok(response) => response,
        Err(_) => {
            return if deps.abort.is_aborted() {
                Attempt::Done(PostOutcome::Aborted)
            } else {
                Attempt::Retry(None)
            };
        }
    };
    if (200..300).contains(&response.status) {
        return Attempt::Done(PostOutcome::Accepted {
            partial: partial_success(signal, &response),
        });
    }
    if NON_RETRYABLE.contains(&response.status) {
        return Attempt::Done(PostOutcome::Rejected {
            status: response.status,
        });
    }
    if RETRYABLE.contains(&response.status) {
        return Attempt::Retry(parse_retry_after(
            response.retry_after.as_deref(),
            chrono::Utc::now().timestamp_millis(),
        ));
    }
    Attempt::Done(PostOutcome::RetryLater)
}

/// One request, response body included, bounded by the request timeout.
async fn attempt(deps: &PostDeps, signal: Signal, body: &[u8]) -> Attempt {
    match tokio::time::timeout(
        Duration::from_millis(deps.request_timeout_ms),
        exchange(deps, signal, body),
    )
    .await
    {
        Ok(attempt) => attempt,
        Err(_) => {
            if deps.abort.is_aborted() {
                Attempt::Done(PostOutcome::Aborted)
            } else {
                Attempt::Retry(None)
            }
        }
    }
}

/// One file = one request. 429/502/503/504, network errors and timed-out requests are retried after
/// 1, 2, 4, 8 and 16 s plus up to 20% jitter (Retry-After wins, capped at 60 s); 400/401/413 are never
/// retried; a 2xx, including one carrying partial_success, settles the batch.
pub async fn post_with_retry(deps: &PostDeps, signal: Signal, body: &[u8]) -> PostOutcome {
    if body.len() > MAX_BODY_BYTES {
        return PostOutcome::Rejected { status: 413 };
    }
    let mut index = 0usize;
    loop {
        if deps.abort.is_aborted() {
            return PostOutcome::Aborted;
        }
        let gate = deps
            .before_attempt
            .as_ref()
            .map_or(AttemptGate::Send, |before_attempt| before_attempt());
        match gate {
            AttemptGate::Send => {}
            AttemptGate::Lost => return PostOutcome::Lost,
            AttemptGate::Withdrawn => return PostOutcome::Withdrawn,
        }
        match attempt(deps, signal, body).await {
            Attempt::Done(outcome) => return outcome,
            Attempt::Retry(retry_after) => {
                let Some(backoff) = BACKOFF_MS.get(index).copied() else {
                    return PostOutcome::RetryLater;
                };
                let delay = retry_after.unwrap_or_else(|| {
                    backoff + ((deps.random)() * backoff as f64 * 0.2) as u64
                });
                (deps.sleep)(delay, deps.abort.clone()).await;
                index += 1;
            }
        }
    }
}
