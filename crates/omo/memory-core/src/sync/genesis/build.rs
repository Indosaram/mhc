use super::types::{GenesisAppend, GenesisBuildError, GenesisPrepared};
use super::validate::{to_value, validate_prepared};
use crate::sync::{
    crypto::{
        entry::{Head, SyncRecordFields},
        signatures::*,
    },
    keyring_port::DeviceKeyring,
    recovery::keyring_append::signed_append,
};

/// Finish a native-prepared genesis through this device's typed signer. No private keys,
/// network calls, consent changes or trust-pin writes: the coordinator owns those boundaries.
pub fn build_genesis(
    account: &str,
    keyring: &dyn DeviceKeyring,
    input: &GenesisPrepared,
) -> Result<(GenesisAppend, String), GenesisBuildError> {
    let prepared = input.clone();
    let GenesisPrepared { body, wraps } = &prepared;
    if body.account != account || body.bootstrap.account != account {
        return Err(GenesisBuildError::AccountMismatch);
    }
    let own = keyring.principal();
    let keys = keyring.public_keys();
    let first = &body.bootstrap.first_device;
    if own.kind != first.kind
        || own.id != first.id
        || keys.sealing != first.sealing_public_key
        || keys.signing != first.signing_public_key
    {
        return Err(GenesisBuildError::PrincipalMismatch);
    }
    validate_prepared(&prepared)?;
    let record = to_value(body, GenesisBuildError::BadRecord)?;
    let digest = sha256_b64(&canonical(&record).map_err(|_| GenesisBuildError::BadRecord)?);
    let head = Head {
        seq: 0,
        link: String::new(),
        keyring_link: String::new(),
    };
    let signed = signed_append(keyring, &head, record, wraps.to_vec())
        .map_err(|error| GenesisBuildError::Capability(error.to_string()))?;
    let fields = SyncRecordFields {
        account: account.into(),
        base_seq: 0,
        digest: digest.clone(),
        epoch: 1,
        link: signed.link.clone(),
        keyring_link: Some(signed.keyring_link.clone()),
    };
    if !verify_tagged(
        "omo-sync/v1/record",
        &record_payload(&fields),
        &keys.signing,
        &signed.record_sig,
    ) {
        return Err(GenesisBuildError::BadSignature);
    }
    Ok((
        GenesisAppend {
            record: body.clone(),
            base_seq: 0,
            link: signed.link,
            keyring_link: signed.keyring_link,
            record_sig: signed.record_sig,
            wraps: wraps.clone(),
        },
        digest,
    ))
}
