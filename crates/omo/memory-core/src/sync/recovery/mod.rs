//! Passkey recovery, bridge-side key custody, and persisted fail-closed write holds.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used)]

pub mod availability;
pub mod bridge;
pub mod bridge_enroll;
pub mod bridge_provenance;
pub mod bridge_recover;
pub mod derive;
pub mod enroll;
pub mod hpke_seal;
pub mod join_notice;
pub mod key_material;
pub mod keyring_append;
pub mod ports;
pub mod recover;
pub mod recover_join;
pub mod recovery_handles;
pub mod remove;
pub mod salt_inputs;
pub mod served_chain;
pub mod write_hold;

#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
pub(crate) mod testing;

pub use availability::{APPROVAL_PATH_MESSAGE, RecoveryAvailability, recovery_availability};
pub use bridge::{ReferenceBridge, create_recovery_bridge};
pub use derive::{
    derive_recovery_keys, hmac_secret_salt_for_prf, recovery_prf_salt, recovery_user_handle,
};
pub use enroll::{EnrollInput, EnrollOutcome, enroll_recovery_key};
pub use join_notice::{JoinNotice, VETO_WINDOW_MS, classify_recovery_join};
pub use recover::{RecoverInput, RecoverOutcome, recover_on_fresh_device};
pub use remove::{RemoveInput, RemoveOutcome, remove_recovery_key};

#[derive(Debug)]
pub enum RecoveryError {
    Crypto,
    InvalidInput(&'static str),
    Io(std::io::Error),
    Canonical(super::canonical_json::CanonicalJsonError),
    Signing(super::crypto::build::BatchInputError),
    Pin(super::crypto::trust_pin::TrustPinError),
    Port(Box<dyn std::error::Error + Send + Sync>),
}
impl std::fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Crypto => f.write_str("recovery cryptographic operation failed"),
            Self::InvalidInput(message) => f.write_str(message),
            Self::Io(error) => error.fmt(f),
            Self::Canonical(error) => error.fmt(f),
            Self::Signing(error) => error.fmt(f),
            Self::Pin(error) => error.fmt(f),
            Self::Port(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for RecoveryError {}
impl From<std::io::Error> for RecoveryError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<super::canonical_json::CanonicalJsonError> for RecoveryError {
    fn from(error: super::canonical_json::CanonicalJsonError) -> Self {
        Self::Canonical(error)
    }
}
impl From<super::crypto::build::BatchInputError> for RecoveryError {
    fn from(error: super::crypto::build::BatchInputError) -> Self {
        Self::Signing(error)
    }
}
impl From<super::crypto::trust_pin::TrustPinError> for RecoveryError {
    fn from(error: super::crypto::trust_pin::TrustPinError) -> Self {
        Self::Pin(error)
    }
}
