use super::{
    fold::{ReceivedChain, fold_refusal},
    fork_evidence::verify_fork_evidence,
};
use serde_json::Value;
pub fn receive_keyring_chain(entries: &[Value]) -> ReceivedChain {
    let result = fold_refusal(entries);
    let refused_at = result.as_ref().err().map_or(u64::MAX, |(seq, _)| *seq);
    for entry in entries {
        let seq = entry["seq"].as_u64().unwrap_or(0);
        if seq >= refused_at {
            break;
        }
        if entry["kind"] != "keyring" || entry["body"]["kind"] != "fork-resolution" {
            continue;
        }
        if let Some(evidence) = entry["body"].get("evidence") {
            let ancestry: Vec<_> = entries
                .iter()
                .filter(|r| r["seq"].as_u64() <= evidence["common"]["seq"].as_u64())
                .cloned()
                .collect();
            if !verify_fork_evidence(&ancestry, evidence) {
                return Err((seq, "fork_evidence_invalid"));
            }
        }
    }
    result
}
