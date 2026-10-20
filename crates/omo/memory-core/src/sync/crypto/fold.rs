use super::parse::{ParsedBody, Principal, RecoveryKey};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct Authority {
    pub members: BTreeMap<String, Principal>,
    pub registry: BTreeMap<String, RecoveryKey>,
    pub epoch: u64,
    pub custody_root: String,
    pub epoch_origins: BTreeMap<u64, String>,
    pub admitted_by: BTreeMap<String, String>,
    pub enrolled_ever: BTreeSet<String>,
    pub cancelled: BTreeSet<String>,
}
#[derive(Debug, Clone)]
pub struct FoldedRecord {
    pub seq: u64,
    pub digest: String,
    pub body: ParsedBody,
    pub before: Option<Authority>,
}
pub fn apply_record(history: &[FoldedRecord], record: &FoldedRecord) -> Option<Authority> {
    let body = &record.body;
    let Some(before) = &record.before else {
        let ParsedBody::Genesis {
            first_device,
            recovery_key,
            custody_root,
            ..
        } = body
        else {
            return None;
        };
        return Some(Authority {
            members: [(first_device.id.clone(), first_device.clone())].into(),
            registry: [(recovery_key.credential_id.clone(), recovery_key.clone())].into(),
            epoch: 1,
            custody_root: custody_root.clone(),
            epoch_origins: [(1, record.digest.clone())].into(),
            admitted_by: [(first_device.id.clone(), record.digest.clone())].into(),
            enrolled_ever: [recovery_key.credential_id.clone()].into(),
            cancelled: BTreeSet::new(),
        });
    };
    let mut next = before.clone();
    match body {
        ParsedBody::Genesis { .. } => return None,
        ParsedBody::ForkResolution {
            members,
            recovery_keys,
            epoch,
        } => {
            next.members = members.iter().map(|p| (p.id.clone(), p.clone())).collect();
            next.registry = recovery_keys
                .iter()
                .map(|p| (p.credential_id.clone(), p.clone()))
                .collect();
            next.enrolled_ever
                .extend(recovery_keys.iter().map(|p| p.credential_id.clone()));
            next.epoch = *epoch;
            if *epoch != before.epoch {
                next.epoch_origins.insert(*epoch, record.digest.clone());
            }
        }
        ParsedBody::Approval { principal, .. } => {
            next.members.insert(principal.id.clone(), principal.clone());
            next.admitted_by
                .insert(principal.id.clone(), record.digest.clone());
        }
        ParsedBody::RecoveryKey { key, .. } => {
            next.registry.insert(key.credential_id.clone(), key.clone());
            next.enrolled_ever.insert(key.credential_id.clone());
        }
        ParsedBody::RecoveryKeyRemoved { credential_id } => {
            next.registry.remove(credential_id);
        }
        ParsedBody::Rotation { members, .. } => {
            next.members = members.iter().map(|p| (p.id.clone(), p.clone())).collect();
            next.epoch = before.epoch.checked_add(1)?;
            next.epoch_origins.insert(next.epoch, record.digest.clone());
        }
        ParsedBody::RecoveryJoin {
            principal,
            credential_id,
            ..
        } => {
            let key = before.registry.get(credential_id)?;
            next.members = [(principal.id.clone(), principal.clone())].into();
            next.registry = [(key.credential_id.clone(), key.clone())].into();
            next.admitted_by
                .insert(principal.id.clone(), record.digest.clone());
        }
        ParsedBody::RecoveryJoinVetoed { join_digest } => {
            let join = history.iter().find(|r| r.digest == *join_digest)?;
            let ParsedBody::RecoveryJoin { credential_id, .. } = &join.body else {
                return None;
            };
            let prior = join.before.as_ref()?;
            next.members = prior.members.clone();
            next.registry = prior.registry.clone();
            next.registry.remove(credential_id);
            next.cancelled.extend(
                history
                    .iter()
                    .filter(|r| r.seq > join.seq)
                    .map(|r| r.digest.clone()),
            );
        }
    }
    Some(next)
}
pub fn is_vetoed_epoch(a: &Authority, epoch: u64) -> bool {
    a.epoch_origins
        .get(&epoch)
        .is_some_and(|d| a.cancelled.contains(d))
}
pub fn is_cancelled_admission(a: &Authority, id: &str) -> bool {
    a.admitted_by
        .get(id)
        .is_some_and(|d| a.cancelled.contains(d))
}
