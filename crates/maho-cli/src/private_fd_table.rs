//! Port of `packages/coding-agent/src/private-fd-table.ts` (senpi#4749).

use std::sync::LazyLock;

use regex::Regex;

pub const PRIVATE_FDS_TABLE_ENV: &str = "SENPI_PRIVATE_FDS";
/// Forwarded in place of a table that was present but yielded no names, so every hop reads the same state.
pub const UNPARSEABLE_TABLE: &str = "unparseable";
const MAX_TABLE_CHARS: usize = 4096;

static NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9-]{0,63}$").expect("name regex"));
static QUOTED_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""([a-z][a-z0-9-]{0,63})"\s*:"#).expect("quoted name regex"));

#[derive(Clone, Debug, PartialEq)]
pub enum DecodedPrivateFdTable {
    Parsed {
        entries: Vec<(String, serde_json::Value)>,
        values: Vec<serde_json::Value>,
    },
    Unparsed {
        names: Vec<String>,
    },
}

pub fn decode_private_fd_table(encoded: &str) -> DecodedPrivateFdTable {
    let parsed_json: Option<serde_json::Value> = if encoded.chars().count() > MAX_TABLE_CHARS {
        None
    } else {
        serde_json::from_str(encoded).ok()
    };
    let Some(serde_json::Value::Object(map)) = parsed_json else {
        let prefix: String = encoded.chars().take(MAX_TABLE_CHARS).collect();
        let names = QUOTED_NAME
            .captures_iter(&prefix)
            .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_owned()))
            .collect();
        return DecodedPrivateFdTable::Unparsed { names };
    };
    let mut entries = Vec::new();
    let mut values = Vec::with_capacity(map.len());
    for (name, value) in map {
        if NAME.is_match(&name) {
            entries.push((name, value.clone()));
        }
        values.push(value);
    }
    DecodedPrivateFdTable::Parsed { entries, values }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_valid_and_unparseable_tables() {
        let ok = decode_private_fd_table(r#"{"channel-a": 5, "BAD": 6}"#);
        match ok {
            DecodedPrivateFdTable::Parsed { entries, values } => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].0, "channel-a");
                assert_eq!(values.len(), 2);
            }
            _ => panic!("expected parsed"),
        }

        let bad = decode_private_fd_table(r#"{"chan-x": 5, invalid"#);
        assert_eq!(
            bad,
            DecodedPrivateFdTable::Unparsed {
                names: vec!["chan-x".to_owned()]
            }
        );
    }
}
