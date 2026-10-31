use super::types::{GenesisBootstrap, GenesisBuildError, GenesisPrepared};
use crate::sync::crypto::{parse::parse_body, signatures::*};
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

fn text(value: &str) -> bool {
    !value.is_empty() && value.nfc().collect::<String>() == value
}

pub(super) fn to_value<T: serde::Serialize>(
    value: &T,
    reason: GenesisBuildError,
) -> Result<Value, GenesisBuildError> {
    serde_json::to_value(value).map_err(|_| reason)
}

/// Public native input, checked before either sealing or recovery signing. The exact key sets
/// of the TypeScript original are enforced by the typed structs.
pub fn validate_bootstrap(bootstrap: &GenesisBootstrap) -> Result<(), GenesisBuildError> {
    let first = &bootstrap.first_device;
    let recovery = &bootstrap.recovery_key;
    let valid = text(&bootstrap.account)
        && decode_b64(&bootstrap.custody_nonce, Some(16)).is_some()
        && first.kind == "device"
        && decode_b64(&first.id, Some(16)).is_some()
        && decode_b64(&first.sealing_public_key, Some(32)).is_some()
        && is_strong_key_text(&first.signing_public_key)
        && decode_b64(&recovery.credential_id, None).is_some()
        && text(&recovery.rp_id)
        && decode_b64(&recovery.enrollment_nonce, Some(32)).is_some()
        && decode_b64(&recovery.sealing_public_key, Some(32)).is_some()
        && is_strong_key_text(&recovery.signing_public_key)
        && device_id_of(&first.signing_public_key).as_deref() == Some(first.id.as_str());
    if valid {
        Ok(())
    } else {
        Err(GenesisBuildError::BadBootstrap)
    }
}

/// Recheck the public bridge result; the bridge cannot supply a trusted root or wrap digest.
pub fn validate_prepared(prepared: &GenesisPrepared) -> Result<(), GenesisBuildError> {
    let GenesisPrepared { body, wraps } = prepared;
    let value = to_value(body, GenesisBuildError::BadRecord)?;
    if body.kind != "genesis"
        || body.epoch != 1
        || !parse_body(&value).is_some_and(|parsed| parsed.kind() == "genesis")
    {
        return Err(GenesisBuildError::BadRecord);
    }
    validate_bootstrap(&body.bootstrap)?;
    let expected = [
        format!("device:{}", body.bootstrap.first_device.id),
        format!("recovery:{}", body.bootstrap.recovery_key.credential_id),
    ];
    if !wraps.iter().zip(&expected).all(|(wrap, id)| {
        wrap.epoch == 1 && &wrap.id == id && decode_b64(&wrap.wrap, None).is_some()
    }) {
        return Err(GenesisBuildError::BadWrapSet);
    }
    let bootstrap = to_value(&body.bootstrap, GenesisBuildError::BadBootstrap)?;
    if tagged_hash("omo-sync/v1/custody-root", &bootstrap).as_deref()
        != Some(body.custody_root.as_str())
    {
        return Err(GenesisBuildError::BadCustodyRoot);
    }
    let digest = canonical(&json!(wraps))
        .map(|bytes| sha256_b64(&bytes))
        .map_err(|_| GenesisBuildError::BadWrapSet)?;
    if digest != body.wraps_digest {
        return Err(GenesisBuildError::WrapsDigestMismatch);
    }
    let context = "omo-sync/v1/recovery-countersign";
    let payload = json!({
        "t": context,
        "account": body.account,
        "custody_root": body.custody_root,
        "predecessor": "",
        "wraps_digest": body.wraps_digest,
    });
    if !verify_tagged(
        context,
        &payload,
        &body.bootstrap.recovery_key.signing_public_key,
        &body.countersign,
    ) {
        return Err(GenesisBuildError::BadCountersign);
    }
    Ok(())
}
