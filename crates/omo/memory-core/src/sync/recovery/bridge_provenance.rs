use super::{
    derive::RecoveryKeyPairs,
    key_material::{EpochMaterial, WrapAddress, open_wrap_with_key},
    ports::{RecoverRefusal, RecoveryCandidate},
};
use crate::sync::crypto::{entry::WrapEntry, links::wrap_set_digest};

pub fn proven_recovery_material(
    account: &str,
    candidate: &RecoveryCandidate,
    keys: &RecoveryKeyPairs,
) -> Result<EpochMaterial, RecoverRefusal> {
    let id = format!("recovery:{}", candidate.key.credential_id);
    let listed = &candidate.enrollment.wraps;
    let matching: Vec<_> = listed.iter().filter(|wrap| wrap.id == id).collect();
    let Some(enrollment) = matching.first() else {
        return Err(RecoverRefusal::WrapUnbound);
    };
    if matching.len() != 1
        || wrap_set_digest(listed).as_deref() != Some(&candidate.countersign.wraps_digest)
        || listed.iter().any(|wrap| wrap.epoch != enrollment.epoch)
    {
        return Err(RecoverRefusal::WrapUnbound);
    }
    let open = |wrap: &WrapEntry, history| {
        open_wrap_with_key(
            keys.seal_private.as_ref(),
            &keys.seal_public,
            wrap,
            &WrapAddress {
                account,
                epoch: wrap.epoch,
                id: &id,
            },
            history,
        )
        .map_err(|_| RecoverRefusal::WrapRefused)
    };
    if candidate.wrap == **enrollment {
        return open(&candidate.wrap, &candidate.enrollment.expected_history);
    }
    if candidate.wrap.epoch <= enrollment.epoch {
        return Err(RecoverRefusal::WrapUnbound);
    }
    let enrolled = open(enrollment, &candidate.enrollment.expected_history)?;
    let later = open(&candidate.wrap, &candidate.expected_history)?;
    let at_enrollment = later
        .history
        .iter()
        .find(|row| row.epoch == enrollment.epoch)
        .ok_or(RecoverRefusal::WrapUnbound)?;
    if at_enrollment.key.len() != enrolled.current.len()
        || at_enrollment
            .key
            .iter()
            .zip(enrolled.current.iter())
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            != 0
    {
        return Err(RecoverRefusal::WrapUnbound);
    }
    Ok(later)
}
