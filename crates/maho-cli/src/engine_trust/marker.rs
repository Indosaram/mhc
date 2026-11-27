//! Marker version 1 of the engine trust launch contract (`packages/coding-agent/src/engine-trust/marker.ts`).

use std::fmt;

use serde::{Deserialize, Serialize};

pub const ENGINE_TRUST_MARKER_ENV: &str = "SENPI_ENGINE_TRUST";
/// Set only by the launcher's own trust exec; a launch that carries it never execs again.
pub const ENGINE_TRUST_HOP_ENV: &str = "SENPI_ENGINE_TRUST_HOP";

/// The names a launch records on arrival and an adopting engine restores for its children.
pub const ENGINE_TRUST_RESTORE_NAMES: [&str; 5] = [
    "NODE_EXTRA_CA_CERTS",
    "NODE_USE_SYSTEM_CA",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "BUN_OPTIONS",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EngineTrustFallbackReason {
    HelperTimeout,
    HelperCrash,
    HelperMalformedOutput,
    HelperMissing,
    HelperWrongArch,
    HelperSignature,
    AccessDenied,
    NegativesIncomplete,
    LinuxBundleUnreadable,
    LinuxBundleMalformed,
}

impl EngineTrustFallbackReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HelperTimeout => "helper-timeout",
            Self::HelperCrash => "helper-crash",
            Self::HelperMalformedOutput => "helper-malformed-output",
            Self::HelperMissing => "helper-missing",
            Self::HelperWrongArch => "helper-wrong-arch",
            Self::HelperSignature => "helper-signature",
            Self::AccessDenied => "access-denied",
            Self::NegativesIncomplete => "negatives-incomplete",
            Self::LinuxBundleUnreadable => "linux-bundle-unreadable",
            Self::LinuxBundleMalformed => "linux-bundle-malformed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EngineTrustAutomatic {
    Os,
    Fallback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EngineTrustOwner {
    Server,
    Standalone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineTrustContractErrorCode {
    TrustMarkerInvalid,
    TrustPathUnrepresentable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineTrustContractError {
    pub code: EngineTrustContractErrorCode,
    pub message: String,
}

impl fmt::Display for EngineTrustContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for EngineTrustContractError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineTrustDefaults {
    pub file: String,
    pub dir: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineTrustRestore {
    #[serde(
        rename = "NODE_EXTRA_CA_CERTS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub node_extra_ca_certs: Option<Option<String>>,
    #[serde(
        rename = "NODE_USE_SYSTEM_CA",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub node_use_system_ca: Option<Option<String>>,
    #[serde(
        rename = "SSL_CERT_FILE",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub ssl_cert_file: Option<Option<String>>,
    #[serde(
        rename = "SSL_CERT_DIR",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub ssl_cert_dir: Option<Option<String>>,
    #[serde(
        rename = "BUN_OPTIONS",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub bun_options: Option<Option<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineTrustMarker {
    pub v: u8,
    pub file: String,
    pub sha256: String,
    pub automatic: EngineTrustAutomatic,
    pub fallback_reason: Option<EngineTrustFallbackReason>,
    pub owner: EngineTrustOwner,
    pub user: Option<String>,
    pub defaults: Option<EngineTrustDefaults>,
    pub restore: EngineTrustRestore,
}

fn valid_path(value: &str) -> bool {
    !value.is_empty() && !value.contains('\0')
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn validate_marker(marker: &EngineTrustMarker) -> bool {
    if marker.v != 1 || !valid_path(&marker.file) || !valid_digest(&marker.sha256) {
        return false;
    }
    if let Some(user) = &marker.user
        && !valid_digest(user)
    {
        return false;
    }
    if let Some(defaults) = &marker.defaults
        && (!valid_path(&defaults.file) || !valid_path(&defaults.dir))
    {
        return false;
    }
    if (marker.automatic == EngineTrustAutomatic::Fallback) != marker.fallback_reason.is_some() {
        return false;
    }
    if marker.owner == EngineTrustOwner::Server && marker.defaults.is_none() {
        return false;
    }
    if marker.owner == EngineTrustOwner::Standalone && marker.restore.bun_options.is_some() {
        return false;
    }
    true
}

pub fn parse_engine_trust_marker(text: &str) -> Option<EngineTrustMarker> {
    let marker: EngineTrustMarker = serde_json::from_str(text).ok()?;
    validate_marker(&marker).then_some(marker)
}

/// One line of JSON; errors rather than write a marker no reader would accept.
pub fn serialize_engine_trust_marker(
    marker: &EngineTrustMarker,
) -> Result<String, EngineTrustContractError> {
    if !validate_marker(marker) {
        return Err(EngineTrustContractError {
            code: EngineTrustContractErrorCode::TrustMarkerInvalid,
            message: "engine trust marker is not version 1".to_owned(),
        });
    }
    serde_json::to_string(marker).map_err(|error| EngineTrustContractError {
        code: EngineTrustContractErrorCode::TrustMarkerInvalid,
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_marker() -> EngineTrustMarker {
        EngineTrustMarker {
            v: 1,
            file: "/tmp/trust/ca.pem".into(),
            sha256: "a".repeat(64),
            automatic: EngineTrustAutomatic::Os,
            fallback_reason: None,
            owner: EngineTrustOwner::Server,
            user: None,
            defaults: Some(EngineTrustDefaults {
                file: "/tmp/trust/defaults.pem".into(),
                dir: "/tmp/trust/certs".into(),
            }),
            restore: EngineTrustRestore::default(),
        }
    }

    #[test]
    fn round_trips_valid_marker_and_rejects_invariants() {
        let marker = sample_marker();
        let json = serialize_engine_trust_marker(&marker).expect("serialize");
        assert_eq!(parse_engine_trust_marker(&json), Some(marker.clone()));

        let mut bad_fallback = marker.clone();
        bad_fallback.automatic = EngineTrustAutomatic::Fallback;
        assert!(serialize_engine_trust_marker(&bad_fallback).is_err());

        let mut bad_standalone = marker;
        bad_standalone.owner = EngineTrustOwner::Standalone;
        bad_standalone.restore.bun_options = Some(None);
        assert!(serialize_engine_trust_marker(&bad_standalone).is_err());
    }
}
