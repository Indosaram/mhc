use super::{
    RecoveryError,
    bridge_provenance::proven_recovery_material,
    derive::*,
    key_material::{WrapAddress, seal_wrap},
    ports::*,
    recovery_handles::{HeldRecovery, RecoveryHandles},
};
use crate::sync::crypto::{entry::SyncRecordFields, links::next_link, signatures::*};
use serde_json::json;
use std::collections::BTreeMap;

pub async fn recover_begin_in_bridge(
    auth: &dyn PasskeyAuthenticator,
    handles: &RecoveryHandles,
    request: RecoverBeginRequest,
) -> Result<RecoverBeginResult, RecoveryError> {
    handles.dispose_all();
    if !auth.prf_supported() {
        return Ok(Err(RecoverRefusal::NoPrf));
    }
    if !request.acknowledge_unknown
        && let Some(discovered) = auth.discover(&request.rp_id).await?
    {
        let user_handle = recovery_user_handle(&request.account);
        if discovered.iter().any(|credential| {
            credential.user_handle == user_handle
                && !request
                    .known_credential_ids
                    .contains(&credential.credential_id)
        }) {
            return Ok(Err(RecoverRefusal::HistoryIncomplete));
        }
    }
    let nonces: BTreeMap<_, _> = request
        .salts
        .into_iter()
        .map(|salt| (salt.credential_id, salt.enrollment_nonce))
        .collect();
    if nonces.is_empty() {
        return Ok(Err(RecoverRefusal::NotEnrolled));
    }
    let mut salts = BTreeMap::new();
    for (credential, nonce) in &nonces {
        let nonce = decode_b64(nonce, Some(32)).ok_or(RecoveryError::InvalidInput(
            "invalid recovery enrollment nonce",
        ))?;
        salts.insert(
            credential.clone(),
            recovery_prf_salt(&request.account, &nonce)?,
        );
    }
    let assertion = auth
        .assert(AssertRequest {
            rp_id: request.rp_id,
            challenge: random::<32>()?.to_vec(),
            prf_salts: salts,
        })
        .await?;
    let Some(assertion) = assertion else {
        return Ok(Err(RecoverRefusal::Cancelled));
    };
    if !assertion.flags.user_verified {
        return Ok(Err(RecoverRefusal::NoUserVerification));
    }
    let Some(nonce) = nonces.get(&assertion.credential_id) else {
        return Ok(Err(RecoverRefusal::NotEnrolled));
    };
    let Some(prf) = assertion.prf_output.filter(|prf| prf.len() == 32) else {
        return Ok(Err(RecoverRefusal::NoPrf));
    };
    let nonce = decode_b64(nonce, Some(32)).ok_or(RecoveryError::InvalidInput(
        "invalid recovery enrollment nonce",
    ))?;
    let keys = derive_recovery_keys(&prf, &nonce)?;
    drop(prf);
    let (sealing_public_key, signing_public_key) = public_key_text(&keys);
    let handle = handles.hold(HeldRecovery {
        account: request.account,
        credential_id: assertion.credential_id.clone(),
        sealing_public_key: sealing_public_key.clone(),
        signing_public_key: signing_public_key.clone(),
        keys,
    })?;
    Ok(Ok(BegunRecovery {
        handle,
        credential_id: assertion.credential_id,
        sealing_public_key,
        signing_public_key,
    }))
}
fn anchor_holds(
    account: &str,
    candidate: &RecoveryCandidate,
    keys: &RecoveryKeyPairs,
) -> Result<(), RecoverRefusal> {
    let (sealing, signing) = public_key_text(keys);
    if sealing != candidate.key.sealing_public_key || signing != candidate.key.signing_public_key {
        return Err(RecoverRefusal::KeyMismatch);
    }
    let proof = &candidate.countersign;
    if !verify_tagged(
        "omo-sync/v1/recovery-countersign",
        &json!({"t":"omo-sync/v1/recovery-countersign","account":account,"custody_root":proof.custody_root,"predecessor":proof.predecessor,"wraps_digest":proof.wraps_digest}),
        &signing,
        &proof.signature,
    ) {
        return Err(RecoverRefusal::CountersignMismatch);
    }
    Ok(())
}
fn sign_join(
    request: &RecoverFinishRequest,
    credential: &str,
    keys: &RecoveryKeyPairs,
) -> Result<Option<SignedJoin>, RecoveryError> {
    let body = json!({"kind":"recovery-join","account":request.account,"epoch":request.join.epoch,"keyring_head":request.join.keyring_head,"principal":request.device,"credential_id":credential});
    let digest = sha256_b64(&canonical(&body)?);
    let (Some(link), Some(keyring_link)) = (
        next_link(&request.join.prior_link, &digest),
        next_link(&request.join.keyring_head, &digest),
    ) else {
        return Ok(None);
    };
    let fields = SyncRecordFields {
        account: request.account.clone(),
        base_seq: request.join.base_seq,
        digest: digest.clone(),
        epoch: request.join.epoch,
        link: link.clone(),
        keyring_link: Some(keyring_link.clone()),
    };
    let record_sig = sign_framed(
        "omo-sync/v1/record",
        &record_payload(&fields),
        keys.sign_seed.as_ref(),
    )?;
    Ok(Some(SignedJoin {
        body,
        digest,
        link,
        keyring_link,
        record_sig,
    }))
}
pub fn recover_finish_in_bridge(
    handles: &RecoveryHandles,
    request: RecoverFinishRequest,
) -> Result<RecoverBridgeResult, RecoveryError> {
    let Some(held) = handles.take(&request.handle) else {
        return Ok(Err(RecoverRefusal::RecoveryExpired));
    };
    if held.account != request.account || request.candidate.key.credential_id != held.credential_id
    {
        return Ok(Err(RecoverRefusal::KeyMismatch));
    }
    if let Err(reason) = anchor_holds(&request.account, &request.candidate, &held.keys) {
        return Ok(Err(reason));
    }
    let material = match proven_recovery_material(&request.account, &request.candidate, &held.keys)
    {
        Ok(value) => value,
        Err(reason) => return Ok(Err(reason)),
    };
    let device_wrap = seal_wrap(
        &WrapAddress {
            account: &request.account,
            epoch: request.candidate.wrap.epoch,
            id: &format!("device:{}", request.device.id),
        },
        &request.device.sealing_public_key,
        &material,
    )?;
    let Some(join) = sign_join(&request, &held.credential_id, &held.keys)? else {
        return Ok(Err(RecoverRefusal::WrapRefused));
    };
    Ok(Ok(RecoveredKey {
        credential_id: held.credential_id,
        device_wrap,
        join,
    }))
}
