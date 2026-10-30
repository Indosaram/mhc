//! Genesis conformance: deterministic sealer and vector builder (the Rust form of
//! `seal.test-support.ts` and `vector.test-support.ts`) plus the build and rejection tests.

use super::*;
use crate::sync::{
    crypto::{
        anchor::{ChainAnchor, creator_anchor},
        chain::{AppendOutcome, append_entries, empty_chain},
        entry::{Head, SyncRecordFields},
        rejection::create_rejection_counter,
        signatures::*,
        vendor::sync_format::keys::{hkdf_sha256, seal},
    },
    keyring_port::{DeviceKeyMaterial, DeviceKeyring, DeviceKeyringImpl, create_device_keyring},
    recovery::{
        derive::x25519_public_key, key_material::WrapAddress, keyring_append::signed_append,
    },
};
use aws_lc_rs::{
    agreement, hmac,
    signature::{Ed25519KeyPair, KeyPair},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::cell::Cell;

const ACCOUNT: &str = "genesis-vector-account";
const GOLDEN: &str = include_str!("fixtures/genesis.json");
const X25519_PKCS8_PREFIX: &[u8] = &[
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e, 0x04, 0x22, 0x04, 0x20,
];

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn extract(suite: &[u8], label: &[u8], ikm: &[u8], salt: &[u8]) -> Vec<u8> {
    hmac::sign(
        &hmac::Key::new(hmac::HMAC_SHA256, salt),
        &[b"HPKE-v1".as_slice(), suite, label, ikm].concat(),
    )
    .as_ref()
    .to_vec()
}

fn expand(suite: &[u8], label: &[u8], prk: &[u8], context: &[u8], length: u16) -> Vec<u8> {
    let tag = hmac::sign(
        &hmac::Key::new(hmac::HMAC_SHA256, prk),
        &[
            &length.to_be_bytes(),
            b"HPKE-v1".as_slice(),
            suite,
            label,
            context,
            &[1],
        ]
        .concat(),
    );
    tag.as_ref()[..usize::from(length)].to_vec()
}

/// Independent RFC 9180 sender with test-owned encapsulation seeds (21 device, 22 recovery).
fn deterministic_seal(address: &WrapAddress<'_>, recipient: &str) -> String {
    let device = address.id.starts_with("device:");
    let nonce = [if device { 11u8 } else { 12 }; 12];
    let address_json =
        json!({"account": address.account, "epoch": address.epoch, "id": address.id});
    let history_aad = canonical(
        &json!({"account": address.account, "epoch": address.epoch, "id": address.id, "version": 1}),
    )
    .expect("history aad");
    let encrypted_history = seal(
        &[7; 32],
        nonce,
        &history_aad,
        &canonical(&json!([])).expect("empty history"),
    )
    .expect("history seal");
    let plaintext = canonical(&json!({
        "version": 1, "account": address.account, "epoch": address.epoch, "id": address.id,
        "key_material": {"version": 1, "current_key": b64(&[7; 32]),
            "history": {"nonce": b64(&nonce), "ciphertext": b64(&encrypted_history)}},
    }))
    .expect("envelope");
    let ephemeral = agreement::PrivateKey::from_private_key(
        &agreement::X25519,
        &[if device { 21u8 } else { 22 }; 32],
    )
    .expect("ephemeral");
    let enc = ephemeral.compute_public_key().expect("encapsulation");
    let public = decode_b64(recipient, Some(32)).expect("recipient key");
    let peer = agreement::UnparsedPublicKey::new(&agreement::X25519, &public);
    const KEM: &[u8] = b"KEM\x00\x20";
    const SUITE: &[u8] = b"HPKE\x00\x20\x00\x01\x00\x02";
    let shared = agreement::agree(&ephemeral, peer, (), |dh| {
        Ok(expand(
            KEM,
            b"shared_secret",
            &extract(KEM, b"eae_prk", dh, &[]),
            &[enc.as_ref(), public.as_slice()].concat(),
            32,
        ))
    })
    .expect("dh");
    let context = [
        &[0],
        extract(SUITE, b"psk_id_hash", &[], &[]).as_slice(),
        extract(SUITE, b"info_hash", b"omo-sync/v1/wrap\0", &[]).as_slice(),
    ]
    .concat();
    let secret = extract(SUITE, b"secret", &[], &shared);
    let key = expand(SUITE, b"key", &secret, &context, 32);
    let base_nonce: [u8; 12] = expand(SUITE, b"base_nonce", &secret, &context, 12)
        .try_into()
        .expect("nonce");
    let ciphertext = seal(
        &key,
        base_nonce,
        &canonical(&address_json).expect("aad"),
        &plaintext,
    )
    .expect("hpke seal");
    b64(&[enc.as_ref(), &ciphertext].concat())
}

struct VectorCapabilities {
    sign_seed: [u8; 32],
    seal_calls: Cell<u32>,
    countersign_calls: Cell<u32>,
}

impl GenesisCapabilities for VectorCapabilities {
    fn seal(&self, address: &WrapAddress<'_>, key: &str) -> Result<String, GenesisBuildError> {
        self.seal_calls.set(self.seal_calls.get() + 1);
        Ok(deterministic_seal(address, key))
    }
    fn countersign(
        &self,
        payload: &GenesisCountersignPayload,
    ) -> Result<String, GenesisBuildError> {
        self.countersign_calls.set(self.countersign_calls.get() + 1);
        Ok(sign_framed(
            payload.t,
            &serde_json::to_value(payload).expect("payload"),
            &self.sign_seed,
        ))
    }
}

fn sign_framed(context: &str, payload: &Value, seed: &[u8]) -> String {
    let key = Ed25519KeyPair::from_seed_unchecked(seed).expect("seed");
    let message = [
        context.as_bytes(),
        &[0],
        &canonical(payload).expect("payload"),
    ]
    .concat();
    b64(key.sign(&message).as_ref())
}

fn device_keyring(seed: u8) -> DeviceKeyringImpl {
    let pair = Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).expect("signing seed");
    let material = DeviceKeyMaterial {
        signing_private_pkcs8: b64(pair.to_pkcs8v1().expect("pkcs8").as_ref()),
        sealing_private_pkcs8: b64(&[X25519_PKCS8_PREFIX, &[seed + 1; 32]].concat()),
    };
    let temporary = create_device_keyring(String::new(), &material).expect("keyring");
    let id = device_id_of(&temporary.public_keys().signing).expect("device id");
    create_device_keyring(id, &material).expect("keyring")
}

