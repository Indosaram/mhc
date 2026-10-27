//! Read-time canonicalisation of legacy names (spec §4.6).

use serde_json::{Map, Value};

/// The retired category name and its replacement.
pub const LEGACY_CATEGORY: &str = "deep";
pub const CATEGORY_REPLACEMENT: &str = "deep-low";
/// The legacy harness block spelling and its replacement.
pub const LEGACY_HARNESS: &str = "[senpi]";
pub const NATIVE_HARNESS: &str = "[native]";

/// What happened to one legacy name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rename {
    /// Dotted path of the legacy key or field.
    pub path: String,
    /// `true` when renamed, `false` when dropped because the new name already existed.
    pub renamed: bool,
}

fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_owned()
    } else {
        format!("{prefix}.{key}")
    }
}

/// Renames the legacy category everywhere in `value` (as a `categories` key at any depth and as
/// the string value of any `category` field).
pub fn rename_legacy_categories(value: &mut Value) -> Vec<Rename> {
    let mut out = Vec::new();
    walk_categories(value, "", &mut out);
    out
}

fn walk_categories(value: &mut Value, path: &str, out: &mut Vec<Rename>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(categories)) = map.get_mut("categories") {
                rename_key(
                    categories,
                    LEGACY_CATEGORY,
                    CATEGORY_REPLACEMENT,
                    &join(path, "categories"),
                    out,
                );
            }
            if let Some(Value::String(name)) = map.get_mut("category")
                && name == LEGACY_CATEGORY
            {
                *name = CATEGORY_REPLACEMENT.into();
                out.push(Rename {
                    path: join(path, "category"),
                    renamed: true,
                });
            }
            for (key, child) in map.iter_mut() {
                walk_categories(child, &join(path, key), out);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter_mut().enumerate() {
                walk_categories(child, &join(path, &index.to_string()), out);
            }
        }
        _ => {}
    }
}

/// Renames `from` to `to` inside `map`, keeping the key position; drops `from` when `to` exists.
pub fn rename_key(
    map: &mut Map<String, Value>,
    from: &str,
    to: &str,
    path: &str,
    out: &mut Vec<Rename>,
) {
    if !map.contains_key(from) {
        return;
    }
    let renamed = !map.contains_key(to);
    let entries = std::mem::take(map);
    for (key, value) in entries {
        if key == from {
            if renamed {
                map.insert(to.to_owned(), value);
            }
        } else {
            map.insert(key, value);
        }
    }
    out.push(Rename {
        path: join(path, from),
        renamed,
    });
}

/// Renames `[senpi]` to `[native]` at the top level and inside every profile.
pub fn rename_legacy_harness(value: &mut Value) -> Vec<Rename> {
    let mut out = Vec::new();
    let Value::Object(root) = value else {
        return out;
    };
    rename_key(root, LEGACY_HARNESS, NATIVE_HARNESS, "", &mut out);
    if let Some(Value::Object(profiles)) = root.get_mut("profiles") {
        for (name, body) in profiles.iter_mut() {
            if let Value::Object(body) = body {
                rename_key(
                    body,
                    LEGACY_HARNESS,
                    NATIVE_HARNESS,
                    &format!("profiles.{name}"),
                    &mut out,
                );
            }
        }
    }
    out
}

/// Whether the document still contains a legacy category name.
pub fn has_legacy_category(value: &Value) -> bool {
    let mut copy = value.clone();
    !rename_legacy_categories(&mut copy).is_empty()
}

/// Whether the document contains a `[senpi]` block (top level or in a profile).
pub fn has_legacy_harness(value: &Value) -> bool {
    let Some(root) = value.as_object() else {
        return false;
    };
    root.contains_key(LEGACY_HARNESS)
        || root
            .get("profiles")
            .and_then(Value::as_object)
            .is_some_and(|profiles| {
                profiles.values().any(|body| {
                    body.as_object()
                        .is_some_and(|b| b.contains_key(LEGACY_HARNESS))
                })
            })
}
