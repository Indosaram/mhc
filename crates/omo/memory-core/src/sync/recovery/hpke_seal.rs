use super::{RecoveryError, derive::random};
use crate::sync::crypto::vendor::sync_format::keys::seal;
use aws_lc_rs::{agreement, hmac};

const KEM: &[u8] = b"KEM\x00\x20";
const SUITE: &[u8] = b"HPKE\x00\x20\x00\x01\x00\x02";
fn extract(suite: &[u8], label: &[u8], ikm: &[u8], salt: &[u8]) -> Vec<u8> {
    hmac::sign(
        &hmac::Key::new(hmac::HMAC_SHA256, salt),
        &[b"HPKE-v1".as_slice(), suite, label, ikm].concat(),
    )
    .as_ref()
    .to_vec()
}
fn expand(suite: &[u8], label: &[u8], prk: &[u8], info: &[u8], length: u16) -> Vec<u8> {
    let tag = hmac::sign(
        &hmac::Key::new(hmac::HMAC_SHA256, prk),
        &[
            &length.to_be_bytes(),
            b"HPKE-v1".as_slice(),
            suite,
            label,
            info,
            &[1],
        ]
        .concat(),
    );
    tag.as_ref()[..usize::from(length)].to_vec()
}
pub fn seal_hpke_box(
    public: &[u8],
    plaintext: &[u8],
    info: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, RecoveryError> {
    if public.len() != 32 {
        return Err(RecoveryError::InvalidInput(
            "an X25519 public key is exactly 32 bytes",
        ));
    }
    let scalar = zeroize::Zeroizing::new(random::<32>()?);
    let ephemeral = agreement::PrivateKey::from_private_key(&agreement::X25519, scalar.as_ref())
        .map_err(|_| RecoveryError::Crypto)?;
    let enc = ephemeral
        .compute_public_key()
        .map_err(|_| RecoveryError::Crypto)?;
    let peer = agreement::UnparsedPublicKey::new(&agreement::X25519, public);
    let shared = agreement::agree(&ephemeral, peer, RecoveryError::Crypto, |dh| {
        if dh.iter().all(|byte| *byte == 0) {
            return Err(RecoveryError::Crypto);
        }
        Ok(zeroize::Zeroizing::new(expand(
            KEM,
            b"shared_secret",
            &extract(KEM, b"eae_prk", dh, &[]),
            &[enc.as_ref(), public].concat(),
            32,
        )))
    })?;
    let context = [
        &[0],
        extract(SUITE, b"psk_id_hash", &[], &[]).as_slice(),
        extract(SUITE, b"info_hash", info, &[]).as_slice(),
    ]
    .concat();
    let secret = zeroize::Zeroizing::new(extract(SUITE, b"secret", &[], &shared));
    let key = zeroize::Zeroizing::new(expand(SUITE, b"key", &secret, &context, 32));
    let nonce: [u8; 12] = expand(SUITE, b"base_nonce", &secret, &context, 12)
        .try_into()
        .map_err(|_| RecoveryError::Crypto)?;
    let ciphertext = seal(&key, nonce, aad, plaintext).ok_or(RecoveryError::Crypto)?;
    Ok([enc.as_ref(), &ciphertext].concat())
}