struct Vector {
    keyring: DeviceKeyringImpl,
    bootstrap: GenesisBootstrap,
    capabilities: VectorCapabilities,
    prepared: GenesisPrepared,
    request: GenesisAppend,
    digest: String,
}

fn vector() -> Vector {
    let keyring = device_keyring(1);
    let keys = keyring.public_keys();
    let sign_seed = hkdf_sha256(&[3; 32], &[4; 32], b"omo-sync/v1/recovery-sign");
    let seal_private = hkdf_sha256(&[3; 32], &[4; 32], b"omo-sync/v1/recovery-seal");
    let sign_public = Ed25519KeyPair::from_seed_unchecked(&sign_seed)
        .expect("recovery seed")
        .public_key()
        .as_ref()
        .to_vec();
    let bootstrap = GenesisBootstrap {
        account: ACCOUNT.into(),
        custody_nonce: b64(&[5; 16]),
        first_device: crate::sync::crypto::parse::Principal {
            kind: "device".into(),
            id: keyring.principal().id,
            sealing_public_key: keys.sealing,
            signing_public_key: keys.signing,
        },
        recovery_key: crate::sync::crypto::parse::RecoveryKey {
            credential_id: b64(&[6; 16]),
            rp_id: "recovery.omo.dev".into(),
            enrollment_nonce: b64(&[4; 32]),
            sealing_public_key: b64(&x25519_public_key(&seal_private).expect("seal public")),
            signing_public_key: b64(&sign_public),
            survivable: crate::sync::crypto::parse::Survivable::Synced,
        },
    };
    let capabilities = VectorCapabilities {
        sign_seed,
        seal_calls: Cell::new(0),
        countersign_calls: Cell::new(0),
    };
    let prepared = prepare_genesis(&bootstrap, &capabilities).expect("prepared");
    let (request, digest) = build_genesis(ACCOUNT, &keyring, &prepared).expect("built");
    Vector {
        keyring,
        bootstrap,
        capabilities,
        prepared,
        request,
        digest,
    }
}

fn built_json(v: &Vector) -> Value {
    json!({"request": v.request, "digest": v.digest})
}

fn walk(
    request: &GenesisAppend,
    signer: &DeviceKeyringImpl,
    anchor: &DeviceKeyringImpl,
) -> AppendOutcome {
    let keys = signer.public_keys();
    let row = json!({
        "seq": 1, "kind": "keyring", "account": request.record.account, "epoch": 1,
        "author": {"kind": "device", "id": signer.principal().id,
            "sealing_public_key": keys.sealing, "signing_public_key": keys.signing},
        "body": request.record,
        "digest": sha256_b64(&canonical(&json!(request.record)).expect("body")),
        "base_seq": 0, "link": request.link, "keyring_link": request.keyring_link,
        "record_sig": request.record_sig, "wraps": request.wraps,
    });
    append_entries(
        &empty_chain(
            ACCOUNT.into(),
            ChainAnchor::Bootstrap(creator_anchor(ACCOUNT.into(), anchor)),
        ),
        &[row],
        &mut create_rejection_counter(None),
    )
}

#[test]
fn recreates_every_wire_byte_of_the_builder_produced_golden_vector() {
    let golden: Value = serde_json::from_str(GOLDEN).expect("golden json");
    assert_eq!(
        canonical(&built_json(&vector())).expect("built"),
        canonical(&golden).expect("golden")
    );
}

