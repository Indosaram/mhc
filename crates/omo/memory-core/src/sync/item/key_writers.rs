use super::{
    ItemError, crdt,
    frontmatter::FmValue,
    history::{FmPart, ItemHistory},
    state::{
        CombinedValue, KeyState, active_reg, combine_key, fm_name, ordered_set_elements,
        read_key_state, reg_text,
    },
    tag::new_tag,
};
use loro::{LoroDoc, LoroValue};
use std::collections::BTreeSet;

fn write_reg(
    doc: &LoroDoc,
    key: &str,
    value: Option<&str>,
    supersedes: Vec<String>,
    slot: usize,
) -> Result<(), ItemError> {
    let entry = LoroValue::from(serde_json::json!({"v":value,"s":supersedes}));
    doc.get_map(fm_name(key, FmPart::Reg))
        .insert(&new_tag(doc, value.unwrap_or("null"), slot), entry)
        .map_err(crdt)
}
pub fn write_natural(doc: &LoroDoc, key: &str, value: &FmValue) -> Result<(), ItemError> {
    match value {
        FmValue::Set { elements, .. } => {
            for (slot, e) in elements.iter().enumerate() {
                doc.get_map(fm_name(key, FmPart::Set))
                    .insert(&new_tag(doc, e, slot), e.as_str())
                    .map_err(crdt)?;
            }
            Ok(())
        }
        FmValue::Reg { value } => write_reg(doc, key, Some(value), Vec::new(), 0),
        FmValue::Remove => Ok(()),
    }
}
pub fn apply_key(
    doc: &LoroDoc,
    key: &str,
    value: &FmValue,
    history: &ItemHistory,
    unseen: &BTreeSet<String>,
) -> Result<(), ItemError> {
    let Some(kh) = history.keys.get(key) else {
        return write_natural(doc, key, value);
    };
    let state = read_key_state(doc, key);
    let combined = combine_key(&state, key, kh, history);
    match kh.kind {
        FmPart::Set => {
            let target = match value {
                FmValue::Set { elements, .. } => elements.clone(),
                FmValue::Reg { value } => vec![value.clone()],
                FmValue::Remove => return clear_key(doc, key, unseen),
            };
            let current = match combined {
                Some(CombinedValue::Set { elements }) => elements,
                _ => ordered_set_elements(&state, key, history),
            };
            if current == target {
                return Ok(());
            }
            let wanted: BTreeSet<_> = target.iter().collect();
            let present: BTreeSet<_> = state.set.iter().map(|(_, e)| e).collect();
            let map = doc.get_map(fm_name(key, FmPart::Set));
            for (tag, e) in &state.set {
                if !wanted.contains(e) && !unseen.contains(tag) {
                    map.delete(tag).map_err(crdt)?;
                }
            }
            for (slot, e) in target.iter().enumerate() {
                if !present.contains(e) {
                    map.insert(&new_tag(doc, e, slot), e.as_str())
                        .map_err(crdt)?;
                }
            }
            supersede_register(doc, key, &state, unseen)
        }
        FmPart::Reg => {
            let target = match value {
                FmValue::Set { flow, .. } => flow,
                FmValue::Reg { value } => value,
                FmValue::Remove => return clear_key(doc, key, unseen),
            };
            if let Some(CombinedValue::Reg { text, verbatim }) = combined
                && (text == *target || (!verbatim && text == reg_text(target)))
            {
                return Ok(());
            }
            let active = active_reg(&state.reg);
            if !active.is_empty() && active.iter().all(|e| e.value.as_deref() == Some(target)) {
                return Ok(());
            }
            write_reg(
                doc,
                key,
                Some(target),
                active
                    .iter()
                    .filter(|e| !unseen.contains(&e.tag))
                    .map(|e| e.tag.clone())
                    .collect(),
                0,
            )?;
            delete_seen_elements(doc, key, &state, unseen)
        }
    }
}
fn delete_seen_elements(
    doc: &LoroDoc,
    key: &str,
    state: &KeyState,
    unseen: &BTreeSet<String>,
) -> Result<(), ItemError> {
    for (tag, _) in &state.set {
        if !unseen.contains(tag) {
            doc.get_map(fm_name(key, FmPart::Set))
                .delete(tag)
                .map_err(crdt)?;
        }
    }
    Ok(())
}
fn supersede_register(
    doc: &LoroDoc,
    key: &str,
    state: &KeyState,
    unseen: &BTreeSet<String>,
) -> Result<(), ItemError> {
    let active: Vec<_> = active_reg(&state.reg)
        .iter()
        .filter(|e| !unseen.contains(&e.tag))
        .map(|e| e.tag.clone())
        .collect();
    if active.is_empty() {
        Ok(())
    } else {
        write_reg(doc, key, None, active, 0)
    }
}
pub fn clear_key(doc: &LoroDoc, key: &str, unseen: &BTreeSet<String>) -> Result<(), ItemError> {
    let state = read_key_state(doc, key);
    delete_seen_elements(doc, key, &state, unseen)?;
    supersede_register(doc, key, &state, unseen)
}
pub fn edit_set(
    doc: &LoroDoc,
    key: &str,
    removed: &BTreeSet<String>,
    added: &[String],
    unseen: &BTreeSet<String>,
) -> Result<(), ItemError> {
    let state = read_key_state(doc, key);
    let map = doc.get_map(fm_name(key, FmPart::Set));
    for (tag, e) in &state.set {
        if removed.contains(e) && !unseen.contains(tag) {
            map.delete(tag).map_err(crdt)?;
        }
    }
    let present: BTreeSet<_> = state
        .set
        .iter()
        .filter(|(_, e)| !removed.contains(e))
        .map(|(_, e)| e)
        .collect();
    for (slot, e) in added.iter().enumerate() {
        if !present.contains(e) {
            map.insert(&new_tag(doc, e, slot), e.as_str())
                .map_err(crdt)?;
        }
    }
    Ok(())
}
pub fn add_register_value(
    doc: &LoroDoc,
    key: &str,
    value: &str,
    slot: usize,
) -> Result<(), ItemError> {
    if read_key_state(doc, key)
        .reg
        .iter()
        .any(|e| e.value.as_deref() == Some(value))
    {
        return Ok(());
    }
    write_reg(doc, key, Some(value), Vec::new(), slot)
}
