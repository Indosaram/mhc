use super::{RecoveryError, ports::*};
use crate::sync::crypto::{
    anchor::ChainAnchor,
    chain::*,
    entry::VerifiedWrap,
    fold::Authority,
    parse::{ParsedBody, parse_body},
    rejection::RejectionCounter,
};
use serde_json::Value;
use std::collections::BTreeMap;
#[derive(Debug, PartialEq, Eq)]
pub enum ServedRefusal {
    Unavailable,
    Unverified,
    AnchorMismatch,
}
pub async fn load_served_chain(
    transport: &dyn RecoveryTransport,
    account: &str,
    rejections: &mut RejectionCounter,
    anchor: ChainAnchor,
) -> Result<Result<ChainState, ServedRefusal>, RecoveryError> {
    let rows = transport.list_entries().await?;
    let keys = transport.list_keys().await?;
    let (Some(rows), Some(keys)) = (rows.as_array(), keys.as_array()) else {
        return Ok(Err(ServedRefusal::Unavailable));
    };
    if !rows.iter().chain(keys).all(Value::is_object) {
        return Ok(Err(ServedRefusal::Unverified));
    }
    let mut by_seq = BTreeMap::new();
    for key in keys {
        by_seq.insert(key["seq"].as_u64(), key);
    }
    let mut entries = Vec::new();
    for row in rows {
        if row["kind"] == "batch" {
            let mut batch = row.clone();
            batch["keyring_link"] = Value::Null;
            entries.push(batch);
            continue;
        }
        let Some(key) = by_seq.get(&row["seq"].as_u64()) else {
            return Ok(Err(ServedRefusal::Unverified));
        };
        if key["digest"] != row["digest"] || key["keyring_link"] != row["keyring_link"] {
            return Ok(Err(ServedRefusal::Unverified));
        }
        entries.push((*key).clone());
    }
    Ok(
        match append_entries(&empty_chain(account.into(), anchor), &entries, rejections) {
            AppendOutcome::Accepted { state, .. } if state.authority().is_some() => Ok(state),
            AppendOutcome::Rejected {
                reason: "anchor_mismatch",
                ..
            } => Err(ServedRefusal::AnchorMismatch),
            _ => Err(ServedRefusal::Unverified),
        },
    )
}
pub fn expected_history(authority: &Authority, epoch: u64) -> BTreeMap<u64, String> {
    authority
        .epoch_origins
        .iter()
        .filter(|(e, origin)| **e < epoch && !authority.cancelled.contains(*origin))
        .map(|(e, origin)| (*e, origin.clone()))
        .collect()
}
pub fn committed_wrap(state: &ChainState, epoch: u64, id: &str) -> Option<VerifiedWrap> {
    for entry in state.received().iter().rev() {
        if state
            .authority()
            .is_some_and(|a| a.cancelled.contains(&entry.entry.fields.digest))
        {
            continue;
        }
        if let Some(wrap) = entry
            .wraps
            .iter()
            .find(|wrap| wrap.entry().epoch == epoch && wrap.entry().id == id)
        {
            return Some(wrap.clone());
        }
    }
    None
}
pub fn recovery_candidates(state: &ChainState, served: &[Value]) -> Vec<RecoveryCandidate> {
    let Some(authority) = state.authority() else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for key in authority.registry.values() {
        let enrolling = state
            .received()
            .iter()
            .enumerate()
            .find_map(|(index, entry)| {
                let parsed = parse_body(&entry.entry.body)?;
                let (enrolled, countersign, wraps_digest) = match parsed {
                    ParsedBody::Genesis {
                        recovery_key,
                        countersign,
                        wraps_digest,
                        ..
                    } => (recovery_key, countersign, wraps_digest),
                    ParsedBody::RecoveryKey {
                        key,
                        countersign,
                        wraps_digest,
                    } => (key, countersign, wraps_digest),
                    _ => return None,
                };
                if enrolled.credential_id != key.credential_id {
                    return None;
                }
                let predecessor = index
                    .checked_sub(1)
                    .and_then(|i| state.received().get(i))
                    .and_then(|e| e.entry.fields.keyring_link.clone())
                    .unwrap_or_default();
                Some((entry, predecessor, countersign, wraps_digest))
            });
        let (Some((enrolled, predecessor, signature, wraps_digest)), Some(committed)) = (
            enrolling,
            committed_wrap(
                state,
                authority.epoch,
                &format!("recovery:{}", key.credential_id),
            ),
        ) else {
            continue;
        };
        let anchored = served
            .iter()
            .filter_map(|row| serde_json::from_value::<RecoveryWrapRow>(row.clone()).ok())
            .any(|row| {
                row.entry == *committed.entry()
                    && state.received().iter().any(|e| {
                        e.entry.seq == row.record.seq
                            && e.entry.fields.digest == row.record.digest
                            && e.entry.fields.keyring_link.as_deref()
                                == Some(&row.record.keyring_link)
                            && e.wraps.iter().any(|w| w.entry() == committed.entry())
                    })
            });
        if !anchored {
            continue;
        }
        candidates.push(RecoveryCandidate {
            key: key.clone(),
            countersign: Countersign {
                custody_root: authority.custody_root.clone(),
                predecessor,
                wraps_digest,
                signature,
            },
            wrap: committed.entry().clone(),
            expected_history: expected_history(authority, authority.epoch),
            enrollment: Enrollment {
                wraps: enrolled.wraps.iter().map(|w| w.entry().clone()).collect(),
                expected_history: expected_history(authority, enrolled.entry.fields.epoch),
            },
        });
    }
    candidates
}
