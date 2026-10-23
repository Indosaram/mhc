//! ItemStore over the item device. One Loro doc per item, created on the device peer; the docs
//! (Loro snapshots) and each item's last materialised text with its frontier live in
//! `<repo>/.git/omo-sync/crdt/` (sync-design §5: plaintext stays inside the repository's git dir),
//! written by temp file + rename. Capture is the device API's `capture_on_device` against that
//! baseline; a deleted working file captures as the item's tombstone; render is `render_for_disk`,
//! so a file whose model did not change keeps its own frontmatter style byte for byte.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use loro::{ExportMode, LoroDoc};
use serde::{Deserialize, Serialize};

use super::super::atomic::{read_json, write_atomic, write_json};
use super::super::error::{EngineError, EngineResult};
use super::super::ports::{BatchPayload, ItemStore};
use crate::fs::resilient;
use crate::support::sha256::sha256_hex;
use crate::sync::item::{
    ItemDevice, ItemFrontier, OpenItemDeviceOptions, capture_on_device, create_device_item_doc,
    delete_item, is_live, item_frontier, open_item_device, record_materialized, render_doc_content,
    render_for_disk, render_item,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Baseline {
    text: Option<String>,
    frontier: ItemFrontier,
}

pub struct DeviceItemStoreOptions {
    /// The device's private state dir (peer + receipts), outside the synced memory root.
    pub state_dir: PathBuf,
    pub memory_root: PathBuf,
    /// `<repo>/.git/omo-sync`: item docs and baselines go under `crdt/`.
    pub sync_dir: PathBuf,
    pub machine_id: Option<String>,
}

#[derive(Default)]
struct State {
    docs: BTreeMap<String, LoroDoc>,
    baselines: BTreeMap<String, Baseline>,
    dirty: BTreeSet<String>,
    device: Option<ItemDevice>,
}

pub struct DeviceItemStore {
    options: DeviceItemStoreOptions,
    state: RefCell<State>,
}

pub fn create_device_item_store(options: DeviceItemStoreOptions) -> DeviceItemStore {
    DeviceItemStore {
        options,
        state: RefCell::new(State::default()),
    }
}

fn file_of(key: &str) -> String {
    sha256_hex(key.as_bytes())
}

fn crdt_error(error: impl std::fmt::Display) -> EngineError {
    EngineError::Other(error.to_string())
}

fn live_device(state: &State) -> EngineResult<ItemDevice> {
    state
        .device
        .clone()
        .ok_or_else(|| EngineError::Other("the item store is used before load()".into()))
}

/// The doc for `key`, created on the device peer when the item is new.
fn doc_of<'a>(state: &'a mut State, key: &str) -> EngineResult<&'a LoroDoc> {
    if !state.docs.contains_key(key) {
        let device = live_device(state)?;
        state
            .docs
            .insert(key.to_string(), create_device_item_doc(&device)?);
    }
    state
        .docs
        .get(key)
        .ok_or_else(|| EngineError::Other("item doc vanished".into()))
}

/// A person re-created a deleted item. The dead doc still holds its last content at its current
/// frontier, so capture runs against exactly that (any new op past the tombstone makes the item
/// live again). Re-created with that same content, the diff is empty, so one explicit no-change op
/// revives it.
fn revive(
    device: &ItemDevice,
    key: &str,
    doc: &LoroDoc,
    working_text: &str,
) -> EngineResult<Option<Vec<u8>>> {
    let base = render_doc_content(doc)?;
    if let Some(ops) = capture_on_device(
        device,
        key,
        doc,
        &base,
        &item_frontier(doc),
        working_text,
        &[],
    )? {
        return Ok(Some(ops));
    }
    let target = doc.fork();
    target.set_peer_id(doc.peer_id()).map_err(crdt_error)?;
    target.set_record_timestamp(false);
    let body = target.get_text("body");
    body.insert(0, "\u{0}").map_err(crdt_error)?;
    body.delete(0, 1).map_err(crdt_error)?;
    target.commit();
    Ok(Some(
        target
            .export(ExportMode::updates(&doc.oplog_vv()))
            .map_err(crdt_error)?,
    ))
}

impl ItemStore for DeviceItemStore {
    fn load(&self) -> EngineResult<()> {
        let crdt = self.options.sync_dir.join("crdt");
        let device = open_item_device(OpenItemDeviceOptions {
            state_dir: &self.options.state_dir,
            memory_root: &self.options.memory_root,
            machine_id: self.options.machine_id.as_deref(),
        })?;
        let index: BTreeMap<String, Baseline> =
            read_json(&crdt.join("baselines.json"), BTreeMap::new())?;
        // Every persisted doc's key, materialised or not: an item applied from a verified batch has
        // CRDT state before it has a baseline, and a crash in between must not drop that state.
        let doc_keys: Vec<String> = read_json(&crdt.join("docs.json"), Vec::new())?;
        let names = match resilient::read_dir_names(&crdt.join("docs")) {
            Ok(names) => names,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        let by_file: BTreeMap<String, String> = index
            .keys()
            .cloned()
            .chain(doc_keys)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|k| (file_of(&k), k))
            .collect();
        let mut docs = BTreeMap::new();
        for name in names {
            let stem = name.strip_suffix(".loro").unwrap_or(&name);
            let Some(key) = by_file.get(stem) else {
                continue;
            };
            let doc = LoroDoc::new();
            doc.set_peer_id(device.peer).map_err(crdt_error)?;
            doc.set_record_timestamp(false);
            doc.import(&resilient::read(&crdt.join("docs").join(&name))?)
                .map_err(crdt_error)?;
            docs.insert(key.clone(), doc);
        }
        let mut state = self.state.borrow_mut();
        state.docs = docs;
        state.baselines = index;
        state.device = Some(device);
        Ok(())
    }

