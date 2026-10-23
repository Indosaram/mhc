use super::super::canonical_json::canonicalize_json;
use super::{entry::*, fold::Authority, strict_ed25519::*};
use aws_lc_rs::{
    digest,
    signature::{ED25519, UnparsedPublicKey},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};

pub fn decode_b64(value: &str, length: Option<usize>) -> Option<Vec<u8>> {
    if value.is_empty() {
        return None;
    }
    URL_SAFE_NO_PAD
        .decode(value)
        .ok()
        .filter(|v| length.is_none_or(|n| v.len() == n))
}
pub fn sha256(bytes: &[u8]) -> Vec<u8> {
    digest::digest(&digest::SHA256, bytes).as_ref().to_vec()
}
pub fn sha256_b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(sha256(bytes))
}
pub fn canonical(
    value: &Value,
) -> Result<Vec<u8>, super::super::canonical_json::CanonicalJsonError> {
    let mut stack = vec![value];
    while let Some(node) = stack.pop() {
        match node {
            Value::Number(n)
                if !n
                    .as_f64()
                    .is_some_and(|v| v.fract() == 0.0 && v.abs() <= 9_007_199_254_740_991.0) =>
            {
                return Err(super::super::canonical_json::CanonicalJsonError::new(
                    super::super::canonical_json::CanonicalJsonViolation::Number,
                    "sync format requires safe integers",
                ));
            }
            Value::Array(rows) => stack.extend(rows),
            Value::Object(row) => stack.extend(row.values()),
            _ => {}
        }
    }
    canonicalize_json(value)
}
pub fn verify_bytes(context: &str, payload: &[u8], key: &[u8], signature: &[u8]) -> bool {
    key.len() == 32
        && signature.len() == 64
        && UnparsedPublicKey::new(&ED25519, key)
            .verify(&[context.as_bytes(), &[0], payload].concat(), signature)
            .is_ok()
}
pub fn verify_tagged(context: &str, payload: &Value, public_key: &str, signature: &str) -> bool {
    let (Some(key), Some(sig), Ok(bytes)) = (
        decode_b64(public_key, Some(32)),
        decode_b64(signature, Some(64)),
        canonical(payload),
    ) else {
        return false;
    };
    if strict_ed25519_precheck(&key, &sig).is_err() {
        return false;
    }
    verify_bytes(context, &bytes, &key, &sig)
}
/// True only for a base64url key that decodes to 32 bytes and passes the strict check.
pub fn is_strong_key_text(public_key: &str) -> bool {
    decode_b64(public_key, Some(32)).is_some_and(|key| strict_ed25519_key(&key))
}
pub fn tagged_hash(context: &str, payload: &Value) -> Option<String> {
    Some(sha256_b64(
        &[context.as_bytes(), &[0], &canonical(payload).ok()?].concat(),
    ))
}
pub fn device_id_of(key: &str) -> Option<String> {
    let digest = sha256(
        &[
            b"omo-sync/v1/device-id\0".as_slice(),
            &decode_b64(key, Some(32))?,
        ]
        .concat(),
    );
    Some(URL_SAFE_NO_PAD.encode(&digest[..16]))
}
pub fn record_payload(f: &SyncRecordFields) -> Value {
    let mut row = json!({"t":"omo-sync/v1/record","account":f.account,"base_seq":f.base_seq,
        "digest":f.digest,"epoch":f.epoch,"link":f.link});
    if let Some(link) = &f.keyring_link {
        row["keyring_link"] = link.clone().into();
    }
    row
}
pub fn author_signing_key<'a>(a: &'a Authority, author: &SyncAuthor) -> Option<&'a str> {
    match author {
        SyncAuthor::Device { id, .. } => a.members.get(id).map(|p| p.signing_public_key.as_str()),
        SyncAuthor::Recovery { credential_id } => a
            .registry
            .get(credential_id)
            .map(|p| p.signing_public_key.as_str()),
    }
}
pub fn verify_record_sig(key: &str, entry: &SyncEntry) -> bool {
    verify_tagged(
        "omo-sync/v1/record",
        &record_payload(entry.fields()),
        key,
        entry.signature(),
    )
}
