//! Port of senpi `packages/coding-agent/src/modes/rpc/secret-sealed-hpke.ts`.
//!
//! The open half of RFC 9180 base mode, one shot (sequence 0), for the suite decision 0022 pins:
//! DHKEM(X25519, HKDF-SHA256) / HKDF-SHA256 / AES-256-GCM (0x0020 / 0x0001 / 0x0002).
//!
//! `x25519-dalek` is not in `Cargo.lock`, so X25519 Diffie-Hellman and keypair generation are
//! isolated behind the [`X25519Dh`] trait (with [`StubX25519Dh`] as the default fail-closed
//! adapter), while [`strict_x25519`] validation, RFC 9180 HKDF-SHA256 key schedule, and
//! AES-256-GCM decryption are fully implemented in-crate.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hmac::{Hmac, Mac};
use sha2::Sha256;

const KEM_SUITE: &[u8] = &[0x4b, 0x45, 0x4d, 0x00, 0x20];
const HPKE_SUITE: &[u8] = &[0x48, 0x50, 0x4b, 0x45, 0x00, 0x20, 0x00, 0x01, 0x00, 0x02];
const VERSION: &[u8] = b"HPKE-v1";

type HmacSha256 = Hmac<Sha256>;

/// 256-bit unsigned integer for strict X25519 u-coordinate validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct U256([u64; 4]);

impl U256 {
    const fn from_le_bytes(raw: &[u8; 32]) -> Self {
        let mut limbs = [0u64; 4];
        let mut i = 0;
        while i < 32 {
            limbs[i / 8] |= (raw[i] as u64) << ((i % 8) * 8);
            i += 1;
        }
        Self(limbs)
    }

    const fn lt(self, rhs: Self) -> bool {
        let mut i = 4;
        while i > 0 {
            i -= 1;
            if self.0[i] < rhs.0[i] {
                return true;
            }
            if self.0[i] > rhs.0[i] {
                return false;
            }
        }
        false
    }

    const fn parse_dec(s: &str) -> Self {
        let bytes = s.as_bytes();
        let mut acc = [0u64; 4];
        let mut idx = 0;
        while idx < bytes.len() {
            let digit = (bytes[idx] - b'0') as u128;
            let mut carry = digit;
            let mut limb = 0;
            while limb < 4 {
                let v = (acc[limb] as u128) * 10 + carry;
                acc[limb] = v as u64;
                carry = v >> 64;
                limb += 1;
            }
            idx += 1;
        }
        Self(acc)
    }
}

const P: U256 = U256([u64::MAX - 18, u64::MAX, u64::MAX, (1u64 << 63) - 1]);
const P_MINUS_1: U256 = U256([u64::MAX - 19, u64::MAX, u64::MAX, (1u64 << 63) - 1]);

/// The five low-order X25519 u-coordinates below p (RFC 7748 section 7 refusals; decision 0003).
const LOW_ORDER_U: [U256; 5] = [
    U256([0, 0, 0, 0]),
    U256([1, 0, 0, 0]),
    U256::parse_dec("325606250916557431795983626356110631294008115727848805560023387167927233504"),
    U256::parse_dec(
        "39382357235489614581723060781553021112529911719440698176882885853963445705823",
    ),
    P_MINUS_1,
];

/// A strict X25519 public value: 32 bytes, top bit clear, u < p, not one of the five low-order points.
pub fn strict_x25519(raw: &[u8]) -> bool {
    let Ok(bytes): Result<&[u8; 32], _> = raw.try_into() else {
        return false;
    };
    if (bytes[31] & 0x80) != 0 {
        return false;
    }
    let u = U256::from_le_bytes(bytes);
    u.lt(P) && !LOW_ORDER_U.contains(&u)
}

/// Pluggable X25519 Diffie-Hellman and keypair provider.
pub trait X25519Dh: Send + Sync {
    fn diffie_hellman(&self, private_key: &[u8], peer_public_key: &[u8; 32]) -> Option<[u8; 32]>;
    fn generate_keypair(&self) -> Result<(Vec<u8>, [u8; 32]), String>;
}

/// Default stub X25519 adapter when no native X25519 implementation is wired.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubX25519Dh;