#[test]
fn builds_a_closed_linked_genesis_that_the_receiver_anchors() {
    let v = vector();
    assert_eq!(v.request.base_seq, 0);
    assert_eq!(v.request.keyring_link, v.request.link);
    assert_eq!(
        v.digest,
        sha256_b64(&canonical(&json!(v.request.record)).expect("record"))
    );
    let AppendOutcome::Accepted { state, .. } = walk(&v.request, &v.keyring, &v.keyring) else {
        panic!("genesis was rejected")
    };
    assert!(state.anchored());
    assert_eq!(state.authority().expect("authority").epoch, 1);
    let members: Vec<_> = state
        .authority()
        .expect("authority")
        .members
        .values()
        .cloned()
        .collect();
    assert_eq!(members, vec![v.bootstrap.first_device.clone()]);
}

#[test]
fn rejects_a_different_creator_anchor_after_a_valid_signature() {
    let v = vector();
    let AppendOutcome::Rejected { reason, .. } = walk(&v.request, &v.keyring, &device_keyring(41))
    else {
        panic!("a foreign anchor must reject")
    };
    assert_eq!(reason, "anchor_mismatch");
}

#[test]
fn refuses_bad_bootstrap_before_sealing_or_countersigning() {
    for mutation in [
        "nonce length",
        "non-NFC RP",
        "invalid credential",
        "empty credential",
    ] {
        let v = vector();
        let mut bootstrap = v.bootstrap.clone();
        match mutation {
            "nonce length" => bootstrap.custody_nonce = b64(&[5; 32]),
            "non-NFC RP" => bootstrap.recovery_key.rp_id = "e\u{301}.test".into(),
            "invalid credential" => bootstrap.recovery_key.credential_id = "not/base64url".into(),
            _ => bootstrap.recovery_key.credential_id = String::new(),
        }
        let capabilities = VectorCapabilities {
            sign_seed: v.capabilities.sign_seed,
            seal_calls: Cell::new(0),
            countersign_calls: Cell::new(0),
        };
        assert_eq!(
            prepare_genesis(&bootstrap, &capabilities).unwrap_err(),
            GenesisBuildError::BadBootstrap,
            "{mutation}"
        );
        assert_eq!(capabilities.seal_calls.get(), 0, "{mutation}");
        assert_eq!(capabilities.countersign_calls.get(), 0, "{mutation}");
    }
}

#[test]
fn rejects_bootstrap_and_body_with_an_extra_key() {
    let v = vector();
    let mut bootstrap = serde_json::to_value(&v.bootstrap).expect("bootstrap");
    bootstrap["extra"] = json!(true);
    assert!(serde_json::from_value::<GenesisBootstrap>(bootstrap).is_err());
    let mut body = serde_json::to_value(&v.prepared.body).expect("body");
    body["extra"] = json!(true);
    assert!(serde_json::from_value::<GenesisBody>(body).is_err());
}

#[test]
fn refuses_prepared_ciphertext_tampering_before_device_signing() {
    let v = vector();
    let mut prepared = v.prepared.clone();
    prepared.wraps[1].wrap = prepared.wraps[0].wrap.clone();
    prepared.body.wraps_digest = sha256_b64(&canonical(&json!(prepared.wraps)).expect("wraps"));
    assert_eq!(
        build_genesis(ACCOUNT, &v.keyring, &prepared).unwrap_err(),
        GenesisBuildError::BadCountersign
    );
}

#[test]
fn refuses_wrap_set_and_digest_mutations_with_their_own_reason() {
    let v = vector();
    let mut reversed = v.prepared.clone();
    reversed.wraps.swap(0, 1);
    assert_eq!(
        validate_prepared(&reversed).unwrap_err(),
        GenesisBuildError::BadWrapSet
    );
    let mut digest = v.prepared.clone();
    digest.body.wraps_digest = sha256_b64(b"other");
    assert_eq!(
        validate_prepared(&digest).unwrap_err(),
        GenesisBuildError::WrapsDigestMismatch
    );
}

#[test]
fn refuses_account_and_principal_mismatch_before_validation() {
    let v = vector();
    assert_eq!(
        build_genesis("another-account", &v.keyring, &v.prepared).unwrap_err(),
        GenesisBuildError::AccountMismatch
    );
    let mut swapped = v.prepared.clone();
    swapped.body.bootstrap.first_device.sealing_public_key = swapped
        .body
        .bootstrap
        .recovery_key
        .sealing_public_key
        .clone();
    assert_eq!(
        build_genesis(ACCOUNT, &v.keyring, &swapped).unwrap_err(),
        GenesisBuildError::PrincipalMismatch
    );
}

#[test]
fn signed_append_over_the_empty_head_matches_the_built_request() {
    let v = vector();
    let head = Head {
        seq: 0,
        link: String::new(),
        keyring_link: String::new(),
    };
    let signed = signed_append(
        &v.keyring,
        &head,
        json!(v.request.record),
        v.request.wraps.to_vec(),
    )
    .expect("signed");
    assert_eq!(signed.record_sig, v.request.record_sig);
    let fields = SyncRecordFields {
        account: ACCOUNT.into(),
        base_seq: 0,
        digest: v.digest.clone(),
        epoch: 1,
        link: signed.link,
        keyring_link: Some(signed.keyring_link),
    };
    assert!(verify_tagged(
        "omo-sync/v1/record",
        &record_payload(&fields),
        &v.keyring.public_keys().signing,
        &signed.record_sig
    ));
}
