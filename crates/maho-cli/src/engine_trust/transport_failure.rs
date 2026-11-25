//! Port of `packages/coding-agent/src/engine-trust/transport-failure.ts` (senpi#5244).

use std::collections::HashSet;
use std::path::Path;
use std::sync::{LazyLock, Mutex, PoisonError};

use super::actions::{EngineTrustActionId, engine_trust_action};
use super::adopted::adopted_engine_trust;
use super::marker::{EngineTrustAutomatic, EngineTrustFallbackReason, EngineTrustMarker};

pub const TLS_CHAIN_VERIFICATION_CODES: [&str; 6] = [
    "UNABLE_TO_GET_ISSUER_CERT",
    "UNABLE_TO_GET_ISSUER_CERT_LOCALLY",
    "UNABLE_TO_VERIFY_LEAF_SIGNATURE",
    "DEPTH_ZERO_SELF_SIGNED_CERT",
    "SELF_SIGNED_CERT_IN_CHAIN",
    "CERT_UNTRUSTED",
];

pub fn is_tls_chain_verification_code(code: &str) -> bool {
    TLS_CHAIN_VERIFICATION_CODES.contains(&code)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticEvidence {
    StructuredTransport,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderTransportDiagnostic {
    pub evidence: DiagnosticEvidence,
    pub code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportTrustFailure {
    pub code: &'static str,
    pub reason: EngineTrustFallbackReason,
    pub action: EngineTrustActionId,
}

static RECORDED_REASONS: LazyLock<Mutex<HashSet<EngineTrustFallbackReason>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

pub fn transport_trust_failure_with_marker(
    diagnostic: Option<&ProviderTransportDiagnostic>,
    trust: Option<&EngineTrustMarker>,
    record_once: bool,
) -> Option<TransportTrustFailure> {
    let diagnostic = diagnostic?;
    if diagnostic.evidence != DiagnosticEvidence::StructuredTransport {
        return None;
    }
    let code = diagnostic.code.as_deref()?;
    if !is_tls_chain_verification_code(code) {
        return None;
    }
    let trust = trust?;
    if trust.automatic != EngineTrustAutomatic::Fallback {
        return None;
    }
    let reason = trust.fallback_reason?;
    if record_once {
        let should_record = RECORDED_REASONS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(reason);
        if should_record {
            append_diagnostics_record(&trust.file, &trust.sha256, reason);
        }
    }
    Some(TransportTrustFailure {
        code: "trust-tls-failed-under-fallback",
        reason,
        action: engine_trust_action(reason).id,
    })
}

pub fn transport_trust_failure(
    diagnostic: Option<&ProviderTransportDiagnostic>,
) -> Option<TransportTrustFailure> {
    transport_trust_failure_with_marker(diagnostic, adopted_engine_trust().as_ref(), true)
}

fn append_diagnostics_record(
    generation_file: &str,
    sha256: &str,
    reason: EngineTrustFallbackReason,
) {
    let Some(parent) = Path::new(generation_file).parent() else {
        return;
    };
    let log_path = parent.join("diagnostics.jsonl");
    let line = serde_json::json!({
        "v": 1,
        "at": chrono::Utc::now().to_rfc3339(),
        "pid": std::process::id(),
        "generation": sha256,
        "codes": ["trust-tls-failed-under-fallback", reason.as_str()],
    });
    let payload = format!("{line}\n");
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            #[cfg(unix)]
            {
                use tokio::io::AsyncWriteExt;
                let mut opts = tokio::fs::OpenOptions::new();
                opts.create(true).append(true).mode(0o600);
                if let Ok(mut file) = opts.open(&log_path).await {
                    let _ = file.write_all(payload.as_bytes()).await;
                }
            }
            #[cfg(not(unix))]
            {
                use tokio::io::AsyncWriteExt;
                let mut opts = tokio::fs::OpenOptions::new();
                opts.create(true).append(true);
                if let Ok(mut file) = opts.open(&log_path).await {
                    let _ = file.write_all(payload.as_bytes()).await;
                }
            }
        });
    }
}