impl X25519Dh for StubX25519Dh {
    fn diffie_hellman(&self, _private_key: &[u8], _peer_public_key: &[u8; 32]) -> Option<[u8; 32]> {
        None
    }

    fn generate_keypair(&self) -> Result<(Vec<u8>, [u8; 32]), String> {
        Err("x25519_unavailable".to_owned())
    }
}

fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).unwrap_or_else(|_| {
        <HmacSha256 as Mac>::new_from_slice(&[0u8; 32]).unwrap_or_else(|_| unreachable!())
    });
    for part in parts {
        mac.update(part);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&mac.finalize().into_bytes());
    out
}

fn extract(salt: &[u8], ikm: &[u8]) -> [u8; 32] {
    let zeros = [0u8; 32];
    let effective_salt = if salt.is_empty() { &zeros[..] } else { salt };
    hmac_sha256(effective_salt, &[ikm])
}

fn expand(prk: &[u8], info: &[u8], length: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(length);
    let mut prev: Vec<u8> = Vec::new();
    let mut counter: u8 = 1;
    while out.len() < length {
        let tag = hmac_sha256(prk, &[&prev, info, &[counter]]);
        prev.clear();
        prev.extend_from_slice(&tag);
        out.extend_from_slice(&tag);
        counter = counter.wrapping_add(1);
    }
    out.truncate(length);
    out
}

fn labeled_extract(suite: &[u8], salt: &[u8], label: &str, ikm: &[u8]) -> [u8; 32] {
    let mut combined = Vec::with_capacity(VERSION.len() + suite.len() + label.len() + ikm.len());
    combined.extend_from_slice(VERSION);
    combined.extend_from_slice(suite);
    combined.extend_from_slice(label.as_bytes());
    combined.extend_from_slice(ikm);
    extract(salt, &combined)
}

fn labeled_expand(suite: &[u8], prk: &[u8], label: &str, info: &[u8], length: usize) -> Vec<u8> {
    let prefix = [((length >> 8) & 0xff) as u8, (length & 0xff) as u8];
    let mut full_info =
        Vec::with_capacity(2 + VERSION.len() + suite.len() + label.len() + info.len());
    full_info.extend_from_slice(&prefix);
    full_info.extend_from_slice(VERSION);
    full_info.extend_from_slice(suite);
    full_info.extend_from_slice(label.as_bytes());
    full_info.extend_from_slice(info);
    expand(prk, &full_info, length)
}

pub struct SecretOpenInput<'a> {
    pub private_key: &'a [u8],
    pub recipient_public_key: &'a [u8],
    pub enc: &'a [u8],
    pub ct: &'a [u8],
    pub info: &'a [u8],
    pub aad: &'a [u8],
}

