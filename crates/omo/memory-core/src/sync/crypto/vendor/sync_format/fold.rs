use super::{
    canonical::*, fork_evidence::branch_refusal, fork_set::fork_set_matches_evidence,
    fork_state::recompute_fork_state,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Default)]
pub struct AuthorityFold {
    pub members: BTreeMap<String, Value>,
    pub registry: BTreeMap<String, Value>,
    pub cancelled: BTreeSet<String>,
}
pub type ReceiverRefusal = &'static str;
pub type ReceivedChain = Result<AuthorityFold, (u64, ReceiverRefusal)>;
fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
fn map(rows: &Value, key: &str) -> BTreeMap<String, Value> {
    rows.as_array()
        .into_iter()
        .flatten()
        .map(|v| (s(v, key).into(), v.clone()))
        .collect()
}
fn refusal(
    entry: &Value,
    done: &[Value],
    before: &BTreeMap<u64, AuthorityFold>,
    depth: usize,
) -> Option<ReceiverRefusal> {
    let kind = s(&entry["author"], "kind");
    if kind != "device" && kind != "recovery" {
        return Some("unknown_author_kind");
    }
    if entry["kind"] == "batch" && entry.get("body").is_some_and(|b| !b.is_null()) {
        return Some("batch_with_body");
    }
    if let Some(wraps) = entry.get("wraps")
        && !wraps.as_array().is_some_and(|a| wraps_ordered(a))
    {
        return Some("noncanonical_order");
    }
    let body = &entry["body"];
    if kind == "recovery" && (entry["kind"] != "keyring" || body["kind"] != "recovery-join") {
        return Some("recovery_author_not_join");
    }
    if entry["kind"] != "keyring" || body.is_null() {
        return None;
    }
    match s(body, "kind") {
        "rotation"
            if !body["members"]
                .as_array()
                .is_none_or(|a| ordered_by(a, "id"))
        => {
            return Some("noncanonical_order");
        }
        "recovery-join" => {
            if kind != "recovery" || entry["author"]["credential_id"] != body["credential_id"] {
                return Some("recovery_join_author");
            }
            if !before
                .get(&entry["seq"].as_u64().unwrap_or(0))
                .is_some_and(|a| {
                    a.registry
                        .contains_key(s(&entry["author"], "credential_id"))
                })
            {
                return Some("recovery_join_unenrolled");
            }
        }
        "recovery-join-vetoed" => {
            let Some(join) = done.iter().find(|e| e["digest"] == body["join_digest"]) else {
                return Some("veto_not_a_join");
            };
            if join["body"]["kind"] != "recovery-join" {
                return Some("veto_not_a_join");
            }
            if kind != "device"
                || !before
                    .get(&join["seq"].as_u64().unwrap_or(0))
                    .is_some_and(|a| a.members.contains_key(s(&entry["author"], "id")))
            {
                return Some("veto_signer_not_member");
            }
            if done.iter().any(|e| {
                e["body"]["kind"] == "recovery-join-vetoed"
                    && e["body"]["join_digest"] == body["join_digest"]
            }) {
                return Some("join_already_vetoed");
            }
        }
        "fork-resolution" => {
            let (Some(evidence), Some(state)) = (body.get("evidence"), body.get("state")) else {
                return Some("fork_state_mismatch");
            };
            if !fork_set_matches_evidence(&body["fork_set"], &evidence["branches"]) {
                return Some("fork_set_mismatch");
            }
            if let Some(r) = branch_refusal(evidence) {
                return Some(r);
            }
            if !state["members"]
                .as_array()
                .is_some_and(|a| ordered_by(a, "id"))
                || !state["recovery_keys"]
                    .as_array()
                    .is_some_and(|a| ordered_by(a, "credential_id"))
            {
                return Some("noncanonical_order");
            }
            let common = &evidence["common"];
            let Some(common) = done
                .iter()
                .find(|e| e["seq"] == common["seq"] && e["digest"] == common["digest"])
            else {
                return Some("fork_common_unknown");
            };
            let ancestry: Vec<_> = done
                .iter()
                .filter(|e| e["seq"].as_u64() <= common["seq"].as_u64())
                .cloned()
                .collect();
            let Some(branches) = evidence["branches"].as_array() else {
                return Some("fork_set_mismatch");
            };
            for branch in branches {
                let mut list = ancestry.clone();
                let Some(records) = branch["records"].as_array() else {
                    return Some("fork_evidence_invalid");
                };
                list.extend(records.iter().cloned());
                if depth >= 64 || fold_at(&list, depth + 1).1.is_some() {
                    return Some("fork_evidence_invalid");
                }
            }
            if recompute_fork_state(&ancestry, evidence, depth + 1).as_ref() != Some(state) {
                return Some("fork_state_mismatch");
            }
            if kind != "device"
                || !state["members"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|m| m["id"] == entry["author"]["id"]))
            {
                return Some("fork_signer_not_member");
            }
        }
        _ => {}
    }
    None
}
pub(super) fn fold_at(
    entries: &[Value],
    depth: usize,
) -> (AuthorityFold, Option<(u64, ReceiverRefusal)>) {
    let mut authority = AuthorityFold::default();
    let mut before = BTreeMap::new();
    let mut done = Vec::new();
    let mut first = None;
    let mut last = 0;
    for entry in entries {
        let seq = entry["seq"].as_u64().unwrap_or(0);
        let replayed = seq <= last;
        last = last.max(seq);
        if !replayed {
            before.insert(seq, authority.clone());
        }
        let reason = if replayed {
            Some("seq_not_increasing")
        } else {
            refusal(entry, &done, &before, depth)
        };
        if let Some(r) = reason {
            if first.is_none() {
                first = Some((seq, r));
            }
            continue;
        }
        let body = &entry["body"];
        if entry["kind"] == "keyring" {
            match s(body, "kind") {
                "genesis" => {
                    let boot = &body["bootstrap"];
                    authority.members = [(
                        s(&boot["first_device"], "id").into(),
                        boot["first_device"].clone(),
                    )]
                    .into();
                    authority.registry = [(
                        s(&boot["recovery_key"], "credential_id").into(),
                        boot["recovery_key"].clone(),
                    )]
                    .into();
                }
                "approval" => {
                    authority.members.insert(
                        s(&body["principal"], "id").into(),
                        body["principal"].clone(),
                    );
                }
                "recovery-key" => {
                    let key = serde_json::json!({"credential_id":body["credential_id"],"rp_id":body["rp_id"],"enrollment_nonce":body["enrollment_nonce"],
                    "sealing_public_key":body["sealing_public_key"],"signing_public_key":body["signing_public_key"],"survivable":body["survivable"]});
                    authority
                        .registry
                        .insert(s(body, "credential_id").into(), key);
                }
                "recovery-key-removed" => {
                    authority.registry.remove(s(body, "credential_id"));
                }
                "recovery-join" => {
                    authority.members = [(
                        s(&body["principal"], "id").into(),
                        body["principal"].clone(),
                    )]
                    .into();
                    authority.registry = authority
                        .registry
                        .get(s(body, "credential_id"))
                        .map(|v| [(s(v, "credential_id").into(), v.clone())].into())
                        .unwrap_or_default();
                }
                "recovery-join-vetoed" => {
                    if let Some(join) = done
                        .iter()
                        .find(|e: &&Value| e["digest"] == body["join_digest"])
                        && let Some(prior) = before.get(&join["seq"].as_u64().unwrap_or(0))
                    {
                        authority.members = prior.members.clone();
                        authority.registry = prior.registry.clone();
                        authority.registry.remove(s(&join["body"], "credential_id"));
                        for later in &done {
                            if later["seq"].as_u64() > join["seq"].as_u64() {
                                authority.cancelled.insert(s(later, "digest").into());
                            }
                        }
                    }
                }
                "rotation" => authority.members = map(&body["members"], "id"),
                "fork-resolution" => {
                    authority.members = map(&body["state"]["members"], "id");
                    authority.registry = map(&body["state"]["recovery_keys"], "credential_id");
                }
                _ => {}
            }
        }
        done.push(entry.clone());
    }
    (authority, first)
}
pub fn fold_authority(entries: &[Value]) -> AuthorityFold {
    fold_at(entries, 0).0
}
pub fn fold_refusal(entries: &[Value]) -> ReceivedChain {
    let (fold, refused) = fold_at(entries, 0);
    match refused {
        Some(r) => Err(r),
        None => Ok(fold),
    }
}
