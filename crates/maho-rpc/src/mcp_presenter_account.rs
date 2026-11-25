//! Port of senpi `packages/coding-agent/src/modes/rpc/mcp-presenter-account.ts`.
//!
//! Same-account presenter enrollment authority types and attestation/step-up traits (senpi#4249, D11).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenterAttestationRefusal {
    AttestationMalformed,
    AttestationSignature,
    AttestationAudience,
    AttestationExpired,
    AttestationKeyMismatch,
}

impl PresenterAttestationRefusal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AttestationMalformed => "attestation_malformed",
            Self::AttestationSignature => "attestation_signature",
            Self::AttestationAudience => "attestation_audience",
            Self::AttestationExpired => "attestation_expired",
            Self::AttestationKeyMismatch => "attestation_key_mismatch",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenterAttestation {
    pub acc: String,
    pub dev: String,
    pub dname: String,
    pub exp: i64,
    pub jti: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretHostProof {
    pub issuer: String,
    pub nonce: String,
}

pub type PresenterSignFn = Box<dyn Fn(&SecretHostProof) -> Result<String, String> + Send + Sync>;

pub struct SecretHostTokenRequest {
    pub host_instance: String,
    pub durable_session_id: String,
    pub host_key_spki: Vec<u8>,
    pub sign: PresenterSignFn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretAnswerRefusal {
    StepupMalformed,
    StepupSignature,
    StepupAudience,
    StepupExpired,
    StepupKeyMismatch,
    StepupBinding,
}

impl SecretAnswerRefusal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StepupMalformed => "stepup_malformed",
            Self::StepupSignature => "stepup_signature",
            Self::StepupAudience => "stepup_audience",
            Self::StepupExpired => "stepup_expired",
            Self::StepupKeyMismatch => "stepup_key_mismatch",
            Self::StepupBinding => "stepup_binding",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretAnswerStepUp {
    pub acc: String,
    pub dev: String,
    pub jti: String,
    pub exp: i64,
    pub hdev: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountPrincipal {
    pub id: String,
    pub acc: String,
    pub dev: String,
    pub dname: String,
    pub attested_until: i64,
    pub public_key_spki: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenterAccountRefusal {
    PresenterAccountUnavailable,
    PresenterAccountMismatch,
    AttestationExpired,
}

impl PresenterAccountRefusal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PresenterAccountUnavailable => "presenter_account_unavailable",
            Self::PresenterAccountMismatch => "presenter_account_mismatch",
            Self::AttestationExpired => "attestation_expired",
        }
    }
}

#[async_trait]
pub trait PresenterAccountAuthority: Send + Sync {
    fn host_account(&self) -> Option<String>;
    async fn verify(
        &self,
        token: &str,
        public_key_spki_der: &[u8],
        now_s: i64,
    ) -> Result<PresenterAttestation, PresenterAttestationRefusal>;
    fn on_account_change(&self, listener: Arc<dyn Fn() + Send + Sync>) -> Box<dyn FnOnce() + Send>;
    async fn mint_secret_host_token(
        &self,
        _request: SecretHostTokenRequest,
    ) -> Result<String, String> {
        Err("secret_host_token_unsupported".to_owned())
    }
    async fn verify_secret_answer(
        &self,
        _token: &str,
        _host_token: &str,
        _public_key_spki_der: &[u8],
        _now_s: i64,
    ) -> Result<SecretAnswerStepUp, SecretAnswerRefusal> {
        Err(SecretAnswerRefusal::StepupBinding)
    }
}