/// HPKE base-mode open; `None` for any refusal (weak `enc`, zero shared secret, or an AEAD failure).
pub fn open_secret_answer(
    input: &SecretOpenInput<'_>,
    dh_provider: &dyn X25519Dh,
) -> Option<Vec<u8>> {
    if !strict_x25519(input.enc)
        || !strict_x25519(input.recipient_public_key)
        || input.ct.len() < 16
    {
        return None;
    }
    let enc_arr: &[u8; 32] = input.enc.try_into().ok()?;
    let mut dh = dh_provider.diffie_hellman(input.private_key, enc_arr)?;
    if dh.iter().all(|&b| b == 0) {
        return None;
    }
    let mut kem_context = Vec::with_capacity(64);
    kem_context.extend_from_slice(input.enc);
    kem_context.extend_from_slice(input.recipient_public_key);
    let mut eae_prk = labeled_extract(KEM_SUITE, &[], "eae_prk", &dh);
    let mut shared_secret = labeled_expand(KEM_SUITE, &eae_prk, "shared_secret", &kem_context, 32);

    let psk_id_hash = labeled_extract(HPKE_SUITE, &[], "psk_id_hash", &[]);
    let info_hash = labeled_extract(HPKE_SUITE, &[], "info_hash", input.info);
    let mut context = Vec::with_capacity(1 + 32 + 32);
    context.push(0x00);
    context.extend_from_slice(&psk_id_hash);
    context.extend_from_slice(&info_hash);

    let mut secret = labeled_extract(HPKE_SUITE, &shared_secret, "secret", &[]);
    let mut key = labeled_expand(HPKE_SUITE, &secret, "key", &context, 32);
    let mut nonce_bytes = labeled_expand(HPKE_SUITE, &secret, "base_nonce", &context, 12);

    let result = Aes256Gcm::new_from_slice(&key).ok().and_then(|cipher| {
        let nonce = Nonce::from_slice(&nonce_bytes);
        cipher
            .decrypt(
                nonce,
                Payload {
                    msg: input.ct,
                    aad: input.aad,
                },
            )
            .ok()
    });

    dh.fill(0);
    eae_prk.fill(0);
    shared_secret.fill(0);
    secret.fill(0);
    key.fill(0);
    nonce_bytes.fill(0);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedDh([u8; 32]);
    impl X25519Dh for FixedDh {
        fn diffie_hellman(&self, _priv: &[u8], _pub: &[u8; 32]) -> Option<[u8; 32]> {
            Some(self.0)
        }
        fn generate_keypair(&self) -> Result<(Vec<u8>, [u8; 32]), String> {
            Ok((vec![9u8; 32], [1u8; 32]))
        }
    }

    #[test]
    fn strict_x25519_rejects_low_order_and_non_canonical_points() {
        assert!(!strict_x25519(&[0u8; 31]));
        assert!(!strict_x25519(&[0u8; 32]));
        let mut one = [0u8; 32];
        one[0] = 1;
        assert!(!strict_x25519(&one));

        // Top bit set
        let mut high = [2u8; 32];
        high[31] = 0x80;
        assert!(!strict_x25519(&high));

        // P - 1
        let mut p_minus_1 = [0xffu8; 32];
        p_minus_1[0] = 0xec;
        p_minus_1[31] = 0x7f;
        assert!(!strict_x25519(&p_minus_1));

        // Valid u-coordinate (9)
        let mut nine = [0u8; 32];
        nine[0] = 9;
        assert!(strict_x25519(&nine));
    }

    #[test]
    fn open_secret_answer_decrypts_valid_ciphertext_and_rejects_bad_aad() {
        let mut enc = [0u8; 32];
        enc[0] = 9;
        let mut pk = [0u8; 32];
        pk[0] = 11;
        let dh = [0x42u8; 32];
        let info = b"omo-secret-answer-v1\0info";
        let aad = b"aad-v1";

        // Derive the same HPKE key & nonce to seal test plaintext.
        let mut kem_context = Vec::new();
        kem_context.extend_from_slice(&enc);
        kem_context.extend_from_slice(&pk);
        let eae_prk = labeled_extract(KEM_SUITE, &[], "eae_prk", &dh);
        let shared = labeled_expand(KEM_SUITE, &eae_prk, "shared_secret", &kem_context, 32);
        let psk_id_hash = labeled_extract(HPKE_SUITE, &[], "psk_id_hash", &[]);
        let info_hash = labeled_extract(HPKE_SUITE, &[], "info_hash", info);
        let mut context = vec![0x00];
        context.extend_from_slice(&psk_id_hash);
        context.extend_from_slice(&info_hash);
        let secret = labeled_extract(HPKE_SUITE, &shared, "secret", &[]);
        let key = labeled_expand(HPKE_SUITE, &secret, "key", &context, 32);
        let nonce_bytes = labeled_expand(HPKE_SUITE, &secret, "base_nonce", &context, 12);

        let cipher = Aes256Gcm::new_from_slice(&key).expect("cipher");
        let ct = cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload {
                    msg: b"secret-plaintext",
                    aad,
                },
            )
            .expect("encrypt");

        let provider = FixedDh(dh);
        let opened = open_secret_answer(
            &SecretOpenInput {
                private_key: &[1u8; 32],
                recipient_public_key: &pk,
                enc: &enc,
                ct: &ct,
                info,
                aad,
            },
            &provider,
        );
        assert_eq!(opened.as_deref(), Some(b"secret-plaintext".as_slice()));

        // Wrong AAD must fail.
        let bad = open_secret_answer(
            &SecretOpenInput {
                private_key: &[1u8; 32],
                recipient_public_key: &pk,
                enc: &enc,
                ct: &ct,
                info,
                aad: b"wrong-aad",
            },
            &provider,
        );
        assert_eq!(bad, None);
    }
}
