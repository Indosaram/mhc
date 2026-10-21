use super::RecoveryError;
use crate::sync::crypto::{signatures::sha256, vendor::sync_format::keys::hkdf_sha256};
use aws_lc_rs::{
    agreement,
    rand::{SecureRandom, SystemRandom},
    signature::{Ed25519KeyPair, KeyPair},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroizing;

fn framed(context: &[u8], payload: &[u8]) -> Vec<u8> {
    [context, &[0], payload].concat()
}
pub fn recovery_user_handle(account: &str) -> Vec<u8> {
    sha256(&framed(
        b"omo-sync/v1/recovery-user",
        account.nfc().collect::<String>().as_bytes(),
    ))
}
pub fn recovery_prf_salt(account: &str, nonce: &[u8]) -> Result<Vec<u8>, RecoveryError> {
    if nonce.len() != 32 {
        return Err(RecoveryError::InvalidInput(
            "an enrollment nonce is exactly 32 bytes",
        ));
    }
    Ok(sha256(&framed(
        b"omo-sync/v1/recovery-prf",
        &[account.nfc().collect::<String>().as_bytes(), nonce].concat(),
    )))
}
pub fn hmac_secret_salt_for_prf(salt: &[u8]) -> Vec<u8> {
    sha256(&framed(b"WebAuthn PRF", salt))
}

/// Private halves cannot be serialized or debugged and are overwritten on every exit.
pub struct RecoveryKeyPairs {
    pub(super) seal_private: Zeroizing<[u8; 32]>,
    pub seal_public: Vec<u8>,
    pub(super) sign_seed: Zeroizing<[u8; 32]>,
    pub sign_public: Vec<u8>,
}
pub fn x25519_public_key(private: &[u8]) -> Result<Vec<u8>, RecoveryError> {
    agreement::PrivateKey::from_private_key(&agreement::X25519, private)
        .map_err(|_| RecoveryError::Crypto)?
        .compute_public_key()
        .map(|key| key.as_ref().to_vec())
        .map_err(|_| RecoveryError::Crypto)
}
pub fn derive_recovery_keys(prf: &[u8], nonce: &[u8]) -> Result<RecoveryKeyPairs, RecoveryError> {
    if prf.len() != 32 || nonce.len() != 32 {
        return Err(RecoveryError::InvalidInput(
            "PRF output and enrollment nonce are exactly 32 bytes",
        ));
    }
    let seal_private = Zeroizing::new(hkdf_sha256(prf, nonce, b"omo-sync/v1/recovery-seal"));
    let sign_seed = Zeroizing::new(hkdf_sha256(prf, nonce, b"omo-sync/v1/recovery-sign"));
    let seal_public = x25519_public_key(seal_private.as_ref())?;
    let sign_public = Ed25519KeyPair::from_seed_unchecked(sign_seed.as_ref())
        .map_err(|_| RecoveryError::Crypto)?
        .public_key()
        .as_ref()
        .to_vec();
    Ok(RecoveryKeyPairs {
        seal_private,
        seal_public,
        sign_seed,
        sign_public,
    })
}
pub fn public_key_text(keys: &RecoveryKeyPairs) -> (String, String) {
    (
        URL_SAFE_NO_PAD.encode(&keys.seal_public),
        URL_SAFE_NO_PAD.encode(&keys.sign_public),
    )
}
pub(super) fn random<const N: usize>() -> Result<[u8; N], RecoveryError> {
    let mut bytes = [0; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| RecoveryError::Crypto)?;
    Ok(bytes)
}
pub(super) fn sign_framed(
    context: &str,
    payload: &serde_json::Value,
    seed: &[u8],
) -> Result<String, RecoveryError> {
    let bytes = crate::sync::crypto::signatures::canonical(payload)?;
    let key = Ed25519KeyPair::from_seed_unchecked(seed).map_err(|_| RecoveryError::Crypto)?;
    let signature = key
        .try_sign(&framed(context.as_bytes(), &bytes))
        .map_err(|_| RecoveryError::Crypto)?;
    Ok(URL_SAFE_NO_PAD.encode(signature.as_ref()))
}
