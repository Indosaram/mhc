//! Port of senpi packages/ai/src/auth/oauth/pkce.ts.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use sha2::{Digest, Sha256};

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

fn base64url_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn generate_pkce() -> Pkce {
    let mut verifier_bytes = [0u8; 32];
    verifier_bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    verifier_bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let verifier = base64url_encode(&verifier_bytes);
    let challenge = base64url_encode(&Sha256::digest(verifier.as_bytes()));
    Pkce { verifier, challenge }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_is_sha256_of_verifier_in_base64url() {
        let pkce = generate_pkce();
        assert_eq!(pkce.verifier.len(), 43);
        assert_eq!(pkce.challenge.len(), 43);
        assert_eq!(pkce.challenge, base64url_encode(&Sha256::digest(pkce.verifier.as_bytes())));
        assert!(!pkce.verifier.contains('+') && !pkce.verifier.contains('/') && !pkce.verifier.contains('='));
    }

    #[test]
    fn verifiers_are_unique_per_call() {
        assert_ne!(generate_pkce().verifier, generate_pkce().verifier);
    }
}
