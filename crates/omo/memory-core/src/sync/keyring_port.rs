//! Device-private key custody and purpose-bound sync signing.

use super::canonical_json::{CanonicalJsonViolation, canonicalize_json, require_canonical_json};
pub use super::hpke_sealed_box::{HpkeSealedBoxError, open_hpke_sealed_box};
use aws_lc_rs::{
    agreement,
    rand::{SecureRandom, SystemRandom},
    signature::{Ed25519KeyPair, KeyPair},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use pkcs8::{
    PrivateKeyInfo,
    der::{Decode, asn1::OctetStringRef},
};

pub enum DeviceKeyringSignRequest<'a> {
    ObjectDigest {
        digest: &'a [u8],
    },
    Record {
        canonical_json: &'a [u8],
    },
    Snapshot {
        canonical_json: &'a [u8],
    },
    /// Runtime boundary for an unrecognized wire purpose; never signs its payload.
    UnknownPurpose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKeyringSignError {
    UnknownPurpose,
    DigestLength,
    RecordJson,
    RecordPrefix,
    RecordCanonicalDuplicateKey,
    RecordCanonicalNumber,
    RecordCanonicalOrder,
    RecordCanonicalEscape,
    RecordCanonicalWhitespace,
    RecordCanonicalEncoding,
    SnapshotPrefix,
}

impl DeviceKeyringSignError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownPurpose => "unknown-purpose",
            Self::DigestLength => "digest-length",
            Self::RecordJson => "record-json",
            Self::RecordPrefix => "record-prefix",
            Self::RecordCanonicalDuplicateKey => "record-canonical-duplicate-key",
            Self::RecordCanonicalNumber => "record-canonical-number",
            Self::RecordCanonicalOrder => "record-canonical-order",
            Self::RecordCanonicalEscape => "record-canonical-escape",
            Self::RecordCanonicalWhitespace => "record-canonical-whitespace",
            Self::RecordCanonicalEncoding => "record-canonical-encoding",
            Self::SnapshotPrefix => "snapshot-prefix",
        }
    }
}

impl std::fmt::Display for DeviceKeyringSignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
impl std::error::Error for DeviceKeyringSignError {}

const OBJECT_TAG: &str = "omo-sync/v1/object-digest";
const RECORD_TAG: &str = "omo-sync/v1/record";
const SNAPSHOT_TAG: &str = "omo-sync/v1/snapshot";
const WRAP_INFO: &[u8] = b"omo-sync/v1/wrap\0";

