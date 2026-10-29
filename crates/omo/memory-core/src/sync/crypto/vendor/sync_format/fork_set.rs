use serde_json::Value;
pub fn fork_set_matches_evidence(set: &Value, branches: &Value) -> bool {
    let (Some(set), Some(branches)) = (set.as_array(), branches.as_array()) else {
        return false;
    };
    set.len() >= 2
        && set.len() == branches.len()
        && set.windows(2).all(|p| {
            let (Some(a), Some(b), Some(ad), Some(bd)) = (
                p[0]["seq"].as_u64(),
                p[1]["seq"].as_u64(),
                p[0]["digest"].as_str(),
                p[1]["digest"].as_str(),
            ) else {
                return false;
            };
            a < b || (a == b && ad.encode_utf16().cmp(bd.encode_utf16()).is_lt())
        })
        && set.iter().zip(branches).all(|(r, b)| {
            r["seq"] == b["tip"]["seq"]
                && r["digest"] == b["tip"]["digest"]
                && r["keyring_link"] == b["tip"]["keyring_link"]
        })
}
