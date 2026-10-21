use super::{ItemError, crdt, document::create_item_doc};
use loro::{ExportMode, UpdateOptions};
pub fn merge_text(base: &str, ours: &str, theirs: &str) -> Result<String, ItemError> {
    if ours == base || ours == theirs {
        return Ok(theirs.into());
    }
    if theirs == base {
        return Ok(ours.into());
    }
    let root = create_item_doc(1)?;
    root.get_text("t").insert(0, base).map_err(crdt)?;
    root.commit();
    let left = root.fork();
    left.set_peer_id(2).map_err(crdt)?;
    left.get_text("t")
        .update(ours, UpdateOptions::default())
        .map_err(crdt)?;
    left.commit();
    let right = root.fork();
    right.set_peer_id(3).map_err(crdt)?;
    right
        .get_text("t")
        .update(theirs, UpdateOptions::default())
        .map_err(crdt)?;
    right.commit();
    left.import(
        &right
            .export(ExportMode::updates(&root.oplog_vv()))
            .map_err(crdt)?,
    )
    .map_err(crdt)?;
    Ok(left.get_text("t").to_string())
}
