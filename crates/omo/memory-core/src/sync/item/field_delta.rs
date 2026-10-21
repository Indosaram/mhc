use super::{
    ItemError, crdt,
    frontmatter::{FmValue, split_item},
    history::{FmPart, analyze_history},
    key_writers::{apply_key, clear_key, edit_set, write_natural},
    three_way::merge_text,
};
use loro::{LoroDoc, UpdateOptions};
use std::collections::{BTreeMap, BTreeSet};
pub fn apply_user_delta(
    doc: &LoroDoc,
    before: &str,
    after: &str,
    unseen: &BTreeSet<String>,
) -> Result<(), ItemError> {
    let b = split_item(before);
    let a = split_item(after);
    let mut keys: Vec<String> =
        a.fm.as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|e| e.key.clone())
            .collect();
    for entry in b.fm.as_deref().unwrap_or(&[]) {
        if !keys.contains(&entry.key) {
            keys.push(entry.key.clone());
        }
    }
    let before: BTreeMap<_, _> =
        b.fm.unwrap_or_default()
            .into_iter()
            .map(|e| (e.key, e.value))
            .collect();
    let after: BTreeMap<_, _> =
        a.fm.unwrap_or_default()
            .into_iter()
            .map(|e| (e.key, e.value))
            .collect();
    let history = analyze_history(doc);
    for key in keys {
        let key = &key;
        let old = before.get(key);
        let new = after.get(key);
        if old == new {
            continue;
        }
        let Some(new) = new.filter(|v| **v != FmValue::Remove) else {
            clear_key(doc, key, unseen)?;
            continue;
        };
        match history.keys.get(key).map(|kh| kh.kind) {
            None => write_natural(doc, key, new)?,
            Some(FmPart::Reg) => apply_key(doc, key, new, &history, unseen)?,
            Some(FmPart::Set) => match (old, new) {
                (Some(FmValue::Set { elements: old, .. }), FmValue::Set { elements: new, .. }) => {
                    let removed = old.iter().filter(|e| !new.contains(e)).cloned().collect();
                    let added: Vec<_> = new.iter().filter(|e| !old.contains(e)).cloned().collect();
                    edit_set(doc, key, &removed, &added, unseen)?;
                }
                _ => apply_key(doc, key, new, &history, unseen)?,
            },
        }
    }
    if b.body != a.body {
        let current = doc.get_text("body").to_string();
        let merged = merge_text(&b.body, &a.body, &current)?;
        if current != merged {
            doc.get_text("body")
                .update(&merged, UpdateOptions::default())
                .map_err(crdt)?;
        }
    }
    Ok(())
}
