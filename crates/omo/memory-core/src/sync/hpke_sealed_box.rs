//! RFC 9180 base-mode X25519 / HKDF-SHA256 / AES-256-GCM sealed boxes.

use aws_lc_rs::{aead, agreement, hmac};

const KEM: &[u8] = b"KEM\x00\x20";
const SUITE: &[u8] = b"HPKE\x00\x20\x00\x01\x00\x02";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HpkeSealedBoxError {
    TooShort,
    InvalidKey,
    OpenFailed,
}

impl std::fmt::Display for HpkeSealedBoxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooShort => "sealed box is too short",
            Self::InvalidKey => "invalid device key material",
            Self::OpenFailed => "sealed box does not open: wrong recipient or corrupted wrap",
        })
    }
}

impl std::error::Error for HpkeSealedBoxError {}

fn labeled_extract(suite: &[u8], label: &[u8], ikm: &[u8], salt: &[u8]) -> hmac::Tag {
    let key = hmac::Key::new(hmac::HMAC_SHA256, salt);
    let mut context = hmac::Context::with_key(&key);
    for part in [b"HPKE-v1".as_slice(), suite, label, ikm] {
        context.update(part);
    }
    context.sign()
}

fn labeled_expand<const N: usize>(suite: &[u8], label: &[u8], prk: &[u8], info: &[u8]) -> [u8; N] {
    const {
        assert!(N <= 32);
    }
    let mut context = hmac::Context::with_key(&hmac::Key::new(hmac::HMAC_SHA256, prk));
    let length = N.to_be_bytes();
    for part in [
        &length[length.len() - 2..],
        b"HPKE-v1".as_slice(),
        suite,
        label,
        info,
        &[1],
    ] {
        context.update(part);
    }
    let mut output = [0; N];
    output.copy_from_slice(&context.sign().as_ref()[..N]);
    output
}

