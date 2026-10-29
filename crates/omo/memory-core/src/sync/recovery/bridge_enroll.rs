use super::{RecoveryError, derive::*, key_material::*, ports::*};
use crate::sync::{
    crypto::{
        links::wrap_set_digest,
        parse::{RecoveryKey, Survivable},
        unwrap::{WrapOutcome, open_wrap},
    },
    keyring_port::DeviceKeyring,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;

fn survivability(flags: PasskeyFlags, attachment: Attachment, roaming: bool) -> Option<Survivable> {
    if flags.backup_eligible && flags.backup_state {
        Some(Survivable::Synced)
    } else if roaming && attachment == Attachment::CrossPlatform {
        Some(Survivable::Roaming)
    } else {
        None
    }
}
async fn abandon(
    auth: &dyn PasskeyAuthenticator,
    request: &EnrollBridgeRequest,
    credential: &str,
    reason: EnrollRefusal,
) -> Result<EnrollBridgeResult, RecoveryError> {
    auth.signal_removed(&request.rp_id, credential).await?;
    Ok(Err(reason))
}
pub async fn enroll_in_bridge(
    auth: &dyn PasskeyAuthenticator,
    keyring: &dyn DeviceKeyring,
    request: EnrollBridgeRequest,
) -> Result<EnrollBridgeResult, RecoveryError> {
    if !auth.prf_supported() {
        return Ok(Err(EnrollRefusal::NoPrf));
    }
    let nonce = random::<32>()?;
    let salt = recovery_prf_salt(&request.account, &nonce)?;
    let created = auth
        .create(CreateRequest {
            rp_id: request.rp_id.clone(),
            user_handle: recovery_user_handle(&request.account),
            user_name: request.user_name.clone(),
            challenge: random::<32>()?.to_vec(),
            prf_salt: salt.clone(),
            exclude_credential_ids: request.used_credential_ids.clone(),
            roaming: request.roaming,
        })
        .await?;
    let Some(created) = created else {
        return Ok(Err(EnrollRefusal::Cancelled));
    };
    let credential = created.credential_id;
    if request.used_credential_ids.contains(&credential) {
        return abandon(auth, &request, &credential, EnrollRefusal::CredentialReused).await;
    }
    let mut flags = created.flags;
    let mut prf = if flags.user_verified {
        created.prf_output
    } else {
        None
    };
    if prf.is_none() || survivability(flags, created.attachment, request.roaming).is_none() {
        let follow_up = auth
            .assert(AssertRequest {
                rp_id: request.rp_id.clone(),
                challenge: random::<32>()?.to_vec(),
                prf_salts: [(credential.clone(), salt)].into(),
            })
            .await?;
        let Some(follow_up) = follow_up else {
            return abandon(auth, &request, &credential, EnrollRefusal::Cancelled).await;
        };
        if follow_up.credential_id != credential {
            return abandon(auth, &request, &credential, EnrollRefusal::Cancelled).await;
        }
        if !follow_up.flags.user_verified {
            return abandon(
                auth,
                &request,
                &credential,
                EnrollRefusal::NoUserVerification,
            )
            .await;
        }
        flags = follow_up.flags;
        prf = follow_up.prf_output;
    }
    let Some(prf) = prf.filter(|value| value.len() == 32) else {
        return abandon(auth, &request, &credential, EnrollRefusal::NoPrf).await;
    };
    let Some(survivable) = survivability(flags, created.attachment, request.roaming) else {
        return abandon(auth, &request, &credential, EnrollRefusal::NotSurvivable).await;
    };
    let opened = open_wrap(
        keyring,
        &request.device_wrap,
        &request.account,
        &request.expected_history,
    );
    let WrapOutcome::Opened { keys: opened } = opened else {
        return abandon(
            auth,
            &request,
            &credential,
            EnrollRefusal::DeviceWrapRefused,
        )
        .await;
    };
    let material = from_opened(
        request.epoch,
        opened.current,
        opened.history,
        &request.expected_history,
    );
    let keys = derive_recovery_keys(&prf, &nonce)?;
    drop(prf);
    let (sealing_public_key, signing_public_key) = public_key_text(&keys);
    let wrap = seal_wrap(
        &WrapAddress {
            account: &request.account,
            epoch: request.epoch,
            id: &format!("recovery:{credential}"),
        },
        &sealing_public_key,
        &material,
    )?;
    let Some(wraps_digest) = wrap_set_digest(std::slice::from_ref(&wrap)) else {
        return abandon(
            auth,
            &request,
            &credential,
            EnrollRefusal::DeviceWrapRefused,
        )
        .await;
    };
    let countersign = sign_framed(
        "omo-sync/v1/recovery-countersign",
        &json!({"t":"omo-sync/v1/recovery-countersign","account":request.account,"custody_root":request.custody_root,"predecessor":request.predecessor,"wraps_digest":wraps_digest}),
        keys.sign_seed.as_ref(),
    )?;
    Ok(Ok(EnrolledKey {
        key: RecoveryKey {
            credential_id: credential,
            rp_id: request.rp_id,
            enrollment_nonce: URL_SAFE_NO_PAD.encode(nonce),
            sealing_public_key,
            signing_public_key,
            survivable,
        },
        countersign,
        wrap,
        wraps_digest,
    }))
}
