use super::{
    ItemError, crdt,
    document::{ItemFrontier, frontiers, item_frontier, update_since},
    frontmatter::{FmValue, split_item},
    history::analyze_history,
    key_writers::{apply_key, clear_key},
    render::render_doc_content,
    transplant::transplant_content,
};
use loro::{LoroDoc, UpdateOptions};
use std::collections::{BTreeMap, BTreeSet};
pub fn capture_item(
    doc: &LoroDoc,
    base_text: &str,
    base_frontier: &ItemFrontier,
    working_text: &str,
    message: Option<&str>,
) -> Result<Option<Vec<u8>>, ItemError> {
    let current = super::capture_receipts::same_frontier(&item_frontier(doc), base_frontier);
    let merged = if current {
        None
    } else {
        let baseline = frontiers(base_frontier)?;
        let scratch = doc.fork_at(&baseline).map_err(crdt)?;
        // A random fork peer leaks into element tags and therefore transplant
        // order. Use a throwaway peer absent from the entire current history,
        // not just the baseline, so its counters cannot collide on merge.
        let peers: BTreeSet<_> = doc.oplog_vv().keys().copied().collect();
        let mut peer = 0;
        while peers.contains(&peer) {
            peer += 1;
        }
        scratch.set_peer_id(peer).map_err(crdt)?;
        scratch.set_record_timestamp(false);
        if render_doc_content(&scratch)? != base_text {
            return Err(ItemError::CaptureBaseline);
        }
        apply_working_text(&scratch, working_text)?;
        scratch.commit();
        let from = scratch
            .frontiers_to_vv(&baseline)
            .ok_or(ItemError::CaptureBaseline)?;
        let Some(update) = update_since(&scratch, &from)? else {
            return Ok(None);
        };
        let merged = doc.fork();
        merged.set_record_timestamp(false);
        merged.import(&update).map_err(crdt)?;
        Some(merged)
    };
    let target = doc.fork();
    target.set_peer_id(doc.peer_id()).map_err(crdt)?;
    target.set_record_timestamp(false);
    match merged {
        Some(merged) => transplant_content(&target, &merged)?,
        None => {
            if render_doc_content(&target)? != base_text {
                return Err(ItemError::CaptureBaseline);
            }
            apply_working_text(&target, working_text)?;
        }
    }
    if let Some(message) = message {
        target.set_next_commit_message(message);
    }
    target.commit();
    update_since(&target, &doc.oplog_vv())
}
fn apply_working_text(doc: &LoroDoc, text: &str) -> Result<(), ItemError> {
    let parsed = split_item(text);
    let history = analyze_history(doc);
    let none = BTreeSet::new();
    let wanted: BTreeMap<_, _> = parsed
        .fm
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|e| (e.key.clone(), &e.value))
        .collect();
    for key in history.keys.keys() {
        if wanted.get(key).is_none_or(|v| matches!(v, FmValue::Remove)) {
            clear_key(doc, key, &none)?;
        }
    }
    for entry in parsed.fm.as_deref().unwrap_or(&[]) {
        if entry.value != FmValue::Remove {
            apply_key(doc, &entry.key, &entry.value, &history, &none)?;
        }
    }
    if doc.get_text("body").to_string() != parsed.body {
        doc.get_text("body")
            .update(&parsed.body, UpdateOptions::default())
            .map_err(crdt)?;
    }
    Ok(())
}
