//! Port of `config/unknown-settings.ts`: a settings file written for a newer version may name keys
//! this version does not know. They are dropped with one warning each instead of failing the whole
//! file, so the other settings still apply. Only the top level is lenient; known nested objects
//! keep their strict schemas.

use serde_json::{Map, Value};
use std::path::Path;

pub struct KnownSettings {
    pub value: Value,
    pub warnings: Vec<String>,
}

pub fn without_unknown_top_level_keys(
    parsed: Value,
    known: &[String],
    path: &Path,
) -> KnownSettings {
    let Value::Object(object) = parsed else {
        return KnownSettings {
            value: parsed,
            warnings: Vec::new(),
        };
    };
    let mut kept = Map::new();
    let mut warnings = Vec::new();
    for (key, value) in object {
        if known.contains(&key) {
            kept.insert(key, value);
        } else {
            warnings.push(format!(
                "Unknown codemode setting \"{key}\" in {} is ignored; the other settings still apply.",
                path.display()
            ));
        }
    }
    KnownSettings {
        value: Value::Object(kept),
        warnings,
    }
}