fn validate_sign_request<'a>(
    request: &DeviceKeyringSignRequest<'a>,
) -> Result<(&'static str, &'a [u8]), DeviceKeyringSignError> {
    use DeviceKeyringSignError as Error;
    let (bytes, snapshot) = match request {
        DeviceKeyringSignRequest::UnknownPurpose => return Err(Error::UnknownPurpose),
        DeviceKeyringSignRequest::ObjectDigest { digest } => {
            return if digest.len() == 32 {
                Ok((OBJECT_TAG, digest))
            } else {
                Err(Error::DigestLength)
            };
        }
        DeviceKeyringSignRequest::Record { canonical_json } => (*canonical_json, false),
        DeviceKeyringSignRequest::Snapshot { canonical_json } => (*canonical_json, true),
    };
    let parsed = require_canonical_json(bytes).map_err(|error| match error.violation {
        CanonicalJsonViolation::Json => Error::RecordJson,
        CanonicalJsonViolation::DuplicateKey => Error::RecordCanonicalDuplicateKey,
        CanonicalJsonViolation::Number => Error::RecordCanonicalNumber,
        CanonicalJsonViolation::Order => Error::RecordCanonicalOrder,
        CanonicalJsonViolation::Escape => Error::RecordCanonicalEscape,
        CanonicalJsonViolation::Whitespace => Error::RecordCanonicalWhitespace,
        CanonicalJsonViolation::Encoding => Error::RecordCanonicalEncoding,
    })?;
    let tag = parsed.get("t").and_then(serde_json::Value::as_str);
    if snapshot {
        if tag != Some(SNAPSHOT_TAG) {
            return Err(Error::SnapshotPrefix);
        }
        Ok((SNAPSHOT_TAG, bytes))
    } else {
        let tag = tag.ok_or(Error::RecordJson)?;
        if !tag.starts_with("omo-sync/v1/") || tag == OBJECT_TAG || tag == SNAPSHOT_TAG {
            return Err(Error::RecordPrefix);
        }
        Ok((RECORD_TAG, bytes))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevicePrincipal {
    pub kind: &'static str,
    pub id: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevicePublicKeys {
    pub signing: String,
    pub sealing: String,
}
pub struct WrapBinding<'a> {
    pub account: &'a str,
    pub epoch: f64,
    pub id: &'a str,
}

#[derive(Debug)]
pub enum DeviceKeyringError {
    InvalidKeyMaterial,
    KeyGeneration,
    Signing,
    SignRequest(DeviceKeyringSignError),
    Hpke(HpkeSealedBoxError),
    InvalidEpoch,
    Canonical(super::canonical_json::CanonicalJsonError),
}
impl std::fmt::Display for DeviceKeyringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidKeyMaterial => f.write_str("invalid device key material"),
            Self::KeyGeneration => f.write_str("device key generation failed"),
            Self::Signing => f.write_str("device signing failed"),
            Self::SignRequest(error) => error.fmt(f),
            Self::Hpke(error) => error.fmt(f),
            Self::InvalidEpoch => {
                f.write_str("unseal: the wrap epoch must be a non-negative integer")
            }
            Self::Canonical(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for DeviceKeyringError {}

/// Deliberately has no Debug or Serialize implementation: private material is never logged.
pub struct DeviceKeyMaterial {
    pub signing_private_pkcs8: String,
    pub sealing_private_pkcs8: String,
}
pub struct GeneratedDeviceKeyMaterial {
    pub material: DeviceKeyMaterial,
    pub public_keys: DevicePublicKeys,
}

pub trait DeviceKeyring {
    fn principal(&self) -> DevicePrincipal;
    fn public_keys(&self) -> DevicePublicKeys;
    fn sign(&self, request: DeviceKeyringSignRequest<'_>) -> Result<Vec<u8>, DeviceKeyringError>;
    fn unseal(&self, wrap: &[u8], binding: &WrapBinding<'_>)
    -> Result<Vec<u8>, DeviceKeyringError>;
}

pub struct DeviceKeyringImpl {
    id: String,
    signing: Ed25519KeyPair,
    sealing: agreement::PrivateKey,
    sealing_public: agreement::PublicKey,
}

// RFC 8410 PKCS#8 v1: X25519 OID and nested OCTET STRING containing the scalar.
const X25519_PKCS8_PREFIX: &[u8] = &[
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e, 0x04, 0x22, 0x04, 0x20,
];

pub fn generate_device_key_material() -> Result<GeneratedDeviceKeyMaterial, DeviceKeyringError> {
    let rng = SystemRandom::new();
    let signing =
        Ed25519KeyPair::generate_pkcs8v1(&rng).map_err(|_| DeviceKeyringError::KeyGeneration)?;
    let mut scalar = [0; 32];
    rng.fill(&mut scalar)
        .map_err(|_| DeviceKeyringError::KeyGeneration)?;
    let material = DeviceKeyMaterial {
        signing_private_pkcs8: URL_SAFE_NO_PAD.encode(signing.as_ref()),
        sealing_private_pkcs8: URL_SAFE_NO_PAD.encode([X25519_PKCS8_PREFIX, &scalar].concat()),
    };
    let keyring = create_device_keyring(String::new(), &material)?;
    Ok(GeneratedDeviceKeyMaterial {
        public_keys: keyring.public_keys(),
        material,
    })
}

pub fn create_device_keyring(
    id: String,
    material: &DeviceKeyMaterial,
) -> Result<DeviceKeyringImpl, DeviceKeyringError> {
    let invalid = || DeviceKeyringError::InvalidKeyMaterial;
    let signing_der = URL_SAFE_NO_PAD
        .decode(&material.signing_private_pkcs8)
        .map_err(|_| invalid())?;
    let signing = Ed25519KeyPair::from_pkcs8(&signing_der).map_err(|_| invalid())?;
    let sealing_der = URL_SAFE_NO_PAD
        .decode(&material.sealing_private_pkcs8)
        .map_err(|_| invalid())?;
    let parsed = PrivateKeyInfo::try_from(sealing_der.as_slice()).map_err(|_| invalid())?;
    if parsed.algorithm.oid.to_string() != "1.3.101.110" || parsed.algorithm.parameters.is_some() {
        return Err(invalid());
    }
    let scalar = OctetStringRef::from_der(parsed.private_key).map_err(|_| invalid())?;
    let sealing = agreement::PrivateKey::from_private_key(&agreement::X25519, scalar.as_bytes())
        .map_err(|_| invalid())?;
    let sealing_public = sealing.compute_public_key().map_err(|_| invalid())?;
    Ok(DeviceKeyringImpl {
        id,
        signing,
        sealing,
        sealing_public,
    })
}

pub fn create_in_memory_device_keyring(
    id: String,
) -> Result<DeviceKeyringImpl, DeviceKeyringError> {
    create_device_keyring(id, &generate_device_key_material()?.material)
}

impl DeviceKeyring for DeviceKeyringImpl {
    fn principal(&self) -> DevicePrincipal {
        DevicePrincipal {
            kind: "device",
            id: self.id.clone(),
        }
    }
    fn public_keys(&self) -> DevicePublicKeys {
        DevicePublicKeys {
            signing: URL_SAFE_NO_PAD.encode(self.signing.public_key().as_ref()),
            sealing: URL_SAFE_NO_PAD.encode(self.sealing_public.as_ref()),
        }
    }
    fn sign(&self, request: DeviceKeyringSignRequest<'_>) -> Result<Vec<u8>, DeviceKeyringError> {
        let (tag, payload) =
            validate_sign_request(&request).map_err(DeviceKeyringError::SignRequest)?;
        let message = [tag.as_bytes(), &[0], payload].concat();
        self.signing
            .try_sign(&message)
            .map(|signature| signature.as_ref().to_vec())
            .map_err(|_| DeviceKeyringError::Signing)
    }
    fn unseal(
        &self,
        wrap: &[u8],
        binding: &WrapBinding<'_>,
    ) -> Result<Vec<u8>, DeviceKeyringError> {
        if !binding.epoch.is_finite()
            || binding.epoch < 0.0
            || binding.epoch.fract() != 0.0
            || binding.epoch > 9_007_199_254_740_991.0
        {
            return Err(DeviceKeyringError::InvalidEpoch);
        }
        let aad = canonicalize_json(&serde_json::json!({"account": binding.account, "epoch": binding.epoch, "id": binding.id}))
            .map_err(DeviceKeyringError::Canonical)?;
        super::hpke_sealed_box::open_with_key(
            &self.sealing,
            self.sealing_public.as_ref(),
            wrap,
            WRAP_INFO,
            &aad,
        )
        .map_err(DeviceKeyringError::Hpke)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::signature::{ED25519, UnparsedPublicKey};

    fn fixture() -> DeviceKeyringImpl {
        let signing = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).expect("fixture signing seed");
        let material = DeviceKeyMaterial {
            signing_private_pkcs8: URL_SAFE_NO_PAD
                .encode(signing.to_pkcs8v1().expect("fixture DER").as_ref()),
            sealing_private_pkcs8: URL_SAFE_NO_PAD.encode([X25519_PKCS8_PREFIX, &[9; 32]].concat()),
        };
        create_device_keyring("fixture".into(), &material).expect("valid PKCS#8 fixture")
    }

    #[test]
    fn rejects_malformed_or_wrong_algorithm_private_material() {
        let signing = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).expect("fixture seed");
        let signing_der = signing.to_pkcs8v1().expect("fixture DER");
        for sealing in [Vec::new(), vec![0; 48], signing_der.as_ref().to_vec()] {
            let material = DeviceKeyMaterial {
                signing_private_pkcs8: URL_SAFE_NO_PAD.encode(signing_der.as_ref()),
                sealing_private_pkcs8: URL_SAFE_NO_PAD.encode(sealing),
            };
            assert!(matches!(
                create_device_keyring("fixture".into(), &material),
                Err(DeviceKeyringError::InvalidKeyMaterial)
            ));
        }
        let material = DeviceKeyMaterial {
            signing_private_pkcs8: URL_SAFE_NO_PAD.encode([X25519_PKCS8_PREFIX, &[9; 32]].concat()),
            sealing_private_pkcs8: URL_SAFE_NO_PAD.encode([X25519_PKCS8_PREFIX, &[9; 32]].concat()),
        };
        assert!(matches!(
            create_device_keyring("fixture".into(), &material),
            Err(DeviceKeyringError::InvalidKeyMaterial)
        ));
    }

    #[test]
    fn signs_only_domain_separated_payloads_for_each_purpose() {
        // Given fixed imported keys and valid payloads for all three purposes.
        let keyring = fixture();
        let public = URL_SAFE_NO_PAD
            .decode(keyring.public_keys().signing)
            .expect("public base64url");
        let verifier = UnparsedPublicKey::new(&ED25519, public);
        let digest = [3; 32];
        let record = br#"{"name":"My Mac","t":"omo-sync/v1/join"}"#;
        let snapshot = br#"{"t":"omo-sync/v1/snapshot"}"#;
        for (request, tag, payload) in [
            (
                DeviceKeyringSignRequest::ObjectDigest { digest: &digest },
                OBJECT_TAG,
                digest.as_slice(),
            ),
            (
                DeviceKeyringSignRequest::Record {
                    canonical_json: record,
                },
                RECORD_TAG,
                record.as_slice(),
            ),
            (
                DeviceKeyringSignRequest::Snapshot {
                    canonical_json: snapshot,
                },
                SNAPSHOT_TAG,
                snapshot.as_slice(),
            ),
        ] {
            // When signing through the custody port.
            let signature = keyring.sign(request).expect("valid typed request");
            // Then only the matching tagged payload verifies.
            assert!(
                verifier
                    .verify(&[tag.as_bytes(), &[0], payload].concat(), &signature)
                    .is_ok()
            );
            assert!(verifier.verify(payload, &signature).is_err());
            for other in [OBJECT_TAG, RECORD_TAG, SNAPSHOT_TAG] {
                if other != tag {
                    assert!(
                        verifier
                            .verify(&[other.as_bytes(), &[0], payload].concat(), &signature)
                            .is_err()
                    );
                }
            }
        }
    }

    #[test]
    fn preserves_canonical_refusal_taxonomy_for_records_and_snapshots() {
        // Given one invalid preimage per canonical violation.
        use DeviceKeyringSignError as Error;
        let keyring = fixture();
        let cases: &[(&[u8], Error)] = &[
            (b"{", Error::RecordJson),
            (br#"{"t":1,"\u0074":2}"#, Error::RecordCanonicalDuplicateKey),
            (
                br#"{"n":1.0,"t":"omo-sync/v1/record"}"#,
                Error::RecordCanonicalNumber,
            ),
            (
                br#"{"z":1,"t":"omo-sync/v1/record"}"#,
                Error::RecordCanonicalOrder,
            ),
            (
                br#"{"t":"omo-sync\/v1/record"}"#,
                Error::RecordCanonicalEscape,
            ),
            (
                br#"{ "t":"omo-sync/v1/record"}"#,
                Error::RecordCanonicalWhitespace,
            ),
            (&[b'"', 0xff, b'"'], Error::RecordCanonicalEncoding),
        ];
        for (bytes, expected) in cases {
            for request in [
                DeviceKeyringSignRequest::Record {
                    canonical_json: bytes,
                },
                DeviceKeyringSignRequest::Snapshot {
                    canonical_json: bytes,
                },
            ] {
                // When signing either purpose, then canonical refusals share record-* reasons.
                assert!(
                    matches!(keyring.sign(request), Err(DeviceKeyringError::SignRequest(actual)) if actual == *expected)
                );
            }
        }
    }

    #[test]
    fn refuses_unknown_purposes_bad_digests_and_wrong_type_tags() {
        // Given requests outside the three signing contracts.
        use DeviceKeyringSignError as Error;
        let keyring = fixture();
        for (request, expected) in [
            (
                DeviceKeyringSignRequest::UnknownPurpose,
                Error::UnknownPurpose,
            ),
            (
                DeviceKeyringSignRequest::ObjectDigest { digest: &[0; 31] },
                Error::DigestLength,
            ),
            (
                DeviceKeyringSignRequest::ObjectDigest { digest: &[0; 33] },
                Error::DigestLength,
            ),
            (
                DeviceKeyringSignRequest::Record {
                    canonical_json: b"[]",
                },
                Error::RecordJson,
            ),
            (
                DeviceKeyringSignRequest::Record {
                    canonical_json: br#"{"t":"other"}"#,
                },
                Error::RecordPrefix,
            ),
            (
                DeviceKeyringSignRequest::Record {
                    canonical_json: br#"{"t":"omo-sync/v1/object-digest"}"#,
                },
                Error::RecordPrefix,
            ),
            (
                DeviceKeyringSignRequest::Record {
                    canonical_json: br#"{"t":"omo-sync/v1/snapshot"}"#,
                },
                Error::RecordPrefix,
            ),
            (
                DeviceKeyringSignRequest::Snapshot {
                    canonical_json: br#"{"t":"omo-sync/v1/join"}"#,
                },
                Error::SnapshotPrefix,
            ),
        ] {
            // When signing, then the exact refusal is returned without a signature.
            assert!(
                matches!(keyring.sign(request), Err(DeviceKeyringError::SignRequest(actual)) if actual == expected)
            );
        }
    }

    #[test]
    fn unseal_binds_account_epoch_recipient_and_private_key() {
        // Given a deterministic sender wrapping an envelope for one context.
        let keyring = fixture();
        let public = URL_SAFE_NO_PAD
            .decode(keyring.public_keys().sealing)
            .expect("public base64url");
        let aad = br#"{"account":"acc","epoch":2,"id":"device:fixture"}"#;
        let wrap = super::super::hpke_sealed_box::seal_fixture(
            &public,
            WRAP_INFO,
            aad,
            b"epoch-key-material",
        );
        let binding = WrapBinding {
            account: "acc",
            epoch: 2.0,
            id: "device:fixture",
        };
        // When opened under the original binding, then the envelope is recovered.
        assert_eq!(
            keyring.unseal(&wrap, &binding).expect("bound wrap opens"),
            b"epoch-key-material"
        );
        for changed in [
            WrapBinding {
                account: "other",
                ..binding
            },
            WrapBinding {
                epoch: 3.0,
                ..binding
            },
            WrapBinding {
                id: "device:other",
                ..binding
            },
        ] {
            assert!(matches!(
                keyring.unseal(&wrap, &changed),
                Err(DeviceKeyringError::Hpke(HpkeSealedBoxError::OpenFailed))
            ));
        }
        let other_private = agreement::PrivateKey::from_private_key(&agreement::X25519, &[13; 32])
            .expect("other scalar");
        let other = DeviceKeyringImpl {
            id: "other".into(),
            signing: Ed25519KeyPair::from_seed_unchecked(&[17; 32]).expect("other seed"),
            sealing_public: other_private
                .compute_public_key()
                .expect("other public key"),
            sealing: other_private,
        };
        assert!(matches!(
            other.unseal(&wrap, &binding),
            Err(DeviceKeyringError::Hpke(HpkeSealedBoxError::OpenFailed))
        ));
    }

    #[test]
    fn rejects_invalid_epochs_before_opening() {
        // Given each invalid JavaScript numeric epoch class.
        let keyring = fixture();
        for epoch in [-1.0, 1.5, f64::NAN, f64::INFINITY, 9_007_199_254_740_992.0] {
            let binding = WrapBinding {
                account: "a",
                epoch,
                id: "device:a",
            };
            // When unsealing, then epoch validation precedes the truncated-box refusal.
            assert!(matches!(
                keyring.unseal(&[], &binding),
                Err(DeviceKeyringError::InvalidEpoch)
            ));
        }
    }
}
