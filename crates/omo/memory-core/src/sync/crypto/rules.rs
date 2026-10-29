use super::{entry::*, fold::*, links::wrap_set_digest, parse::ParsedBody, signatures::*};
use serde_json::json;
fn check_wrap_set(
    wraps: &[WrapEntry],
    digest: &str,
    epoch: u64,
    recipients: Vec<String>,
) -> Option<&'static str> {
    let Some(actual) = wrap_set_digest(wraps) else {
        return Some("bad_wrap_set");
    };
    if actual != digest {
        return Some("wraps_digest_mismatch");
    }
    let mut got: Vec<_> = wraps.iter().map(|w| (w.epoch, w.id.clone())).collect();
    got.sort();
    let mut want: Vec<_> = recipients.into_iter().map(|id| (epoch, id)).collect();
    want.sort();
    if got == want {
        None
    } else {
        Some("bad_wrap_set")
    }
}
fn countersign_ok(
    account: &str,
    root: &str,
    predecessor: &str,
    wraps: &str,
    key: &str,
    sig: &str,
) -> bool {
    verify_tagged(
        "omo-sync/v1/recovery-countersign",
        &json!({"t":"omo-sync/v1/recovery-countersign","account":account,
        "custody_root":root,"predecessor":predecessor,"wraps_digest":wraps}),
        key,
        sig,
    )
}
/// Every `signing_public_key` value anywhere in `value`; a non-string one counts as "" so it refuses.
fn signing_keys_in(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Array(rows) => rows.iter().for_each(|v| signing_keys_in(v, out)),
        serde_json::Value::Object(map) => {
            for (key, field) in map {
                if key == "signing_public_key" {
                    out.push(field.as_str().unwrap_or("").to_owned());
                } else {
                    signing_keys_in(field, out);
                }
            }
        }
        _ => {}
    }
}
/// The signing keys a record brings into the authority; each must be a strong key.
fn admitted_keys(entry: &KeyringEntry, body: &ParsedBody) -> Vec<String> {
    match body {
        ParsedBody::Genesis {
            first_device,
            recovery_key,
            ..
        } => vec![
            first_device.signing_public_key.clone(),
            recovery_key.signing_public_key.clone(),
        ],
        ParsedBody::RecoveryKey { key, .. } => vec![key.signing_public_key.clone()],
        ParsedBody::Approval { principal, .. } | ParsedBody::RecoveryJoin { principal, .. } => {
            vec![principal.signing_public_key.clone()]
        }
        ParsedBody::ForkResolution { .. } => {
            let mut out = Vec::new();
            signing_keys_in(&entry.body, &mut out);
            out
        }
        ParsedBody::Rotation { .. }
        | ParsedBody::RecoveryKeyRemoved { .. }
        | ParsedBody::RecoveryJoinVetoed { .. } => Vec::new(),
    }
}
pub fn check_record_rules(
    entry: &KeyringEntry,
    body: &ParsedBody,
    before: Option<&Authority>,
    history: &[FoldedRecord],
    predecessor: &str,
) -> Option<&'static str> {
    let account = &entry.fields.account;
    if !admitted_keys(entry, body)
        .iter()
        .all(|k| is_strong_key_text(k))
    {
        return Some("weak_key");
    }
    if let ParsedBody::Genesis {
        first_device,
        recovery_key,
        bootstrap,
        custody_root,
        countersign,
        wraps_digest,
    } = body
    {
        if before.is_some() || entry.seq != 1 {
            return Some("bad_genesis");
        }
        if device_id_of(&first_device.signing_public_key).as_ref() != Some(&first_device.id) {
            return Some("bad_device_id");
        }
        if tagged_hash("omo-sync/v1/custody-root", bootstrap).as_ref() != Some(custody_root) {
            return Some("bad_custody_root");
        }
        if !countersign_ok(
            account,
            custody_root,
            "",
            wraps_digest,
            &recovery_key.signing_public_key,
            countersign,
        ) {
            return Some("bad_countersign");
        }
        return check_wrap_set(
            &entry.wraps,
            wraps_digest,
            1,
            vec![
                format!("device:{}", first_device.id),
                format!("recovery:{}", recovery_key.credential_id),
            ],
        );
    }
    let Some(before) = before else {
        return Some("bad_genesis");
    };
    match body {
        ParsedBody::Genesis { .. } => Some("bad_genesis"),
        ParsedBody::RecoveryKey {
            key,
            countersign,
            wraps_digest,
        } => {
            if before.enrolled_ever.contains(&key.credential_id) {
                return Some("credential_reused");
            }
            if !countersign_ok(
                account,
                &before.custody_root,
                predecessor,
                wraps_digest,
                &key.signing_public_key,
                countersign,
            ) {
                return Some("bad_countersign");
            }
            check_wrap_set(
                &entry.wraps,
                wraps_digest,
                before.epoch,
                vec![format!("recovery:{}", key.credential_id)],
            )
        }
        ParsedBody::RecoveryKeyRemoved { credential_id } => {
            if !before.registry.contains_key(credential_id) {
                return Some("unknown_credential");
            }
            unexpected(&entry.wraps)
        }
        ParsedBody::Approval {
            principal,
            wraps_digest,
            commitment,
            joining_nonce,
        } => {
            if before.members.contains_key(&principal.id) {
                return Some("already_member");
            }
            if device_id_of(&principal.signing_public_key).as_ref() != Some(&principal.id) {
                return Some("bad_device_id");
            }
            if tagged_hash(
                "omo-sync/v1/approval-commitment",
                &json!({"account":account,"sealing_public_key":principal.sealing_public_key,
                "signing_public_key":principal.signing_public_key,"nonce":joining_nonce}),
            )
            .as_ref()
                != Some(commitment)
            {
                return Some("bad_commitment");
            }
            check_wrap_set(
                &entry.wraps,
                wraps_digest,
                before.epoch,
                vec![format!("device:{}", principal.id)],
            )
        }
        ParsedBody::Rotation {
            members,
            wraps_digest,
            ..
        } => {
            if members.iter().any(|p| {
                before
                    .members
                    .get(&p.id)
                    .is_none_or(|m| m.signing_public_key != p.signing_public_key)
            }) {
                return Some("member_not_in_fold");
            }
            let mut recipients: Vec<_> =
                members.iter().map(|p| format!("device:{}", p.id)).collect();
            recipients.extend(before.registry.keys().map(|id| format!("recovery:{id}")));
            check_wrap_set(
                &entry.wraps,
                wraps_digest,
                before.epoch.saturating_add(1),
                recipients,
            )
        }
        ParsedBody::RecoveryJoin {
            keyring_head,
            principal,
            credential_id,
        } => {
            if !before.registry.contains_key(credential_id) {
                return Some("unknown_credential");
            }
            if keyring_head != predecessor {
                return Some("bad_keyring_head");
            }
            if device_id_of(&principal.signing_public_key).as_ref() != Some(&principal.id) {
                return Some("bad_device_id");
            }
            unexpected(&entry.wraps)
        }
        ParsedBody::RecoveryJoinVetoed { join_digest } => {
            let Some(join) = history.iter().find(|r| r.digest == *join_digest) else {
                return Some("unknown_join");
            };
            if !matches!(join.body, ParsedBody::RecoveryJoin { .. }) || join.before.is_none() {
                return Some("unknown_join");
            }
            let member = match &entry.author {
                SyncAuthor::Device { id, .. } => join
                    .before
                    .as_ref()
                    .is_some_and(|a| a.members.contains_key(id)),
                SyncAuthor::Recovery { .. } => false,
            };
            if !member {
                return Some("veto_signer_not_member");
            }
            unexpected(&entry.wraps)
        }
        ParsedBody::ForkResolution { .. } => unexpected(&entry.wraps),
    }
}
fn unexpected(wraps: &[WrapEntry]) -> Option<&'static str> {
    if wraps.is_empty() {
        None
    } else {
        Some("unexpected_wraps")
    }
}
