use super::{
    canonical::*, fork_evidence::branch_refusal, fork_set::fork_set_matches_evidence,
    fork_state::recompute_fork_state,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default)]
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
fn refusal(entry: &Value, state: &KeyringFold, depth: usize) -> Option<ReceiverRefusal> {
    let done = &state.done;
    let before = &state.before_join;
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
                .is_none_or(|a| ordered_by(a, "id")) =>
        {
            return Some("noncanonical_order");
        }
        "recovery-join" => {
            if kind != "recovery" || entry["author"]["credential_id"] != body["credential_id"] {
                return Some("recovery_join_author");
            }
            if !state
                .authority
                .registry
                .contains_key(s(&entry["author"], "credential_id"))
            {
                return Some("recovery_join_unenrolled");
            }
        }
        "recovery-join-vetoed" => {
            let Some(join) = state.with_digest(s(body, "join_digest")) else {
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
            if state.vetoed.contains(s(body, "join_digest")) {
                return Some("join_already_vetoed");
            }
        }
        "fork-resolution" => {
            let (Some(evidence), Some(stated)) = (body.get("evidence"), body.get("state")) else {
                return Some("fork_state_mismatch");
            };
            if !fork_set_matches_evidence(&body["fork_set"], &evidence["branches"]) {
                return Some("fork_set_mismatch");
            }
            if let Some(r) = branch_refusal(evidence) {
                return Some(r);
            }
            if !stated["members"]
                .as_array()
                .is_some_and(|a| ordered_by(a, "id"))
                || !stated["recovery_keys"]
                    .as_array()
                    .is_some_and(|a| ordered_by(a, "credential_id"))
            {
                return Some("noncanonical_order");
            }
            let common = &evidence["common"];
            let Some(common) = state
                .by_seq
                .get(&common["seq"].as_u64().unwrap_or(0))
                .and_then(|at| done.get(*at))
                .filter(|e| e["digest"] == common["digest"])
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
            if recompute_fork_state(&ancestry, evidence, depth + 1).as_ref() != Some(stated) {
                return Some("fork_state_mismatch");
            }
            if kind != "device"
                || !stated["members"]
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
pub use super::authority::FoldState as KeyringFold;
impl KeyringFold {
    fn with_digest(&self, digest: &str) -> Option<&Value> {
        self.by_digest.get(digest).and_then(|at| self.done.get(*at))
    }
    pub fn through(&self, seq: u64) -> &[Value] {
        self.by_seq.get(&seq).map_or(&[], |at| &self.done[..=*at])
    }
    pub fn refusal(&self, entry: &Value) -> Option<ReceiverRefusal> {
        self.refusal_at(entry, 0)
    }
    fn refusal_at(&self, entry: &Value, depth: usize) -> Option<ReceiverRefusal> {
        if entry["seq"].as_u64().unwrap_or(0) <= self.last_seq {
            return Some("seq_not_increasing");
        }
        refusal(entry, self, depth)
    }
    pub fn pass(&mut self, entry: &Value) {
        self.last_seq = self.last_seq.max(entry["seq"].as_u64().unwrap_or(0));
    }
    pub fn accept(&mut self, entry: &Value) {
        self.pass(entry);
        let seq = entry["seq"].as_u64().unwrap_or(0);
        if entry["kind"] == "keyring" && entry["body"]["kind"] == "recovery-join" {
            self.before_join.insert(seq, self.authority.clone());
        }
        let body = &entry["body"];
        if entry["kind"] == "keyring" {
            match s(body, "kind") {
                "genesis" => {
                    let boot = &body["bootstrap"];
                    self.authority.members = [(
                        s(&boot["first_device"], "id").into(),
                        boot["first_device"].clone(),
                    )]
                    .into();
                    self.authority.registry = [(
                        s(&boot["recovery_key"], "credential_id").into(),
                        boot["recovery_key"].clone(),
                    )]
                    .into();
                }
                "approval" => {
                    self.authority.members.insert(
                        s(&body["principal"], "id").into(),
                        body["principal"].clone(),
                    );
                }
                "recovery-key" => {
                    let key = serde_json::json!({"credential_id":body["credential_id"],"rp_id":body["rp_id"],"enrollment_nonce":body["enrollment_nonce"],
                    "sealing_public_key":body["sealing_public_key"],"signing_public_key":body["signing_public_key"],"survivable":body["survivable"]});
                    self.authority
                        .registry
                        .insert(s(body, "credential_id").into(), key);
                }
                "recovery-key-removed" => {
                    self.authority.registry.remove(s(body, "credential_id"));
                }
                "recovery-join" => {
                    self.authority.members = [(
                        s(&body["principal"], "id").into(),
                        body["principal"].clone(),
                    )]
                    .into();
                    self.authority.registry = self
                        .authority
                        .registry
                        .get(s(body, "credential_id"))
                        .map(|v| [(s(v, "credential_id").into(), v.clone())].into())
                        .unwrap_or_default();
                }
                "recovery-join-vetoed" => {
                    if let Some(join) = self.with_digest(s(body, "join_digest")).cloned()
                        && let Some(prior) = self
                            .before_join
                            .get(&join["seq"].as_u64().unwrap_or(0))
                            .cloned()
                    {
                        self.authority.members = prior.members.clone();
                        self.authority.registry = prior.registry.clone();
                        self.authority
                            .registry
                            .remove(s(&join["body"], "credential_id"));
                        for later in &self.done {
                            if later["seq"].as_u64() > join["seq"].as_u64() {
                                self.authority.cancelled.insert(s(later, "digest").into());
                            }
                        }
                    }
                }
                "rotation" => self.authority.members = map(&body["members"], "id"),
                "fork-resolution" => {
                    self.authority.members = map(&body["state"]["members"], "id");
                    self.authority.registry = map(&body["state"]["recovery_keys"], "credential_id");
                }
                _ => {}
            }
        }

        if entry["body"]["kind"] == "recovery-join-vetoed" {
            self.vetoed.insert(s(&entry["body"], "join_digest").into());
        }
        let at = self.done.len();
        self.by_seq.insert(seq, at);
        self.by_digest
            .entry(s(entry, "digest").into())
            .or_insert(at);
        self.done.push(entry.clone());
    }
}
pub(super) fn fold_at(
    entries: &[Value],
    depth: usize,
) -> (AuthorityFold, Option<(u64, ReceiverRefusal)>) {
    let mut state = KeyringFold::default();
    let mut first = None;
    for entry in entries {
        if let Some(reason) = state.refusal_at(entry, depth) {
            first.get_or_insert((entry["seq"].as_u64().unwrap_or(0), reason));
            state.pass(entry);
        } else {
            state.accept(entry);
        }
    }
    (state.authority, first)
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
