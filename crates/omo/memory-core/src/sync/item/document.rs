use super::{ItemError, crdt};
use loro::{Frontiers, LoroDoc, LoroValue, ToJson, VersionVector};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontierId {
    pub peer: String,
    pub counter: i32,
}
pub type ItemFrontier = Vec<FrontierId>;
pub fn create_item_doc(peer: u64) -> Result<LoroDoc, ItemError> {
    let doc = LoroDoc::new();
    doc.set_peer_id(peer).map_err(crdt)?;
    doc.set_record_timestamp(false);
    Ok(doc)
}
pub fn item_frontier(doc: &LoroDoc) -> ItemFrontier {
    doc.state_frontiers()
        .iter()
        .map(|id| FrontierId {
            peer: id.peer.to_string(),
            counter: id.counter,
        })
        .collect()
}
pub(crate) fn frontiers(ids: &ItemFrontier) -> Result<Frontiers, ItemError> {
    let mut out = Frontiers::new();
    for id in ids {
        out.push(loro::ID::new(
            id.peer.parse::<u64>().map_err(crdt)?,
            id.counter,
        ));
    }
    Ok(out)
}
pub fn delete_item(doc: &LoroDoc) -> Result<(), ItemError> {
    doc.commit();
    let mut ends: BTreeMap<String, i32> = doc
        .oplog_vv()
        .iter()
        .map(|(p, e)| (p.to_string(), *e))
        .collect();
    *ends.entry(doc.peer_id().to_string()).or_default() += 1;
    let value = serde_json::Value::Array(
        ends.into_iter()
            .map(|(p, e)| serde_json::json!([p, e]))
            .collect(),
    );
    doc.get_list("tombstones")
        .push(LoroValue::from(value))
        .map_err(crdt)?;
    doc.commit();
    Ok(())
}
pub fn is_live(doc: &LoroDoc) -> bool {
    let mut coverage = BTreeMap::new();
    let entries = doc.get_list("tombstones").get_deep_value().to_json_value();
    for entry in entries.as_array().into_iter().flatten() {
        for pair in entry.as_array().into_iter().flatten() {
            if let Some(pair) = pair.as_array()
                && let (Some(peer), Some(end)) = (
                    pair.first().and_then(|v| v.as_str()),
                    pair.get(1).and_then(|v| v.as_i64()),
                )
            {
                let covered = coverage.entry(peer.to_owned()).or_insert(0i64);
                *covered = (*covered).max(end);
            }
        }
    }
    doc.oplog_vv()
        .iter()
        .any(|(p, e)| i64::from(*e) > *coverage.get(&p.to_string()).unwrap_or(&0))
}
pub(crate) fn update_since(
    doc: &LoroDoc,
    from: &VersionVector,
) -> Result<Option<Vec<u8>>, ItemError> {
    if &doc.oplog_vv() == from {
        return Ok(None);
    }
    doc.export(loro::ExportMode::updates(from))
        .map(Some)
        .map_err(crdt)
}
