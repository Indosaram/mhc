//! Port of `packages/coding-agent/src/engine-trust/shell-trust.ts` (omo#10762).

use std::sync::LazyLock;

use regex::Regex;

use super::actions::{EngineTrustActionId, engine_trust_action};
use super::adopted::adopted_engine_trust;
use super::marker::{EngineTrustAutomatic, EngineTrustFallbackReason};

pub const SHELL_TRUST_PATTERN_IDS: [&str; 12] = [
    "UNABLE_TO_GET_ISSUER_CERT_LOCALLY",
    "UNABLE_TO_GET_ISSUER_CERT",
    "UNABLE_TO_VERIFY_LEAF_SIGNATURE",
    "SELF_SIGNED_CERT_IN_CHAIN",
    "DEPTH_ZERO_SELF_SIGNED_CERT",
    "CERT_UNTRUSTED",
    "unable-to-verify-the-first-certificate",
    "unable-to-get-local-issuer-certificate",
    "unable-to-get-issuer-certificate",
    "self-signed-certificate-in-certificate-chain",
    "self-signed-certificate",
    "certificate-not-trusted",
];

static PATTERNS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    let tok = |code: &str| {
        Regex::new(&format!(r"(?:^|[^A-Za-z0-9_]){code}(?:$|[^A-Za-z0-9_])"))
            .expect("valid token regex")
    };
    let icase = |pat: &str| Regex::new(&format!("(?i){pat}")).expect("valid icase regex");
    vec![
        (
            "UNABLE_TO_GET_ISSUER_CERT_LOCALLY",
            tok("UNABLE_TO_GET_ISSUER_CERT_LOCALLY"),
        ),
        (
            "UNABLE_TO_GET_ISSUER_CERT",
            tok("UNABLE_TO_GET_ISSUER_CERT"),
        ),
        (
            "UNABLE_TO_VERIFY_LEAF_SIGNATURE",
            tok("UNABLE_TO_VERIFY_LEAF_SIGNATURE"),
        ),
        (
            "SELF_SIGNED_CERT_IN_CHAIN",
            tok("SELF_SIGNED_CERT_IN_CHAIN"),
        ),
        (
            "DEPTH_ZERO_SELF_SIGNED_CERT",
            tok("DEPTH_ZERO_SELF_SIGNED_CERT"),
        ),
        ("CERT_UNTRUSTED", tok("CERT_UNTRUSTED")),
        (
            "unable-to-verify-the-first-certificate",
            icase(r"unable to verify the first certificate"),
        ),
        (
            "unable-to-get-local-issuer-certificate",
            icase(r"unable to get local issuer certificate"),
        ),
        (
            "unable-to-get-issuer-certificate",
            icase(r"unable to get issuer certificate"),
        ),
        (
            "self-signed-certificate-in-certificate-chain",
            icase(r"self[- ]signed certificate in certificate chain"),
        ),
        (
            "self-signed-certificate",
            icase(r"self[- ]signed certificate"),
        ),
        ("certificate-not-trusted", icase(r"certificate not trusted")),
    ]
});

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellTrustStatus {
    pub code: &'static str,
    pub reason: EngineTrustFallbackReason,
    pub action: EngineTrustActionId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellTrustHint {
    pub code: &'static str,
    pub heuristic: bool,
    pub pattern: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellTrust {
    pub status: ShellTrustStatus,
    pub hint: Option<ShellTrustHint>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellTrustAnnotation {
    pub trust: ShellTrust,
    pub text: Option<String>,
}

pub fn annotate_shell_trust_with_marker(
    marker: Option<&super::marker::EngineTrustMarker>,
    exit_code: Option<i32>,
    output: &str,
) -> Option<ShellTrustAnnotation> {
    let marker = marker?;
    if marker.automatic != EngineTrustAutomatic::Fallback {
        return None;
    }
    let reason = marker.fallback_reason?;
    let code = exit_code?;
    if code == 0 {
        return None;
    }
    let action = engine_trust_action(reason);
    let status = ShellTrustStatus {
        code: "trust-import-degraded",
        reason,
        action: action.id,
    };
    let pattern = PATTERNS
        .iter()
        .find(|(_, regex)| regex.is_match(output))
        .map(|(id, _)| *id);
    let Some(pattern) = pattern else {
        return Some(ShellTrustAnnotation {
            trust: ShellTrust { status, hint: None },
            text: None,
        });
    };
    Some(ShellTrustAnnotation {
        trust: ShellTrust {
            status,
            hint: Some(ShellTrustHint {
                code: "tls-verify-text-in-output",
                heuristic: true,
                pattern,
            }),
        },
        text: Some(format!(
            "omo: certificate import is degraded ({}), so some TLS connections may fail. This command's output contains text that looks like a certificate verification error; omo has not confirmed the cause. To diagnose: {}",
            reason.as_str(),
            action.text
        )),
    })
}

pub fn annotate_shell_trust(exit_code: Option<i32>, output: &str) -> Option<ShellTrustAnnotation> {
    annotate_shell_trust_with_marker(adopted_engine_trust().as_ref(), exit_code, output)
}

#[cfg(test)]
mod tests {
    use super::super::marker::{EngineTrustMarker, EngineTrustOwner, EngineTrustRestore};
    use super::*;

    #[test]
    fn annotates_fallback_nonzero_with_heuristic_hint() {
        let marker = EngineTrustMarker {
            v: 1,
            file: "/trust/ca.pem".into(),
            sha256: "c".repeat(64),
            automatic: EngineTrustAutomatic::Fallback,
            fallback_reason: Some(EngineTrustFallbackReason::HelperTimeout),
            owner: EngineTrustOwner::Standalone,
            user: None,
            defaults: None,
            restore: EngineTrustRestore::default(),
        };
        assert!(
            annotate_shell_trust_with_marker(Some(&marker), Some(0), "CERT_UNTRUSTED").is_none()
        );
        let plain =
            annotate_shell_trust_with_marker(Some(&marker), Some(1), "unrelated").expect("status");
        assert!(plain.text.is_none());
        let hinted = annotate_shell_trust_with_marker(
            Some(&marker),
            Some(1),
            "error: SELF_SIGNED_CERT_IN_CHAIN",
        )
        .expect("hint");
        assert_eq!(
            hinted.trust.hint.as_ref().map(|h| h.pattern),
            Some("SELF_SIGNED_CERT_IN_CHAIN")
        );
        assert!(hinted.text.is_some());
    }
}
