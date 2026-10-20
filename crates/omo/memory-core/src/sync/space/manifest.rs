//! Add-only binding aliases, display text, and absorbing join tombstones.

use loro::{
    ExportMode, LoroDoc, LoroEncodeError, LoroResult, LoroValue, PeerID, UpdateOptions,
    UpdateTimeoutError, ValueOrContainer,
};

const BINDINGS_NAME: &str = "bindings";
const NAME_NAME: &str = "name";
const ABSORBED_NAME: &str = "absorbed_into";

pub fn create_manifest_doc(peer: PeerID) -> LoroResult<LoroDoc> {
    let doc = LoroDoc::new();
    doc.set_peer_id(peer)?;
    doc.set_record_timestamp(false);
    Ok(doc)
}

pub fn manifest_add_binding(doc: &LoroDoc, key: &str) -> LoroResult<bool> {
    let map = doc.get_map(BINDINGS_NAME);
    if map.get(key).is_some() {
        return Ok(false);
    }
    map.insert(key, true)?;
    doc.commit();
    Ok(true)
}

pub fn manifest_has_binding(doc: &LoroDoc, key: &str) -> bool {
    doc.get_map(BINDINGS_NAME).get(key).is_some()
}

pub fn manifest_bindings(doc: &LoroDoc) -> Vec<String> {
    let mut keys: Vec<String> = doc
        .get_map(BINDINGS_NAME)
        .keys()
        .map(|key| key.to_string())
        .collect();
    // JavaScript's default sort compares UTF-16 code units, not UTF-8 bytes.
    keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    keys
}

pub fn manifest_display_name(doc: &LoroDoc) -> Option<String> {
    let text = doc.get_text(NAME_NAME).to_string();
    if text.is_empty() { None } else { Some(text) }
}

pub fn manifest_set_display_name(doc: &LoroDoc, name: &str) -> Result<(), UpdateTimeoutError> {
    let text = doc.get_text(NAME_NAME);
    if text.to_string() == name {
        return Ok(());
    }
    text.update(name, UpdateOptions::default())?;
    doc.commit();
    Ok(())
}

pub fn manifest_absorbed_into(doc: &LoroDoc) -> Option<String> {
    match doc.get_movable_list(ABSORBED_NAME).get(0) {
        Some(ValueOrContainer::Value(LoroValue::String(value))) => Some(value.to_string()),
        _ => None,
    }
}

pub fn manifest_mark_absorbed(doc: &LoroDoc, target_space_id: &str) -> LoroResult<()> {
    if manifest_absorbed_into(doc).is_some() {
        return Ok(());
    }
    doc.get_movable_list(ABSORBED_NAME).push(target_space_id)?;
    doc.commit();
    Ok(())
}

pub fn manifest_export(doc: &LoroDoc) -> Result<Vec<u8>, LoroEncodeError> {
    doc.export(ExportMode::all_updates())
}