/// Opens `enc || ciphertext || tag`. Empty `info` and `aad` are passed as `&[]`.
pub fn open_hpke_sealed_box(
    sealing_private_raw: &[u8],
    sealing_public_raw: &[u8],
    wrap: &[u8],
    info: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, HpkeSealedBoxError> {
    if wrap.len() < 48 {
        return Err(HpkeSealedBoxError::TooShort);
    }
    let key = agreement::PrivateKey::from_private_key(&agreement::X25519, sealing_private_raw)
        .map_err(|_| HpkeSealedBoxError::InvalidKey)?;
    open_with_key(&key, sealing_public_raw, wrap, info, aad)
}

pub(super) fn open_with_key(
    private: &agreement::PrivateKey,
    public: &[u8],
    wrap: &[u8],
    info: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, HpkeSealedBoxError> {
    if wrap.len() < 48 {
        return Err(HpkeSealedBoxError::TooShort);
    }
    if public.len() != 32 {
        return Err(HpkeSealedBoxError::InvalidKey);
    }
    let (enc, ciphertext) = wrap.split_at(32);
    let peer = agreement::UnparsedPublicKey::new(&agreement::X25519, enc);
    let shared = agreement::agree(private, peer, HpkeSealedBoxError::OpenFailed, |dh| {
        if dh.iter().all(|byte| *byte == 0) {
            return Err(HpkeSealedBoxError::OpenFailed);
        }
        let kem_context = [enc, public].concat();
        Ok(labeled_expand::<32>(
            KEM,
            b"shared_secret",
            labeled_extract(KEM, b"eae_prk", dh, &[]).as_ref(),
            &kem_context,
        ))
    })?;
    let context = [
        &[0],
        labeled_extract(SUITE, b"psk_id_hash", &[], &[]).as_ref(),
        labeled_extract(SUITE, b"info_hash", info, &[]).as_ref(),
    ]
    .concat();
    let secret = labeled_extract(SUITE, b"secret", &[], &shared);
    let key = labeled_expand::<32>(SUITE, b"key", secret.as_ref(), &context);
    let nonce = labeled_expand::<12>(SUITE, b"base_nonce", secret.as_ref(), &context);
    let key = aead::UnboundKey::new(&aead::AES_256_GCM, &key)
        .map_err(|_| HpkeSealedBoxError::OpenFailed)?;
    let key = aead::LessSafeKey::new(key);
    let mut plaintext = ciphertext.to_vec();
    let opened = key
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(aad),
            &mut plaintext,
        )
        .map_err(|_| HpkeSealedBoxError::OpenFailed)?;
    let length = opened.len();
    plaintext.truncate(length);
    Ok(plaintext)
}

#[cfg(test)]
pub(super) fn seal_fixture(public: &[u8], info: &[u8], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let ephemeral = agreement::PrivateKey::from_private_key(&agreement::X25519, &[11; 32])
        .expect("fixture scalar");
    let enc = ephemeral.compute_public_key().expect("fixture public key");
    let peer = agreement::UnparsedPublicKey::new(&agreement::X25519, public);
    let shared = agreement::agree(&ephemeral, peer, (), |dh| {
        Ok(labeled_expand::<32>(
            KEM,
            b"shared_secret",
            labeled_extract(KEM, b"eae_prk", dh, &[]).as_ref(),
            &[enc.as_ref(), public].concat(),
        ))
    })
    .expect("fixture agreement");
    let context = [
        &[0],
        labeled_extract(SUITE, b"psk_id_hash", &[], &[]).as_ref(),
        labeled_extract(SUITE, b"info_hash", info, &[]).as_ref(),
    ]
    .concat();
    let secret = labeled_extract(SUITE, b"secret", &[], &shared);
    let key = labeled_expand::<32>(SUITE, b"key", secret.as_ref(), &context);
    let nonce = labeled_expand::<12>(SUITE, b"base_nonce", secret.as_ref(), &context);
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_256_GCM, &key).expect("fixture AEAD key"),
    );
    let mut ciphertext = plaintext.to_vec();
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::from(aad),
        &mut ciphertext,
    )
    .expect("fixture encryption");
    [enc.as_ref(), &ciphertext].concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(value: &str) -> Vec<u8> {
        value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let digits = std::str::from_utf8(pair).expect("hex is ASCII");
                u8::from_str_radix(digits, 16).expect("valid fixture hex")
            })
            .collect()
    }

    fn vector() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let private = hex("497b4502664cfea5d5af0b39934dac72242a74f8480451e1aee7d6a53320333d");
        let public = hex("430f4b9859665145a6b1ba274024487bd66f03a2dd577d7753c68d7d7d00c00c");
        let wrap = hex(concat!(
            "6c93e09869df3402d7bf231bf540fadd35cd56be14f97178f0954db94b7fc256",
            "e5d84cd531cfb583096e7cfa9641bd3079cf3a91cda813c52deb5f512be9931980a41de125a925cdad859d5b7a"
        ));
        (private, public, wrap)
    }

    #[test]
    fn opens_rfc9180_base_mode_known_answer() {
        // Given the published suite 0x0020/0x0001/0x0002 vector.
        let (private, public, wrap) = vector();
        // When opening its first ciphertext.
        let plaintext = open_hpke_sealed_box(
            &private,
            &public,
            &wrap,
            b"Ode on a Grecian Urn",
            b"Count-0",
        );
        // Then the independent known plaintext is recovered.
        assert_eq!(
            plaintext.expect("RFC vector opens"),
            b"Beauty is truth, truth beauty"
        );
    }

    #[test]
    fn refuses_corruption_wrong_context_recipient_and_low_order_enc() {
        // Given an independently specified valid ciphertext and mutations of its inputs.
        let (private, public, wrap) = vector();
        let mut cases = vec![
            (
                private.clone(),
                public.clone(),
                wrap.clone(),
                b"wrong info".to_vec(),
                b"Count-0".to_vec(),
            ),
            (
                private.clone(),
                public.clone(),
                wrap.clone(),
                b"Ode on a Grecian Urn".to_vec(),
                b"wrong aad".to_vec(),
            ),
            (
                vec![7; 32],
                public.clone(),
                wrap.clone(),
                b"Ode on a Grecian Urn".to_vec(),
                b"Count-0".to_vec(),
            ),
        ];
        for index in [0, 32, wrap.len() - 1] {
            let mut corrupted = wrap.clone();
            corrupted[index] ^= 1;
            cases.push((
                private.clone(),
                public.clone(),
                corrupted,
                b"Ode on a Grecian Urn".to_vec(),
                b"Count-0".to_vec(),
            ));
        }
        for first in [0, 1] {
            let mut low_order = wrap.clone();
            low_order[..32].fill(0);
            low_order[0] = first;
            cases.push((
                private.clone(),
                public.clone(),
                low_order,
                Vec::new(),
                Vec::new(),
            ));
        }
        for (private, public, wrap, info, aad) in cases {
            // When opening the altered vector, then no plaintext is released.
            assert_eq!(
                open_hpke_sealed_box(&private, &public, &wrap, &info, &aad),
                Err(HpkeSealedBoxError::OpenFailed)
            );
        }
    }

    #[test]
    fn refuses_every_box_shorter_than_overhead() {
        // Given all truncated wire lengths.
        let (private, public, wrap) = vector();
        for length in 0..48 {
            // When opening, then length refusal precedes cryptographic processing.
            assert_eq!(
                open_hpke_sealed_box(&private, &public, &wrap[..length], &[], &[]),
                Err(HpkeSealedBoxError::TooShort)
            );
        }
    }
}
