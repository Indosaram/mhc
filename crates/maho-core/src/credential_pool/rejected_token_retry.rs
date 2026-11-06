//! Port of senpi packages/coding-agent/src/core/credential-pool/rejected-token-retry.ts.
//!
//! Upstream returns an async generator over the first attempt; Rust relays into a fresh
//! `AssistantMessageEventStream`, whose `result()` follows the terminal event of the attempt that ended
//! the request, as upstream's `result()` follows `final`.

use std::sync::Arc;

use maho_ai::types::{AssistantMessageEvent, AssistantMessageEventStream, BoxFuture, ErrorReason};

pub struct RejectableAttempt {
    pub stream: AssistantMessageEventStream,
    /// The stored OAuth access token this attempt authenticated with, when the provider can refuse it.
    pub rejectable_access: Option<String>,
    /// HTTP statuses that prove `rejectable_access` was refused (`OAuthAuth.rejectedTokenStatuses`).
    pub rejected_token_statuses: Vec<u16>,
}

pub type RunRejectableAttempt =
    Arc<dyn Fn(Option<String>) -> BoxFuture<'static, RejectableAttempt> + Send + Sync>;

// Account-scoped wording: the credential pool reads it as a per-account 403, blocks the slot until a new
// login, and fails over to a healthy sibling account.
pub const REFUSED_AFTER_REEXCHANGE: &str = "A freshly re-exchanged token for this account was refused as well; log in to this account again or switch accounts.";

fn refuses_token(event: &AssistantMessageEvent, statuses: &[u16]) -> bool {
    let AssistantMessageEvent::Error {
        reason: ErrorReason::Error,
        error,
    } = event
    else {
        return false;
    };
    let status = error
        .provider_diagnostic
        .as_ref()
        .and_then(|diagnostic| diagnostic.http_status);
    status.is_some_and(|status| statuses.contains(&status))
}

fn is_committed_output(event: &AssistantMessageEvent) -> bool {
    matches!(
        event,
        AssistantMessageEvent::TextDelta { .. }
            | AssistantMessageEvent::ThinkingDelta { .. }
            | AssistantMessageEvent::ToolcallDelta { .. }
            | AssistantMessageEvent::ToolcallEnd { .. }
    )
}

/// Relays `source` to `target` until it ends or fails.
async fn relay(
    source: &AssistantMessageEventStream,
    target: &AssistantMessageEventStream,
    mark_refusal: Option<&[u16]>,
) {
    let mut committed = false;
    loop {
        match source.next().await {
            Ok(Some(mut event)) => {
                if let Some(statuses) = mark_refusal
                    && !committed
                    && refuses_token(&event, statuses)
                    && let AssistantMessageEvent::Error { error, .. } = &mut event
                {
                    let message = error
                        .error_message
                        .clone()
                        .unwrap_or_else(|| "provider stream error".to_owned());
                    error.error_message = Some(format!("{message}\n{REFUSED_AFTER_REEXCHANGE}"));
                }
                committed |= is_committed_output(&event);
                target.push(event);
            }
            Ok(None) => {
                target.end(None);
                return;
            }
            Err(error) => {
                target.fail(error);
                return;
            }
        }
    }
}

