use super::super::super::{links::next_link, signatures::*};
use super::fold::fold_authority;
use serde_json::Value;
pub fn branch_refusal(evidence: &Value) -> Option<&'static str> {
    let Some(branches) = evidence["branches"].as_array() else {
        return Some("fork_set_mismatch");
    };
    let Some(common_seq) = evidence["common"]["seq"].as_u64() else {
        return Some("fork_set_mismatch");
    };
    for branch in branches {
        let Some(records) = branch["records"].as_array() else {
            return Some("fork_set_mismatch");
        };
        let Some(last) = records.last() else {
            return Some("fork_set_mismatch");
        };
        let mut expected = common_seq;
        for r in records {
            expected = match expected.checked_add(1) {
                Some(n) => n,
                None => return Some("noncanonical_order"),
            };
            if r["seq"].as_u64() != Some(expected)
                || r["base_seq"].as_u64().and_then(|n| n.checked_add(1)) != Some(expected)
            {
                return Some("noncanonical_order");
            }
        }
        let tip_link = records
            .iter()
            .rev()
            .find(|r| r["kind"] == "keyring")
            .map_or(&evidence["common"]["keyring_link"], |r| &r["keyring_link"]);
        if last["seq"] != branch["tip"]["seq"]
            || last["digest"] != branch["tip"]["digest"]
            || *tip_link != branch["tip"]["keyring_link"]
        {
            return Some("fork_set_mismatch");
        }
    }
    None
}
pub fn verify_fork_evidence(ancestry: &[Value], evidence: &Value) -> bool {
    let Some(common) = ancestry.iter().find(|r| {
        r["seq"] == evidence["common"]["seq"] && r["digest"] == evidence["common"]["digest"]
    }) else {
        return false;
    };
    let Some(branches) = evidence["branches"].as_array() else {
        return false;
    };
    for branch in branches {
        let mut prior = common["link"].as_str().unwrap_or("").to_owned();
        let mut keyring = evidence["common"]["keyring_link"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        let mut prefix = ancestry.to_vec();
        let Some(records) = branch["records"].as_array() else {
            return false;
        };
        for r in records {
            let Some(digest) = r["digest"].as_str() else {
                return false;
            };
            if next_link(&prior, digest).as_deref() != r["link"].as_str() {
                return false;
            }
            if r["kind"] == "batch" {
                if !r["keyring_link"].is_null() || !r["body"].is_null() {
                    return false;
                }
            } else {
                let Ok(bytes) = canonical(&r["body"]) else {
                    return false;
                };
                if r["body"].is_null()
                    || sha256_b64(&bytes) != digest
                    || next_link(&keyring, digest).as_deref() != r["keyring_link"].as_str()
                {
                    return false;
                }
            }
            let a = fold_authority(&prefix);
            let author = &r["author"];
            let key = match author["kind"].as_str() {
                Some("device") => author["id"].as_str().and_then(|id| a.members.get(id)),
                Some("recovery") => author["credential_id"]
                    .as_str()
                    .and_then(|id| a.registry.get(id)),
                _ => None,
            }
            .and_then(|v| v["signing_public_key"].as_str());
            let (Some(key), Some(sig)) = (key, r["record_sig"].as_str()) else {
                return false;
            };
            let mut payload = serde_json::json!({"t":"omo-sync/v1/record","account":r["account"],"base_seq":r["base_seq"],"digest":r["digest"],"epoch":r["epoch"],"link":r["link"]});
            if r["kind"] == "keyring" {
                payload["keyring_link"] = r["keyring_link"].clone();
            }
            if !verify_tagged("omo-sync/v1/record", &payload, key, sig) {
                return false;
            }
            prior = r["link"].as_str().unwrap_or("").into();
            if r["kind"] == "keyring" {
                keyring = r["keyring_link"].as_str().unwrap_or("").into();
            }
            prefix.push(r.clone());
        }
    }
    true
}
