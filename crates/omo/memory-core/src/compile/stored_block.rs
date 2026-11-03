use serde_json::{Value, json};

use super::compile::MemoryExpiry;

/// A persisted block with the expiry slot it was compiled in and every instant its revision declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBlock {
    pub block: String,
    pub slot: String,
    pub expiries: Vec<MemoryExpiry>,
}

pub fn encode_stored_block(stored: &StoredBlock) -> String {
    let expiries: Vec<Value> = stored
        .expiries
        .iter()
        .map(|expiry| json!([expiry.path, expiry.expires_at_ms]))
        .collect();
    json!({"block": stored.block, "slot": stored.slot, "expiries": expiries}).to_string()
}

fn expiry_pair(pair: &Value) -> Option<MemoryExpiry> {
    // Like the TS destructure `[path, expiresAtMs]`, any extra elements are ignored.
    let row = pair.as_array()?;
    Some(MemoryExpiry {
        path: row.first()?.as_str()?.to_string(),
        expires_at_ms: whole_number(row.get(1)?)?,
    })
}

fn whole_number(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        value
            .as_f64()
            .filter(|n| n.is_finite() && n.fract() == 0.0)
            .map(|n| n as i64)
    })
}

/// A stored block, or `None` when the entry is not readable as one (it is then recompiled and rewritten).
pub fn decode_stored_block(text: &str) -> Option<StoredBlock> {
    let parsed: Value = serde_json::from_str(text).ok()?;
    let block = parsed.get("block")?.as_str()?.to_string();
    let slot = parsed.get("slot")?.as_str()?.to_string();
    let pairs = parsed.get("expiries")?.as_array()?;
    let expiries: Vec<MemoryExpiry> = pairs.iter().filter_map(expiry_pair).collect();
    (expiries.len() == pairs.len()).then_some(StoredBlock {
        block,
        slot,
        expiries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored() -> StoredBlock {
        StoredBlock {
            block: "b".into(),
            slot: "system/a.md@5".into(),
            expiries: vec![MemoryExpiry {
                path: "system/a.md".into(),
                expires_at_ms: 5,
            }],
        }
    }

    #[test]
    fn round_trips_a_stored_block() {
        assert_eq!(
            decode_stored_block(&encode_stored_block(&stored())),
            Some(stored())
        );
    }

    #[test]
    fn malformed_entries_read_as_absent() {
        for text in [
            "not json",
            r#"{"block":1,"slot":"","expiries":[]}"#,
            r#"{"block":"b","slot":"","expiries":"x"}"#,
            r#"{"block":"b","slot":"","expiries":[["p"]]}"#,
            r#"{"block":"b","slot":"","expiries":[["p","1"]]}"#,
        ] {
            assert_eq!(decode_stored_block(text), None, "{text}");
        }
    }
}
