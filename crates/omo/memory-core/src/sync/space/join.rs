//! Join toward the lower space ID. The caller establishes a shared binding first.

use loro::{LoroDoc, LoroResult};

use super::manifest::{
    manifest_absorbed_into, manifest_add_binding, manifest_bindings, manifest_mark_absorbed,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinDirection {
    pub target: String,
    pub absorbed: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinResult {
    pub target_id: String,
    pub absorbed_id: String,
    pub changed: bool,
}

pub fn join_direction(a_id: &str, b_id: &str) -> Option<JoinDirection> {
    match a_id.encode_utf16().cmp(b_id.encode_utf16()) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Less => Some(JoinDirection {
            target: a_id.into(),
            absorbed: b_id.into(),
        }),
        std::cmp::Ordering::Greater => Some(JoinDirection {
            target: b_id.into(),
            absorbed: a_id.into(),
        }),
    }
}

pub fn join_spaces(
    a_doc: &LoroDoc,
    a_id: &str,
    b_doc: &LoroDoc,
    b_id: &str,
) -> LoroResult<JoinResult> {
    let Some(dir) = join_direction(a_id, b_id) else {
        return Ok(JoinResult {
            target_id: a_id.into(),
            absorbed_id: a_id.into(),
            changed: false,
        });
    };
    let (absorbed_doc, target_doc) = if dir.absorbed == a_id {
        (a_doc, b_doc)
    } else {
        (b_doc, a_doc)
    };
    if let Some(existing) = manifest_absorbed_into(absorbed_doc) {
        return Ok(JoinResult {
            target_id: existing,
            absorbed_id: dir.absorbed,
            changed: false,
        });
    }
    for key in manifest_bindings(absorbed_doc) {
        manifest_add_binding(target_doc, &key)?;
    }
    manifest_mark_absorbed(absorbed_doc, &dir.target)?;
    Ok(JoinResult {
        target_id: dir.target,
        absorbed_id: dir.absorbed,
        changed: true,
    })
}
