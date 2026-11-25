//! Port of `packages/coding-agent/src/engine-trust/actions.ts` (omo#10762).

use super::marker::EngineTrustFallbackReason;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineTrustActionReason {
    Fallback(EngineTrustFallbackReason),
    TrustPathUnrepresentable,
    TrustUserExtrasUnrecoverable,
}

impl From<EngineTrustFallbackReason> for EngineTrustActionReason {
    fn from(value: EngineTrustFallbackReason) -> Self {
        Self::Fallback(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineTrustActionId {
    ReinstallOmo,
    RetryAfterStoreReady,
    CheckStoreAccess,
    ReportHelperFailure,
    RegenerateSystemBundle,
    SetTmpdir,
    FixExtraCaVariable,
}

impl EngineTrustActionId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReinstallOmo => "reinstall-omo",
            Self::RetryAfterStoreReady => "retry-after-store-ready",
            Self::CheckStoreAccess => "check-store-access",
            Self::ReportHelperFailure => "report-helper-failure",
            Self::RegenerateSystemBundle => "regenerate-system-bundle",
            Self::SetTmpdir => "set-tmpdir",
            Self::FixExtraCaVariable => "fix-extra-ca-variable",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineTrustAction {
    pub id: EngineTrustActionId,
    pub text: &'static str,
}

fn action_text(id: EngineTrustActionId) -> &'static str {
    match id {
        EngineTrustActionId::ReinstallOmo => {
            "Reinstall or update omo (omo update; desktop: install the current app). The certificate helper is missing or damaged."
        }
        EngineTrustActionId::RetryAfterStoreReady => {
            "Run omo trust ensure --wait, then restart the session. The OS certificate store answered too slowly."
        }
        EngineTrustActionId::CheckStoreAccess => {
            "Run omo doctor, which names the store or registry value that could not be read."
        }
        EngineTrustActionId::ReportHelperFailure => {
            "Run omo doctor and attach its output to a bug report; reinstalling omo (omo update) may repair the helper."
        }
        EngineTrustActionId::RegenerateSystemBundle => {
            "Run your distribution's update-ca-certificates or update-ca-trust. The generated TLS bundle could not be read."
        }
        EngineTrustActionId::SetTmpdir => {
            "Both the trust directory and the temporary directory contain a character this platform cannot pass in SSL_CERT_DIR. Set TMPDIR (Windows: TMP) to a path without : (Windows: ;) and start omo again."
        }
        EngineTrustActionId::FixExtraCaVariable => {
            "Point NODE_EXTRA_CA_CERTS at your own CA file, or unset it."
        }
    }
}

pub fn engine_trust_action(reason: impl Into<EngineTrustActionReason>) -> EngineTrustAction {
    let id = match reason.into() {
        EngineTrustActionReason::Fallback(
            EngineTrustFallbackReason::HelperMissing
            | EngineTrustFallbackReason::HelperWrongArch
            | EngineTrustFallbackReason::HelperSignature,
        ) => EngineTrustActionId::ReinstallOmo,
        EngineTrustActionReason::Fallback(EngineTrustFallbackReason::HelperTimeout) => {
            EngineTrustActionId::RetryAfterStoreReady
        }
        EngineTrustActionReason::Fallback(
            EngineTrustFallbackReason::AccessDenied
            | EngineTrustFallbackReason::NegativesIncomplete,
        ) => EngineTrustActionId::CheckStoreAccess,
        EngineTrustActionReason::Fallback(
            EngineTrustFallbackReason::HelperCrash
            | EngineTrustFallbackReason::HelperMalformedOutput,
        ) => EngineTrustActionId::ReportHelperFailure,
        EngineTrustActionReason::Fallback(
            EngineTrustFallbackReason::LinuxBundleUnreadable
            | EngineTrustFallbackReason::LinuxBundleMalformed,
        ) => EngineTrustActionId::RegenerateSystemBundle,
        EngineTrustActionReason::TrustPathUnrepresentable => EngineTrustActionId::SetTmpdir,
        EngineTrustActionReason::TrustUserExtrasUnrecoverable => {
            EngineTrustActionId::FixExtraCaVariable
        }
    };
    EngineTrustAction {
        id,
        text: action_text(id),
    }
}
