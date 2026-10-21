use super::{ItemError, crdt, tag::new_tag};
use loro::{LoroDoc, LoroValue, ToJson, UpdateOptions};
use std::collections::BTreeMap;
pub fn transplant_content(target: &LoroDoc, merged: &LoroDoc) -> Result<(), ItemError> {
    let roots = merged.get_value().to_json_value();
    // JSON preserve_order exposes Loro's hash-map traversal. Fresh operation
    // counters must not depend on that traversal or on the scratch peer's tags.
    let roots: BTreeMap<_, _> = roots.as_object().into_iter().flatten().collect();
    for (name, cid) in roots {
        if name != "body" && !name.starts_with("fm\u{1f}") {
            continue;
        }
        let Some(cid) = cid.as_str() else { continue };
        if cid.ends_with(":Text") {
            let want = merged.get_text(name.as_str()).to_string();
            if target.get_text(name.as_str()).to_string() != want {
                target
                    .get_text(name.as_str())
                    .update(&want, UpdateOptions::default())
                    .map_err(crdt)?;
            }
        } else if cid.ends_with(":Map") {
            let want = merged
                .get_map(name.as_str())
                .get_deep_value()
                .to_json_value();
            let have = target
                .get_map(name.as_str())
                .get_deep_value()
                .to_json_value();
            let Some(want) = want.as_object() else {
                continue;
            };
            let Some(have) = have.as_object() else {
                continue;
            };
            let added: Vec<_> = want.keys().filter(|tag| !have.contains_key(*tag))
                .collect::<std::collections::BTreeSet<_>>().into_iter().collect();
            let mut renames = BTreeMap::new();
            for (i, tag) in added.iter().enumerate() {
                let value = &want[*tag];
                let salt = if let Some(object) = value.as_object() {
                    serde_json::to_string(&object.iter().collect::<BTreeMap<_, _>>()).map_err(crdt)?
                } else {
                    value.to_string()
                };
                renames.insert((*tag).clone(), new_tag(target, &salt, i + 1));
            }
            let map = target.get_map(name.as_str());
            for tag in have.keys().filter(|tag| !want.contains_key(*tag))
                .collect::<std::collections::BTreeSet<_>>() {
                map.delete(tag).map_err(crdt)?;
            }
            for tag in added {
                let mut value = want[tag].clone();
                if let Some(s) = value.get_mut("s").and_then(|v| v.as_array_mut()) {
                    for old in s {
                        if let Some(new) = old.as_str().and_then(|t| renames.get(t)) {
                            *old = serde_json::Value::String(new.clone());
                        }
                    }
                }
                map.insert(&renames[tag], LoroValue::from(value))
                    .map_err(crdt)?;
            }
        }
    }
    Ok(())
}
