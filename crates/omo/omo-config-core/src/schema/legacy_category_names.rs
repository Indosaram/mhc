use serde_json::{Map, Value};

/// Retired builtin category keys and the canonical key that replaced them.
///
/// `deep` was split into `deep-low` (default lane) and `deep-high` (escalation lane) in 2026-09.
/// A config written against the old name keeps working: the key is canonicalized to `deep-low` when
/// the config is loaded, and the startup migration rewrites the file itself.
pub const LEGACY_CATEGORY_NAME_ALIASES: [(&str, &str); 1] = [("deep", "deep-low")];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyCategoryRename {
    pub canonical: String,
    pub dropped: bool,
    pub legacy: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalizeLegacyCategoryNamesResult {
    pub document: Map<String, Value>,
    pub renames: Vec<LegacyCategoryRename>,
}

pub fn canonical_category_name(name: &str) -> String {
    match LEGACY_CATEGORY_NAME_ALIASES
        .iter()
        .find(|(legacy, _)| *legacy == name)
    {
        Some((_, canonical)) => (*canonical).to_string(),
        None => name.to_string(),
    }
}

fn child(path: &[String], segment: &str) -> Vec<String> {
    let mut next = path.to_vec();
    next.push(segment.to_string());
    next
}

fn join_path(path: &[String], segment: &str) -> String {
    child(path, segment).join(".")
}

// A `categories` record is keyed by category name; every other `category` property in the config
// schema (team members, memory reflection) holds one as a string VALUE. Both spellings are rewritten.
fn canonicalize_categories_record(
    categories: &Map<String, Value>,
    path: &[String],
    renames: &mut Vec<LegacyCategoryRename>,
) -> Map<String, Value> {
    let mut result = Map::new();
    for (name, definition) in categories {
        let canonical = canonical_category_name(name);
        if canonical == *name {
            result.insert(name.clone(), definition.clone());
            continue;
        }
        let dropped = categories.contains_key(&canonical);
        renames.push(LegacyCategoryRename {
            canonical: canonical.clone(),
            dropped,
            legacy: name.clone(),
            path: join_path(path, name),
        });
        if !dropped {
            result.insert(canonical, definition.clone());
        }
    }
    result
}

fn canonicalize_value(
    value: &Value,
    path: &[String],
    renames: &mut Vec<LegacyCategoryRename>,
) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    canonicalize_value(entry, &child(path, &index.to_string()), renames)
                })
                .collect(),
        ),
        Value::Object(map) => {
            let mut result = Map::new();
            for (key, entry) in map {
                if key == "categories" && entry.is_object() {
                    let record = entry.as_object().expect("checked object");
                    result.insert(
                        key.clone(),
                        Value::Object(canonicalize_categories_record(
                            record,
                            &child(path, key),
                            renames,
                        )),
                    );
                    continue;
                }
                if key == "category"
                    && let Value::String(text) = entry
                {
                    let canonical = canonical_category_name(text);
                    if canonical != *text {
                        renames.push(LegacyCategoryRename {
                            canonical: canonical.clone(),
                            dropped: false,
                            legacy: text.clone(),
                            path: join_path(path, key),
                        });
                    }
                    result.insert(key.clone(), Value::String(canonical));
                    continue;
                }
                result.insert(key.clone(), canonicalize_value(entry, &child(path, key), renames));
            }
            Value::Object(result)
        }
        other => other.clone(),
    }
}

pub fn canonicalize_legacy_category_names(document: &Value) -> CanonicalizeLegacyCategoryNamesResult {
    let mut renames = Vec::new();
    let canonicalized = match document {
        Value::Object(_) => canonicalize_value(document, &[], &mut renames),
        _ => Value::Object(Map::new()),
    };
    CanonicalizeLegacyCategoryNamesResult {
        document: match canonicalized {
            Value::Object(map) => map,
            _ => Map::new(),
        },
        renames,
    }
}

pub fn has_legacy_category_names(document: &Value) -> bool {
    !canonicalize_legacy_category_names(document).renames.is_empty()
}
