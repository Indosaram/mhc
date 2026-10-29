use super::*;
use serde_json::Value;

const CORPUS: &str = include_str!("strict-ed25519-corpus.json");

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .filter_map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
        .collect()
}
fn field(row: &Value, name: &str) -> Vec<u8> {
    hex(row[name].as_str().unwrap_or(""))
}

#[test]
fn ed25519_corpus_verdicts() {
    let corpus: Value = serde_json::from_str(CORPUS).expect("corpus parses");
    let rows = corpus["ed25519"].as_array().expect("ed25519 rows");
    assert!(!rows.is_empty());
    for row in rows {
        let (key, sig, msg) = (field(row, "key"), field(row, "sig"), field(row, "msg"));
        let id = row["name"].as_str().unwrap_or("");
        assert_eq!(
            strict_ed25519_key(&key),
            row["keyValid"] == true,
            "key {id}"
        );
        let verdict = strict_ed25519_verify(&key, &sig, &msg);
        let label = verdict.map_or_else(StrictEd25519Refusal::as_str, |()| "ok");
        assert_eq!(Some(label), row["verdict"].as_str(), "verdict {id}");
        assert_eq!(
            strict_ed25519_precheck(&key, &sig).is_ok(),
            row["verdict"] != "malformed",
            "precheck {id}"
        );
    }
}

#[test]
fn x25519_corpus_verdicts() {
    let corpus: Value = serde_json::from_str(CORPUS).expect("corpus parses");
    for row in corpus["x25519"]["keys"].as_array().expect("keys") {
        assert_eq!(
            strict_x25519_key(&field(row, "key")),
            row["valid"] == true,
            "{}",
            row["name"]
        );
    }
    for row in corpus["x25519"]["sharedSecrets"]
        .as_array()
        .expect("secrets")
    {
        assert_eq!(
            check_shared_secret(&field(row, "bytes")),
            row["valid"] == true,
            "{}",
            row["name"]
        );
    }
}
