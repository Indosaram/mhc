use serde_json::Value;
pub fn ordered_by(rows: &[Value], key: &str) -> bool {
    rows.windows(2).all(|p| {
        p[0][key]
            .as_str()
            .zip(p[1][key].as_str())
            .is_some_and(|(a, b)| a.encode_utf16().cmp(b.encode_utf16()).is_lt())
    })
}
pub fn wraps_ordered(rows: &[Value]) -> bool {
    rows.windows(2).all(|p| {
        let (Some(a), Some(b), Some(ai), Some(bi)) = (
            p[0]["epoch"].as_u64(),
            p[1]["epoch"].as_u64(),
            p[0]["id"].as_str(),
            p[1]["id"].as_str(),
        ) else {
            return false;
        };
        a < b || (a == b && ai.encode_utf16().cmp(bi.encode_utf16()).is_lt())
    })
}
