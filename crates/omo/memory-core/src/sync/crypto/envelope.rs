use super::entry::*;
use serde_json::Value;

pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
pub fn exact(value: &Value, keys: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.len() == keys.len() && keys.iter().all(|k| o.contains_key(*k)))
}
pub fn integer(value: &Value, min: u64) -> Option<u64> {
    value
        .as_u64()
        .filter(|n| *n >= min && *n <= MAX_SAFE_INTEGER)
        .or_else(|| {
            value
                .as_f64()
                .filter(|n| n.is_finite() && n.fract() == 0.0)
                .and_then(|n| n.to_string().parse::<u64>().ok())
                .filter(|n| *n >= min && *n <= MAX_SAFE_INTEGER)
        })
}
pub fn text(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}
fn within_depth(value: &Value) -> bool {
    let mut stack = vec![(value, 1)];
    while let Some((node, depth)) = stack.pop() {
        match node {
            Value::Array(a) => {
                if depth > 64 {
                    return false;
                }
                stack.extend(a.iter().map(|v| (v, depth + 1)));
            }
            Value::Object(o) => {
                if depth > 64 {
                    return false;
                }
                stack.extend(o.values().map(|v| (v, depth + 1)));
            }
            _ => {}
        }
    }
    true
}
const COMMON: &[&str] = &[
    "seq",
    "base_seq",
    "kind",
    "account",
    "epoch",
    "author",
    "digest",
    "link",
    "keyring_link",
    "record_sig",
];

/// Closed transport parsing happens before admission can access any fields.
pub fn parse_entry(row: &Value) -> Result<SyncEntry, &'static str> {
    let bad = "bad_envelope";
    if !row.is_object() || !within_depth(row) {
        return Err(bad);
    }
    let batch = row["kind"] == "batch";
    if batch && row.get("body").is_some() {
        return Err("batch_with_body");
    }
    let mut keys = COMMON.to_vec();
    if !batch {
        if row["kind"] != "keyring" {
            return Err(bad);
        }
        keys.extend(["body", "wraps"]);
    }
    if !exact(row, &keys) {
        return Err(bad);
    }
    let seq = integer(&row["seq"], 1).ok_or(bad)?;
    let fields = SyncRecordFields {
        account: text(&row["account"]).ok_or(bad)?.into(),
        base_seq: integer(&row["base_seq"], 0).ok_or(bad)?,
        epoch: integer(&row["epoch"], 1).ok_or(bad)?,
        digest: text(&row["digest"]).ok_or(bad)?.into(),
        link: text(&row["link"]).ok_or(bad)?.into(),
        keyring_link: if batch {
            if !row["keyring_link"].is_null() {
                return Err(bad);
            }
            None
        } else {
            Some(text(&row["keyring_link"]).ok_or(bad)?.into())
        },
    };
    let a = &row["author"];
    let author = match a["kind"].as_str() {
        Some("device")
            if exact(
                a,
                &["kind", "id", "sealing_public_key", "signing_public_key"],
            ) =>
        {
            SyncAuthor::Device {
                id: text(&a["id"]).ok_or(bad)?.into(),
                sealing_public_key: text(&a["sealing_public_key"]).ok_or(bad)?.into(),
                signing_public_key: text(&a["signing_public_key"]).ok_or(bad)?.into(),
            }
        }
        Some("recovery") if exact(a, &["kind", "credential_id"]) => SyncAuthor::Recovery {
            credential_id: text(&a["credential_id"]).ok_or(bad)?.into(),
        },
        _ => return Err(bad),
    };
    let record_sig = text(&row["record_sig"]).ok_or(bad)?.into();
    if batch {
        return Ok(SyncEntry::Batch(BatchEntry {
            seq,
            fields,
            author,
            record_sig,
        }));
    }
    if !row["body"].is_object() {
        return Err(bad);
    }
    let wraps = row["wraps"]
        .as_array()
        .ok_or(bad)?
        .iter()
        .map(|w| {
            if !exact(w, &["epoch", "id", "wrap"]) {
                return Err(bad);
            }
            Ok(WrapEntry {
                epoch: integer(&w["epoch"], 1).ok_or(bad)?,
                id: text(&w["id"]).ok_or(bad)?.into(),
                wrap: text(&w["wrap"]).ok_or(bad)?.into(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SyncEntry::Keyring(KeyringEntry {
        seq,
        fields,
        author,
        record_sig,
        body: row["body"].clone(),
        wraps,
    }))
}

pub fn envelope_refusal(entry: &SyncEntry) -> Option<&'static str> {
    parse_entry(&entry.wire()).err()
}
