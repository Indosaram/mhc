//! Port of senpi packages/coding-agent/src/core/served-credentials.ts (senpi#2491).

use maho_ai::types::{
    AssistantMessage, ProviderDiagnosticCategory, StopReason, sanitize_provider_diagnostic,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;

/// How long a served response vouches for its credential (senpi#2491).
pub const CREDENTIAL_SERVED_WINDOW_MS: i64 = 10 * 60_000;
const MAX_SERVED_CREDENTIALS: usize = 1_024;
type ServedCredentialEntries = HashMap<String, (i64, u64)>;

#[derive(Debug, Clone, Copy)]
pub struct RequestCredential<'a> {
    pub provider: &'a str,
    pub base_url: Option<&'a str>,
    pub api_key: Option<&'a str>,
}

/// When each credential last served a response, keyed by a one-way hash of provider, endpoint and key so no
/// key material is held. An `auth` failure on a credential that served within the window is marked on its
/// message: a gateway refused that request while the key kept working for others. Without an api key
/// (headers, ADC or SigV4), identity reduces to provider + base url.
pub struct ServedCredentials {
    /// Key -> (served at, insertion sequence); the sequence tracks last success for the size cap.
    served_at: Mutex<(ServedCredentialEntries, u64)>,
    now: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl Default for ServedCredentials {
    fn default() -> Self {
        Self::new(Box::new(|| chrono::Utc::now().timestamp_millis()))
    }
}

impl ServedCredentials {
    pub fn new(now: Box<dyn Fn() -> i64 + Send + Sync>) -> Self {
        Self {
            served_at: Mutex::new((HashMap::new(), 0)),
            now,
        }
    }

    /// senpi `observe(result, credential)`: records the request's final message before any consumer reads
    /// it. `None` (an extension stream without a final message) proves nothing about the credential.
    pub fn observe(
        &self,
        message: Option<&mut AssistantMessage>,
        credential: RequestCredential<'_>,
    ) {
        let Some(message) = message else { return };
        let key = credential_key(credential);
        let now = (self.now)();
        let mut guard = self.served_at.lock().unwrap_or_else(|e| e.into_inner());
        let (served, sequence) = &mut *guard;
        served.retain(|_, (served_at, _)| now - *served_at <= CREDENTIAL_SERVED_WINDOW_MS);
        if message.stop_reason != StopReason::Error && message.stop_reason != StopReason::Aborted {
            *sequence += 1;
            served.insert(key, (now, *sequence));
            if served.len() > MAX_SERVED_CREDENTIALS
                && let Some(oldest) = served
                    .iter()
                    .min_by_key(|(_, (_, seq))| *seq)
                    .map(|(key, _)| key.clone())
            {
                served.remove(&oldest);
            }
            return;
        }
        if message.stop_reason == StopReason::Error
            && served.contains_key(&key)
            && message
                .provider_diagnostic
                .as_ref()
                .and_then(sanitize_provider_diagnostic)
                .is_some_and(|diagnostic| diagnostic.category == ProviderDiagnosticCategory::Auth)
        {
            message.credential_served_recently = Some(true);
        }
    }
}

/// Owned `RequestCredential` for a stream relay.
#[derive(Debug, Clone)]
pub struct OwnedRequestCredential {
    pub provider: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
}

impl OwnedRequestCredential {
    fn borrowed(&self) -> RequestCredential<'_> {
        RequestCredential {
            provider: &self.provider,
            base_url: self.base_url.as_deref(),
            api_key: self.api_key.as_deref(),
        }
    }
}

