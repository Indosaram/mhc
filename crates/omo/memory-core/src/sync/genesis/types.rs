use crate::sync::crypto::{
    entry::WrapEntry,
    parse::{Principal, RecoveryKey},
};
use crate::sync::recovery::key_material::WrapAddress;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenesisBootstrap {
    pub account: String,
    pub custody_nonce: String,
    pub first_device: Principal,
    pub recovery_key: RecoveryKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenesisBody {
    pub kind: String,
    pub account: String,
    pub epoch: u64,
    pub bootstrap: GenesisBootstrap,
    pub custody_root: String,
    pub countersign: String,
    pub wraps_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenesisPrepared {
    pub body: GenesisBody,
    pub wraps: [WrapEntry; 2],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GenesisAppend {
    pub record: GenesisBody,
    pub base_seq: u64,
    pub link: String,
    pub keyring_link: String,
    pub record_sig: String,
    pub wraps: [WrapEntry; 2],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GenesisCountersignPayload {
    pub t: &'static str,
    pub account: String,
    pub custody_root: String,
    pub predecessor: &'static str,
    pub wraps_digest: String,
}

/// Native-owned capabilities for one genesis operation. Both seals use the same fresh K1,
/// epoch 1 and empty history, with independent history nonces and HPKE encapsulations.
/// The owner disposes K1, PRF output and recovery private keys, even on failure.
/// Only public addresses, keys, ciphertext and signatures cross this boundary.
pub trait GenesisCapabilities {
    fn seal(
        &self,
        address: &WrapAddress<'_>,
        sealing_public_key: &str,
    ) -> Result<String, GenesisBuildError>;
    /// Ed25519 over ASCII(payload.t) || NUL || JCS(payload), never unframed JSON.
    fn countersign(&self, payload: &GenesisCountersignPayload)
    -> Result<String, GenesisBuildError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenesisBuildError {
    BadBootstrap,
    BadRecord,
    PrincipalMismatch,
    AccountMismatch,
    BadWrapSet,
    BadCustodyRoot,
    WrapsDigestMismatch,
    BadCountersign,
    BadSignature,
    /// A native capability or the device signer failed; the message carries no secret material.
    Capability(String),
}

impl GenesisBuildError {
    pub fn reason(&self) -> &str {
        match self {
            Self::BadBootstrap => "bad_bootstrap",
            Self::BadRecord => "bad_record",
            Self::PrincipalMismatch => "principal_mismatch",
            Self::AccountMismatch => "account_mismatch",
            Self::BadWrapSet => "bad_wrap_set",
            Self::BadCustodyRoot => "bad_custody_root",
            Self::WrapsDigestMismatch => "wraps_digest_mismatch",
            Self::BadCountersign => "bad_countersign",
            Self::BadSignature => "bad_signature",
            Self::Capability(message) => message,
        }
    }
}
impl std::fmt::Display for GenesisBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "genesis: {}", self.reason())
    }
}
impl std::error::Error for GenesisBuildError {}
