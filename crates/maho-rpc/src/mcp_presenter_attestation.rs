//! Port of senpi `packages/coding-agent/src/modes/rpc/mcp-presenter-attestation.ts`.
//!
//! Host-side checks of a presenter attestation (D11 S3-2): RFC 7638/8037 key thumbprint,
//! single-use `jti` replay cache (refuse rather than evict), and bounded claims.

use base64::Engine as _;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// RFC 7638/8037 thumbprint of an Ed25519 SPKI DER key: raw 32 bytes after the fixed 12-byte
/// prefix as JWK `{"crv":"Ed25519","kty":"OKP","x":...}`, SHA-256, unpadded base64url.
pub fn presenter_key_thumbprint(spki_der: &[u8]) -> Option<String> {
    if spki_der.len() != 44 || spki_der[..12] != ED25519_SPKI_PREFIX {
        return None;
    }
    let x = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&spki_der[12..]);
    let jwk = serde_json::json!({ "crv": "Ed25519", "kty": "OKP", "x": x });
    let encoded = serde_json::to_string(&jwk).ok()?;
    let digest = Sha256::digest(encoded.as_bytes());
    Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JtiAdmit {
    Ok,
    AttestationReplayed,
    PresenterBusy,
}

impl JtiAdmit {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::AttestationReplayed => "attestation_replayed",
            Self::PresenterBusy => "presenter_busy",
        }
    }
}

pub struct PresenterJtiCache {
    seen: HashMap<String, i64>,
    cap: usize,
}

impl Default for PresenterJtiCache {
    fn default() -> Self {
        Self::new(4096)
    }
}

impl PresenterJtiCache {
    pub fn new(cap: usize) -> Self {
        Self {
            seen: HashMap::new(),
            cap,
        }
    }

    pub fn admit(&mut self, jti: &str, exp: i64, now_s: i64) -> JtiAdmit {
        self.seen.retain(|_, until| *until > now_s);
        if self.seen.contains_key(jti) {
            return JtiAdmit::AttestationReplayed;
        }
        if self.seen.len() >= self.cap {
            return JtiAdmit::PresenterBusy;
        }
        self.seen.insert(jti.to_owned(), exp.saturating_add(60));
        JtiAdmit::Ok
    }

    pub fn clear(&mut self) {
        self.seen.clear();
    }
}

/// Display and cache inputs from the issuer: non-empty, at most 256 characters, no control chars.
pub fn bounded_claim(value: &str) -> bool {
    !value.is_empty() && value.chars().count() <= 256 && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_thumbprint_and_enforces_jti_replay_cap() {
        let mut der = vec![0u8; 44];
        der[..12].copy_from_slice(&ED25519_SPKI_PREFIX);
        assert!(presenter_key_thumbprint(&der).is_some());
        assert!(presenter_key_thumbprint(&[0u8; 43]).is_none());

        let mut cache = PresenterJtiCache::new(1);
        assert_eq!(cache.admit("j1", 100, 50), JtiAdmit::Ok);
        assert_eq!(cache.admit("j1", 100, 50), JtiAdmit::AttestationReplayed);
        assert_eq!(cache.admit("j2", 100, 50), JtiAdmit::PresenterBusy);
        assert_eq!(cache.admit("j2", 200, 160), JtiAdmit::Ok);
        assert!(bounded_claim("dev-1"));
        assert!(!bounded_claim("dev\n1"));
    }
}
