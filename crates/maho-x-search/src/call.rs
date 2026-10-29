//! Call lifecycle (spec §4): validate, resolve the bearer, send, map the outcome, normalize.

use crate::output::{self, ToolOutput};
use crate::request::{self, HttpClient, HttpRequest};
use crate::{EnvLookup, ErrorCode, ToolFailure, availability, normalize, params};
use maho_ai::utils::abort::AbortSignal;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// Default request deadline.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(60);

/// What the host's credential store says about `xai` for this call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoredCredential {
    /// No `xai` credential stored: the env var is consulted.
    Absent,
    /// A stored credential, resolved to its API key (`Ok(None)` / blank / `Err` → no bearer, and
    /// the env var is not consulted).
    Present(Result<Option<String>, String>),
}

pub type CredentialFuture = Pin<Box<dyn Future<Output = StoredCredential> + Send>>;
/// Looks the stored credential up; called once per call (no caching).
pub type CredentialLookup = Arc<dyn Fn() -> CredentialFuture + Send + Sync>;

/// Everything a call needs from its surroundings.
#[derive(Clone)]
pub struct CallDeps {
    pub http: Arc<dyn HttpClient>,
    pub env: EnvLookup,
    pub deadline: Duration,
}

const AUTH_HINT: &str =
    "no xAI credential is available; log in to xAI with /login or set XAI_API_KEY";

/// Spec §4.2: stored credential first (no env fallback when stored), else trimmed `XAI_API_KEY`.
pub fn resolve_bearer(
    stored: StoredCredential,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    match stored {
        StoredCredential::Present(resolved) => resolved
            .ok()
            .flatten()
            .map(|key| key.trim().to_owned())
            .filter(|key| !key.is_empty()),
        StoredCredential::Absent => availability::env_api_key(env),
    }
}

fn cancelled() -> ToolFailure {
    ToolFailure::new(ErrorCode::Upstream, "the search was cancelled")
}

/// Maps an HTTP outcome to the normalized result or a failure (spec §4.6).
pub fn map_response(
    response: &request::HttpResponse,
    max_results: usize,
) -> Result<normalize::Normalized, ToolFailure> {
    match response.status {
        200..=299 => serde_json::from_str::<serde_json::Value>(&response.body)
            .map(|json| normalize::normalize(&json, max_results))
            .map_err(|error| {
                ToolFailure::new(
                    ErrorCode::Protocol,
                    format!("xAI returned a body that is not JSON ({error})"),
                )
            }),
        401 | 403 => Err(ToolFailure::new(
            ErrorCode::Auth,
            format!(
                "xAI rejected the credential (HTTP {}); log in again with /login or check XAI_API_KEY",
                response.status
            ),
        )),
        429 => Err(ToolFailure::new(
            ErrorCode::RateLimited,
            "xAI rate limit reached; try again later",
        )),
        status => Err(ToolFailure::new(
            ErrorCode::Upstream,
            format!("xAI answered HTTP {status}"),
        )),
    }
}

async fn execute(
    input: &serde_json::Value,
    deps: &CallDeps,
    lookup: &CredentialLookup,
    signal: Option<&AbortSignal>,
) -> Result<normalize::Normalized, ToolFailure> {
    let params = params::validate(input)?;
    if signal.is_some_and(AbortSignal::aborted) {
        return Err(cancelled());
    }
    let stored = lookup().await;
    let bearer = resolve_bearer(stored, deps.env.as_ref())
        .ok_or_else(|| ToolFailure::new(ErrorCode::Auth, AUTH_HINT))?;
    let model = request::choose_model(deps.env.as_ref());
    let http_request = HttpRequest {
        url: request::ENDPOINT.to_owned(),
        headers: vec![
            ("content-type".into(), "application/json".into()),
            ("authorization".into(), format!("Bearer {bearer}")),
        ],
        body: request::body(&params, &model),
    };
    if signal.is_some_and(AbortSignal::aborted) {
        return Err(cancelled());
    }
    let max_results = usize::try_from(params.max_results).unwrap_or(usize::MAX);
    let send = tokio::time::timeout(deps.deadline, deps.http.post(http_request));
    let outcome = match signal {
        Some(signal) => tokio::select! {
            biased;
            () = signal.cancelled() => return Err(cancelled()),
            outcome = send => outcome,
        },
        None => send.await,
    };
    let response = outcome
        .map_err(|_| {
            ToolFailure::new(
                ErrorCode::Timeout,
                format!("xAI did not answer within {} ms", deps.deadline.as_millis()),
            )
        })?
        .map_err(|error| {
            ToolFailure::new(
                ErrorCode::Upstream,
                format!("request to xAI failed: {error}"),
            )
        })?;
    map_response(&response, max_results)
}

/// Runs one `x_search` call end to end; failures are results with `is_error`, never panics.
pub async fn run(
    input: &serde_json::Value,
    deps: &CallDeps,
    lookup: &CredentialLookup,
    signal: Option<&AbortSignal>,
) -> ToolOutput {
    match execute(input, deps, lookup, signal).await {
        Ok(normalized) => output::success(&normalized),
        Err(failure) => output::failure(&failure),
    }
}
