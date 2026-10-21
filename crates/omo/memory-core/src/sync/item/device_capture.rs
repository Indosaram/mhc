use super::{
    ItemError,
    adopt_values::{RegisterValue, register_values_written, with_register_values},
    capture::capture_item,
    capture_receipts::{
        CaptureReceipt, OpSpan, capture_message, read_receipts, receipt_for, receipt_landed,
        same_frontier, write_receipts,
    },
    crdt,
    device::{ItemDevice, assert_device_doc},
    document::{ItemFrontier, frontiers, is_live, update_since},
    field_delta::apply_user_delta,
    render::render_doc_content,
};
use crate::support::random::random_id;
use loro::{LoroDoc, ToJson};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, OnceLock},
};
type Locks = BTreeMap<(std::path::PathBuf, String), std::sync::Weak<Mutex<()>>>;
fn serialized_lock(device: &ItemDevice, key: &str) -> Result<Arc<Mutex<()>>, ItemError> {
    static LOCKS: OnceLock<Mutex<Locks>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(crdt)?;
    locks.retain(|_, v| v.strong_count() > 0);
    let key = (device.state_dir.clone(), key.into());
    if let Some(lock) = locks.get(&key).and_then(|w| w.upgrade()) {
        return Ok(lock);
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    Ok(lock)
}
pub fn capture_on_device(
    device: &ItemDevice,
    item_key: &str,
    doc: &LoroDoc,
    base_text: &str,
    base_frontier: &ItemFrontier,
    working_text: &str,
    register_values: &[RegisterValue],
) -> Result<Option<Vec<u8>>, ItemError> {
    assert_device_doc(device, doc)?;
    let lock = serialized_lock(device, item_key)?;
    let _guard = lock.lock().map_err(crdt)?;
    let receipts = read_receipts(&device.state_dir, item_key)?;
    let unchanged = is_live(doc) && working_text == render_doc_content(doc)?;
    if unchanged && register_values_written(doc, register_values) {
        save_receipt(
            device,
            item_key,
            &receipts,
            receipt_for(
                doc,
                base_frontier,
                working_text,
                map_tags(doc).into_iter().collect(),
                None,
            )?,
        )?;
        return Ok(None);
    }
    let prior = receipts
        .iter()
        .find(|r| same_frontier(&r.base, base_frontier) && receipt_landed(doc, r));
    let capture_id = random_id();
    let message = capture_message(&capture_id);
    let captured = if unchanged {
        None
    } else if let Some(prior) = prior {
        let target = doc.fork();
        target.set_peer_id(doc.peer_id()).map_err(crdt)?;
        target.set_record_timestamp(false);
        let seen: BTreeSet<_> = prior.seen.iter().cloned().collect();
        let unseen = map_tags(&target).difference(&seen).cloned().collect();
        apply_user_delta(&target, &prior.text, working_text, &unseen)?;
        target.set_next_commit_message(&message);
        target.commit();
        update_since(&target, &doc.oplog_vv())?
    } else {
        capture_item(doc, base_text, base_frontier, working_text, Some(&message))?
    };
    let Some(update) = with_register_values(doc, captured, register_values, Some(&message))? else {
        return Ok(None);
    };
    let mut seen = if unchanged {
        map_tags(doc)
    } else if let Some(prior) = prior {
        prior.seen.iter().cloned().collect()
    } else {
        map_tags(&doc.fork_at(&frontiers(base_frontier)?).map_err(crdt)?)
    };
    let before = map_tags(doc);
    let start = doc.oplog_vv().get(&doc.peer_id()).copied().unwrap_or(0);
    let after = doc.fork();
    after.import(&update).map_err(crdt)?;
    seen.extend(map_tags(&after).difference(&before).cloned());
    let end = after.oplog_vv().get(&doc.peer_id()).copied().unwrap_or(0);
    let span = OpSpan {
        peer: doc.peer_id().to_string(),
        start,
        end,
        capture_id,
    };
    save_receipt(
        device,
        item_key,
        &receipts,
        receipt_for(
            &after,
            base_frontier,
            working_text,
            seen.into_iter().collect(),
            Some(span),
        )?,
    )?;
    Ok(Some(update))
}
pub fn record_materialized(
    device: &ItemDevice,
    item_key: &str,
    doc: &LoroDoc,
    base_frontier: &ItemFrontier,
    written_text: &str,
) -> Result<(), ItemError> {
    assert_device_doc(device, doc)?;
    let lock = serialized_lock(device, item_key)?;
    let _guard = lock.lock().map_err(crdt)?;
    let receipts = read_receipts(&device.state_dir, item_key)?;
    save_receipt(
        device,
        item_key,
        &receipts,
        receipt_for(
            doc,
            base_frontier,
            written_text,
            map_tags(doc).into_iter().collect(),
            None,
        )?,
    )
}
fn save_receipt(
    device: &ItemDevice,
    item_key: &str,
    receipts: &[CaptureReceipt],
    next: CaptureReceipt,
) -> Result<(), ItemError> {
    let mut kept: Vec<_> = receipts
        .iter()
        .filter(|r| !same_frontier(&r.base, &next.base))
        .cloned()
        .collect();
    kept.push(next);
    write_receipts(&device.state_dir, item_key, &kept)
}
fn map_tags(doc: &LoroDoc) -> BTreeSet<String> {
    let roots = doc.get_value().to_json_value();
    let mut tags = BTreeSet::new();
    for name in roots.as_object().into_iter().flat_map(|v| v.keys()) {
        if name.starts_with("fm\u{1f}")
            && (name.ends_with("\u{1f}reg") || name.ends_with("\u{1f}set"))
        {
            tags.extend(doc.get_map(name.as_str()).keys().map(|k| k.to_string()));
        }
    }
    tags
}
