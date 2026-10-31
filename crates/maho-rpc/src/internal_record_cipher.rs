//! One direction of the sealed internal hop (senpi `internal-record-cipher.ts`, senpi#3795): AES-256-GCM records
//! framed by a 4-byte big-endian body length. The nonce is 4 zero bytes then the 8-byte big-endian record counter;
//! the AAD is the direction label then the same counter. A failed authentication never advances the receive counter.
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InternalDirection {
    C2s,
    S2c,
}
impl InternalDirection {
    pub fn label(self) -> &'static str {
        match self {
            Self::C2s => "c2s",
            Self::S2c => "s2c",
        }
    }
}

pub const INTERNAL_RECORD_MAX: usize = 64 * 1024;
const RECORD_LIMIT: u64 = 1 << 32;
const TAG: usize = 16;

/// The record contract's error codes (`internal_record_*`); never carries key or plaintext material.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InternalRecordError(pub &'static str);

pub struct InternalRecordCipher {
    cipher: Aes256Gcm,
    direction: InternalDirection,
    counter: u64,
}

impl InternalRecordCipher {
    pub fn new(key: &[u8; 32], direction: InternalDirection) -> Self {
        Self::with_counter(key, direction, 0)
    }
    pub fn with_counter(key: &[u8; 32], direction: InternalDirection, counter: u64) -> Self {
        Self {
            cipher: Aes256Gcm::new(key.into()),
            direction,
            counter,
        }
    }

    fn parameters(&self) -> Result<([u8; 12], Vec<u8>), InternalRecordError> {
        if self.counter >= RECORD_LIMIT {
            return Err(InternalRecordError("internal_record_counter_exhausted"));
        }
        let count = self.counter.to_be_bytes();
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&count);
        let mut aad = self.direction.label().as_bytes().to_vec();
        aad.extend_from_slice(&count);
        Ok((nonce, aad))
    }

    /// The whole wire record: length header then ciphertext and tag.
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, InternalRecordError> {
        if plaintext.is_empty() || plaintext.len() > INTERNAL_RECORD_MAX {
            return Err(InternalRecordError("internal_record_length"));
        }
        let (nonce, aad) = self.parameters()?;
        let body = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| InternalRecordError("internal_record_invalid"))?;
        let mut record = Vec::with_capacity(4 + body.len());
        record.extend_from_slice(&(body.len() as u32).to_be_bytes());
        record.extend_from_slice(&body);
        self.counter += 1;
        Ok(record)
    }

    /// One record body (no header). Nothing is released unless the tag verifies.
    pub fn open(&mut self, body: &[u8]) -> Result<Vec<u8>, InternalRecordError> {
        if body.len() < TAG + 1 || body.len() > INTERNAL_RECORD_MAX + TAG {
            return Err(InternalRecordError("internal_record_length"));
        }
        let (nonce, aad) = self.parameters()?;
        let plaintext = self
            .cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: body,
                    aad: &aad,
                },
            )
            .map_err(|_| InternalRecordError("internal_record_unverified"))?;
        self.counter += 1;
        Ok(plaintext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // HKDF-SHA256(ikm=[7;32], salt=[11;32]||[13;32], info="senpi-rpc-internal c2s", 32), computed independently.
    fn derived() -> [u8; 32] {
        let mut key = [0u8; 32];
        hex::decode_to_slice(
            "2142357ff086a7d3f778ed6d60855ad6d41e5bcac0d45921a22405b9206ea483",
            &mut key,
        )
        .expect("valid hex");
        key
    }

    #[test]
    fn matches_the_independent_aes_gcm_wire_contract() {
        let body = hex::decode("bb07eac88e6f0816acb0458b152a6b2a91d11c2616").expect("hex");
        let mut expected = (body.len() as u32).to_be_bytes().to_vec();
        expected.extend_from_slice(&body);
        let sealed = InternalRecordCipher::new(&derived(), InternalDirection::C2s)
            .seal(b"hello")
            .expect("seal");
        assert_eq!(sealed, expected);
        let opened = InternalRecordCipher::new(&derived(), InternalDirection::C2s)
            .open(&body)
            .expect("open");
        assert_eq!(opened, b"hello");
    }

    #[test]
    fn rejects_tamper_reorder_replay_and_direction_without_releasing_plaintext() {
        for attack in ["tamper", "reorder", "replay", "direction"] {
            let mut sender = InternalRecordCipher::new(&derived(), InternalDirection::C2s);
            let first = sender.seal(b"first").expect("seal")[4..].to_vec();
            let second = sender.seal(b"second").expect("seal")[4..].to_vec();
            let direction = if attack == "direction" {
                InternalDirection::S2c
            } else {
                InternalDirection::C2s
            };
            let mut receiver = InternalRecordCipher::new(&derived(), direction);
            let mut forged = first.clone();
            if attack == "tamper" {
                forged[0] ^= 1;
            }
            if attack == "reorder" {
                forged = second;
            }
            if attack == "replay" {
                assert_eq!(receiver.open(&first).expect("first"), b"first");
            }
            assert!(receiver.open(&forged).is_err(), "{attack}");
        }
    }

    #[test]
    fn rejects_invalid_body_lengths() {
        for length in [0, 16, 65553] {
            let error = InternalRecordCipher::new(&derived(), InternalDirection::C2s)
                .open(&vec![0; length])
                .expect_err("invalid length");
            assert_eq!(error.0, "internal_record_length");
        }
    }

    #[test]
    fn closes_a_direction_at_the_2_pow_32_record_bound() {
        let mut sender =
            InternalRecordCipher::with_counter(&derived(), InternalDirection::C2s, 0xffff_ffff);
        let mut receiver =
            InternalRecordCipher::with_counter(&derived(), InternalDirection::C2s, 0xffff_ffff);
        let record = sender.seal(b"last").expect("seal");
        assert_eq!(receiver.open(&record[4..]).expect("open"), b"last");
        assert!(sender.seal(b"overflow").is_err());
        assert!(receiver.open(&[0; 17]).is_err());
    }

    #[test]
    fn a_failed_open_does_not_advance_the_counter() {
        let mut sender = InternalRecordCipher::new(&derived(), InternalDirection::C2s);
        let record = sender.seal(b"one").expect("seal");
        let mut receiver = InternalRecordCipher::new(&derived(), InternalDirection::C2s);
        assert!(receiver.open(&[0; 20]).is_err());
        assert_eq!(receiver.open(&record[4..]).expect("open"), b"one");
    }
}
