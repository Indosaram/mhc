use super::{
    ItemError, crdt,
    document::update_since,
    history::{FmPart, analyze_history},
    key_writers::add_register_value,
    state::{read_key_state, register_values},
};
use loro::LoroDoc;
use std::collections::BTreeMap;
#[derive(Clone, Debug)]
pub struct RegisterValue {
    pub key: String,
    pub value: String,
}
pub fn with_register_values(
    doc: &LoroDoc,
    captured: Option<Vec<u8>>,
    values: &[RegisterValue],
    message: Option<&str>,
) -> Result<Option<Vec<u8>>, ItemError> {
    if values.is_empty() {
        return Ok(captured);
    }
    let target = doc.fork();
    target.set_peer_id(doc.peer_id()).map_err(crdt)?;
    target.set_record_timestamp(false);
    if let Some(update) = captured {
        target.import(&update).map_err(crdt)?;
    }
    for (slot, v) in values.iter().enumerate() {
        add_register_value(&target, &v.key, &v.value, slot)?;
    }
    if let Some(message) = message {
        target.set_next_commit_message(message);
    }
    target.commit();
    update_since(&target, &doc.oplog_vv())
}
pub fn register_values_written(doc: &LoroDoc, values: &[RegisterValue]) -> bool {
    values.iter().all(|v| {
        read_key_state(doc, &v.key)
            .reg
            .iter()
            .any(|e| e.value.as_ref() == Some(&v.value))
    })
}
pub fn active_registers(doc: &LoroDoc) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for (key, kh) in analyze_history(doc).keys {
        if kh.kind != FmPart::Reg {
            continue;
        }
        let state = read_key_state(doc, &key);
        let mut parts = register_values(&state.reg);
        for (_, e) in state.set {
            if !parts.contains(&e) {
                parts.push(e);
            }
        }
        if !parts.is_empty() {
            out.insert(key, parts);
        }
    }
    out
}