/// senpi `servedCredentials.observe(stream.result(), credential)`. TS marks the shared message object before
/// any consumer's `then` runs; Rust consumers hold copies, so the terminal event is marked in a relay before
/// it is forwarded. A stream that fails outright proves nothing about its credential and passes through.
pub fn observe_stream(
    served: std::sync::Arc<ServedCredentials>,
    inner: maho_ai::utils::event_stream::AssistantMessageEventStream,
    credential: OwnedRequestCredential,
) -> maho_ai::utils::event_stream::AssistantMessageEventStream {
    use maho_ai::types::AssistantMessageEvent;
    let outer = maho_ai::utils::event_stream::AssistantMessageEventStream::assistant();
    outer.forward_cancellation_from(inner.clone());
    let relay = outer.clone();
    tokio::spawn(async move {
        let mut settled = false;
        loop {
            match inner.next().await {
                Ok(Some(AssistantMessageEvent::Done {
                    reason,
                    mut message,
                })) => {
                    served.observe(Some(&mut message), credential.borrowed());
                    settled = true;
                    relay.push(AssistantMessageEvent::Done { reason, message });
                }
                Ok(Some(AssistantMessageEvent::Error { reason, mut error })) => {
                    served.observe(Some(&mut error), credential.borrowed());
                    settled = true;
                    relay.push(AssistantMessageEvent::Error { reason, error });
                }
                Ok(Some(event)) => relay.push(event),
                Ok(None) => {
                    match inner.result_if_ready() {
                        Some(Ok(_)) if settled => relay.end(None),
                        Some(Ok(mut message)) => {
                            served.observe(Some(&mut message), credential.borrowed());
                            relay.end(Some(message));
                        }
                        Some(Err(error)) => relay.fail(error),
                        None => relay.end(None),
                    }
                    return;
                }
                Err(error) => {
                    relay.fail(error);
                    return;
                }
            }
        }
    });
    outer
}

fn credential_key(credential: RequestCredential<'_>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!(
        "{}\0{}\0{}",
        credential.provider,
        credential.base_url.unwrap_or(""),
        credential.api_key.unwrap_or("")
    ));
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::types::{ProviderDiagnostic, ProviderDiagnosticEvidence};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    fn blank() -> AssistantMessage {
        serde_json::from_value(serde_json::json!({
            "content": [], "api": "faux", "provider": "faux", "model": "faux-1",
            "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
            "stopReason": "stop", "timestamp": 0
        }))
        .expect("assistant")
    }

    fn message(stop_reason: StopReason, auth: bool) -> AssistantMessage {
        let mut message = blank();
        message.stop_reason = stop_reason;
        if auth {
            message.provider_diagnostic = Some(ProviderDiagnostic {
                category: ProviderDiagnosticCategory::Auth,
                http_status: Some(401),
                code: None,
                evidence: ProviderDiagnosticEvidence::StructuredStatus,
                resets_at: None,
            });
        }
        message
    }

    /// senpi test/suite/regressions/2491-request-refused.test.ts: an auth failure is marked only within the
    /// window after the same credential served.
    #[test]
    fn an_auth_failure_after_a_recent_success_is_marked() {
        let clock = Arc::new(AtomicI64::new(0));
        let reader = clock.clone();
        let served = ServedCredentials::new(Box::new(move || reader.load(Ordering::SeqCst)));
        let credential = RequestCredential {
            provider: "gw",
            base_url: Some("https://gw"),
            api_key: Some("k"),
        };
        let mut failed = message(StopReason::Error, true);
        served.observe(Some(&mut failed), credential);
        assert_eq!(failed.credential_served_recently, None);

        served.observe(Some(&mut message(StopReason::Stop, false)), credential);
        let mut failed = message(StopReason::Error, true);
        served.observe(Some(&mut failed), credential);
        assert_eq!(failed.credential_served_recently, Some(true));

        let mut other = message(StopReason::Error, true);
        served.observe(
            Some(&mut other),
            RequestCredential {
                api_key: Some("other"),
                ..credential
            },
        );
        assert_eq!(other.credential_served_recently, None);

        clock.store(CREDENTIAL_SERVED_WINDOW_MS + 1, Ordering::SeqCst);
        let mut stale = message(StopReason::Error, true);
        served.observe(Some(&mut stale), credential);
        assert_eq!(stale.credential_served_recently, None);
    }
}