    fn peer(&self) -> EngineResult<String> {
        Ok(live_device(&self.state.borrow())?.peer.to_string())
    }

    fn keys(&self) -> Vec<String> {
        let state = self.state.borrow();
        state
            .docs
            .keys()
            .chain(state.baselines.keys())
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn capture(&self, key: &str, working_text: Option<&str>) -> EngineResult<Option<Vec<u8>>> {
        let mut state = self.state.borrow_mut();
        let device = live_device(&state)?;
        let base = state.baselines.get(key).cloned().unwrap_or(Baseline {
            text: None,
            frontier: Vec::new(),
        });
        let doc = doc_of(&mut state, key)?.clone();
        if working_text == base.text.as_deref() {
            return Ok(None);
        }
        let Some(text) = working_text else {
            if !is_live(&doc) {
                return Ok(None);
            }
            let from = doc.oplog_vv();
            delete_item(&doc)?;
            let update = doc.export(ExportMode::updates(&from)).map_err(crdt_error)?;
            state.dirty.insert(key.to_string());
            return Ok(Some(update));
        };
        if !is_live(&doc) && doc.oplog_vv().iter().next().is_some() {
            return revive(&device, key, &doc, text);
        }
        Ok(capture_on_device(
            &device,
            key,
            &doc,
            base.text.as_deref().unwrap_or(""),
            &base.frontier,
            text,
            &[],
        )?)
    }

    fn apply(&self, key: &str, ops: &[u8]) -> EngineResult<()> {
        let mut state = self.state.borrow_mut();
        doc_of(&mut state, key)?.import(ops).map_err(crdt_error)?;
        state.dirty.insert(key.to_string());
        Ok(())
    }

    fn render(&self, key: &str, disk_text: Option<&str>) -> EngineResult<Option<String>> {
        let state = self.state.borrow();
        let Some(doc) = state.docs.get(key) else {
            return Ok(None);
        };
        if !is_live(doc) {
            return Ok(None);
        }
        Ok(match disk_text {
            None => render_item(doc)?,
            Some(disk) => render_for_disk(doc, disk)?,
        })
    }

    fn version(&self, key: &str) -> EngineResult<String> {
        let mut state = self.state.borrow_mut();
        let mut ids: Vec<String> = item_frontier(doc_of(&mut state, key)?)
            .iter()
            .map(|id| format!("{}:{}", id.peer, id.counter))
            .collect();
        ids.sort();
        Ok(ids.join(","))
    }

    fn mark_materialized(&self, key: &str, text: Option<&str>) -> EngineResult<()> {
        let mut state = self.state.borrow_mut();
        let device = live_device(&state)?;
        let doc = doc_of(&mut state, key)?.clone();
        let frontier = item_frontier(&doc);
        if let Some(text) = text {
            record_materialized(&device, key, &doc, &frontier, text)?;
        }
        state.baselines.insert(
            key.to_string(),
            Baseline {
                text: text.map(str::to_string),
                frontier,
            },
        );
        state.dirty.insert(key.to_string());
        Ok(())
    }

    fn apply_snapshot(&self, payload: &BatchPayload) -> EngineResult<()> {
        let mut state = self.state.borrow_mut();
        for item in &payload.items {
            doc_of(&mut state, &item.key)?
                .import(&item.ops)
                .map_err(crdt_error)?;
            state.dirty.insert(item.key.clone());
        }
        Ok(())
    }

    fn persist(&self) -> EngineResult<()> {
        let crdt = self.options.sync_dir.join("crdt");
        let mut state = self.state.borrow_mut();
        let dirty = std::mem::take(&mut state.dirty);
        for key in &dirty {
            if let Some(doc) = state.docs.get(key) {
                let bytes = doc.export(ExportMode::snapshot()).map_err(crdt_error)?;
                write_atomic(
                    &crdt.join("docs").join(format!("{}.loro", file_of(key))),
                    &bytes,
                )?;
            }
        }
        let keys: Vec<&String> = state.docs.keys().collect();
        write_json(&crdt.join("docs.json"), &keys)?;
        write_json(&crdt.join("baselines.json"), &state.baselines)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "device_item_store_tests.rs"]
mod tests;
