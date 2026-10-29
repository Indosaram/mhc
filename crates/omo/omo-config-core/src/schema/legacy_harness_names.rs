use serde_json::{Map, Value};

pub use crate::schema::harness::{
    OMO_CONFIG_LEGACY_HARNESS_ALIASES, canonical_harness_name, harness_block_key,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyHarnessRename {
    pub canonical: String,
    pub dropped: bool,
    pub legacy: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalizeLegacyHarnessBlocksResult {
    pub document: Map<String, Value>,
    pub renames: Vec<LegacyHarnessRename>,
}

fn legacy_harness_of_block_key(key: &str) -> Option<&str> {
    let harness = key.strip_prefix('[')?.strip_suffix(']')?;
    OMO_CONFIG_LEGACY_HARNESS_ALIASES
        .iter()
        .find(|(legacy, _)| *legacy == harness)
        .map(|(legacy, _)| *legacy)
}

fn join_path(path: &[String], segment: &str) -> String {
    let mut next = path.to_vec();
    next.push(segment.to_string());
    next.join(".")
}

// A harness block is legal only at the root and inside a `profiles.<name>` object, so the walk is
// targeted rather than recursive: a `[senpi]` string sitting anywhere else is user data, not a key
// this rename owns.
fn canonicalize_blocks_in(
    container: &Map<String, Value>,
    path: &[String],
    renames: &mut Vec<LegacyHarnessRename>,
) -> Map<String, Value> {
    let mut result = Map::new();
    for (key, value) in container {
        let Some(legacy_harness) = legacy_harness_of_block_key(key) else {
            result.insert(key.clone(), value.clone());
            continue;
        };
        let canonical = harness_block_key(&canonical_harness_name(legacy_harness));
        let dropped = container.contains_key(&canonical);
        renames.push(LegacyHarnessRename {
            canonical: canonical.clone(),
            dropped,
            legacy: key.clone(),
            path: join_path(path, key),
        });
        if !dropped {
            result.insert(canonical, value.clone());
        }
    }
    result
}

pub fn canonicalize_legacy_harness_blocks(document: &Value) -> CanonicalizeLegacyHarnessBlocksResult {
    let Value::Object(root) = document else {
        return CanonicalizeLegacyHarnessBlocksResult {
            document: Map::new(),
            renames: Vec::new(),
        };
    };

    let mut renames = Vec::new();
    let mut canonicalized = canonicalize_blocks_in(root, &[], &mut renames);
    if let Some(Value::Object(profiles)) = canonicalized.get("profiles").cloned() {
        let mut canonical_profiles = Map::new();
        for (name, profile) in &profiles {
            match profile {
                Value::Object(record) => {
                    canonical_profiles.insert(
                        name.clone(),
                        Value::Object(canonicalize_blocks_in(
                            record,
                            &["profiles".to_string(), name.clone()],
                            &mut renames,
                        )),
                    );
                }
                other => {
                    canonical_profiles.insert(name.clone(), other.clone());
                }
            }
        }
        canonicalized.insert("profiles".to_string(), Value::Object(canonical_profiles));
    }
    CanonicalizeLegacyHarnessBlocksResult {
        document: canonicalized,
        renames,
    }
}

pub fn has_legacy_harness_blocks(document: &Value) -> bool {
    !canonicalize_legacy_harness_blocks(document)
        .renames
        .is_empty()
}