/// Runs one request attempt and, when the provider refuses the stored OAuth token before any output reached
/// the caller, runs it exactly once more with that token named as rejected so auth resolution re-exchanges
/// it. Pre-commit frames are held back so the caller sees one stream, never a start from an attempt that was
/// replaced. A provider that declares no refusal statuses gets its own stream back untouched.
pub async fn retry_once_on_rejected_token(
    attempt: RunRejectableAttempt,
) -> AssistantMessageEventStream {
    let first = attempt(None).await;
    let statuses = first.rejected_token_statuses.clone();
    let Some(rejected_access) = first
        .rejectable_access
        .clone()
        .filter(|_| !statuses.is_empty())
    else {
        return first.stream;
    };
    let target = AssistantMessageEventStream::assistant();
    let output = target.clone();
    tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            match first.stream.next().await {
                Ok(Some(event)) if refuses_token(&event, &statuses) => {
                    let _ = first.stream.cancel().await;
                    let retried = attempt(Some(rejected_access)).await;
                    relay(&retried.stream, &target, Some(&statuses)).await;
                    return;
                }
                Ok(Some(event)) if is_committed_output(&event) => {
                    for held_event in held.drain(..) {
                        target.push(held_event);
                    }
                    target.push(event);
                    relay(&first.stream, &target, None).await;
                    return;
                }
                Ok(Some(event)) => held.push(event),
                Ok(None) => {
                    for held_event in held.drain(..) {
                        target.push(held_event);
                    }
                    target.end(None);
                    return;
                }
                Err(error) => {
                    for held_event in held.drain(..) {
                        target.push(held_event);
                    }
                    target.fail(error);
                    return;
                }
            }
        }
    });
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::types::{
        AssistantMessage, DoneReason, ProviderDiagnostic, ProviderDiagnosticCategory,
        ProviderDiagnosticEvidence, StopReason, Usage,
    };
    use std::sync::Mutex;

    fn message(stop_reason: StopReason) -> AssistantMessage {
        AssistantMessage {
            credential_served_recently: None,
            error_cause: None,
            prompt_cache_breakpoint: None,
            prompt_cache_diagnostics: None,
            thinking_level: None,
            provider_diagnostic: None,
            content: Vec::new(),
            api: "faux".into(),
            provider: "faux".into(),
            model: "m".into(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason,
            stop_details: None,
            deferred: None,
            error_message: None,
            abort_source: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0,
            failure_source: None,
            timing: None,
        }
    }

    fn refused(status: u16) -> AssistantMessageEvent {
        let mut error = message(StopReason::Error);
        error.error_message = Some("401 unauthorized".into());
        error.provider_diagnostic = Some(ProviderDiagnostic {
            category: ProviderDiagnosticCategory::Auth,
            http_status: Some(status),
            code: None,
            evidence: ProviderDiagnosticEvidence::StructuredStatus,
            resets_at: None,
        });
        AssistantMessageEvent::Error {
            reason: ErrorReason::Error,
            error,
        }
    }

    fn done() -> AssistantMessageEvent {
        AssistantMessageEvent::Done {
            reason: DoneReason::Stop,
            message: message(StopReason::Stop),
        }
    }

    fn start() -> AssistantMessageEvent {
        AssistantMessageEvent::Start {
            partial: message(StopReason::Stop),
        }
    }

    fn stream_of(events: Vec<AssistantMessageEvent>) -> AssistantMessageEventStream {
        let stream = AssistantMessageEventStream::assistant();
        for event in events {
            stream.push(event);
        }
        stream
    }

    type Calls = Arc<Mutex<Vec<Option<String>>>>;

    fn attempts(
        scripts: Vec<Vec<AssistantMessageEvent>>,
        statuses: Vec<u16>,
    ) -> (RunRejectableAttempt, Calls) {
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        let scripts = Arc::new(Mutex::new(scripts.into_iter()));
        let seen = calls.clone();
        let run: RunRejectableAttempt = Arc::new(move |rejected| {
            if let Ok(mut seen) = seen.lock() {
                seen.push(rejected);
            }
            let events = scripts
                .lock()
                .ok()
                .and_then(|mut scripts| scripts.next())
                .unwrap_or_default();
            let statuses = statuses.clone();
            Box::pin(async move {
                RejectableAttempt {
                    stream: stream_of(events),
                    rejectable_access: Some("token-1".into()),
                    rejected_token_statuses: statuses,
                }
            })
        });
        (run, calls)
    }

    fn kinds(events: &[AssistantMessageEvent]) -> Vec<&'static str> {
        events
            .iter()
            .map(|event| match event {
                AssistantMessageEvent::Start { .. } => "start",
                AssistantMessageEvent::Done { .. } => "done",
                AssistantMessageEvent::Error { .. } => "error",
                _ => "other",
            })
            .collect()
    }

    #[tokio::test]
    async fn retries_once_with_the_refused_token_named_and_hides_the_replaced_start() {
        let (run, calls) = attempts(
            vec![vec![start(), refused(401)], vec![start(), done()]],
            vec![401, 403],
        );
        let stream = retry_once_on_rejected_token(run).await;
        let events = stream.collect().await.unwrap_or_default();
        assert_eq!(kinds(&events), vec!["start", "done"]);
        assert_eq!(
            calls.lock().map(|c| c.clone()).unwrap_or_default(),
            vec![None, Some("token-1".to_owned())]
        );
        assert_eq!(
            stream.result().await.map(|m| m.stop_reason),
            Ok(StopReason::Stop)
        );
    }

    #[tokio::test]
    async fn marks_a_second_refusal_as_account_scoped() {
        let (run, calls) = attempts(vec![vec![refused(403)], vec![refused(403)]], vec![403]);
        let events = retry_once_on_rejected_token(run)
            .await
            .collect()
            .await
            .unwrap_or_default();
        assert_eq!(calls.lock().map(|c| c.len()).unwrap_or(0), 2);
        let Some(AssistantMessageEvent::Error { error, .. }) = events.last() else {
            panic!("no error")
        };
        assert_eq!(
            error.error_message.as_deref(),
            Some(format!("401 unauthorized\n{REFUSED_AFTER_REEXCHANGE}").as_str())
        );
    }

    #[tokio::test]
    async fn returns_the_stream_untouched_when_the_provider_declares_no_refusal_statuses() {
        let (run, calls) = attempts(vec![vec![refused(401)]], Vec::new());
        let events = retry_once_on_rejected_token(run)
            .await
            .collect()
            .await
            .unwrap_or_default();
        assert_eq!(kinds(&events), vec!["error"]);
        assert_eq!(calls.lock().map(|c| c.len()).unwrap_or(0), 1);
    }

    #[tokio::test]
    async fn does_not_retry_a_status_outside_the_declared_set() {
        let (run, calls) = attempts(vec![vec![start(), refused(500)]], vec![401]);
        let events = retry_once_on_rejected_token(run)
            .await
            .collect()
            .await
            .unwrap_or_default();
        assert_eq!(kinds(&events), vec!["start", "error"]);
        assert_eq!(calls.lock().map(|c| c.len()).unwrap_or(0), 1);
    }
}
