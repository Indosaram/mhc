//! senpi `modes/rpc/compact-call-table.ts`: the resident side of the calls component (S06):
//! live entries, four indexes, owned slot values, and the persistent root.

use crate::compact_call_model::{
    CallCounters, IgnoredCounters, OutstandingCall, call_leaves, call_order, terminal,
};
use crate::compact_component_roots::{CompactComponentRoots, ComponentRoot};
use crate::compact_content_store::CompactContentStore;
use crate::compact_read_lease::CompactError;
use crate::compact_values::{CompactValues, ValueRef};
use futures::future::BoxFuture;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallSlotField {
    Hook,
    Progress,
    Result,
}

pub struct OwnedSlot {
    pub reference: ValueRef,
    pub bytes: usize,
}

struct Entry {
    call: OutstandingCall,
    order: String,
}

#[derive(Debug, Default)]
pub struct TableCounters {
    pub replacements: u64,
    pub progress_replacements: u64,
    pub unknown: u64,
    pub late: u64,
    pub collisions: u64,
    pub reused: u64,
}

fn turn_key(turn_id: &str, tool_call_id: &str) -> String {
    format!("{turn_id}\u{0000}{tool_call_id}")
}

fn message_key(message_id: &str, content_index: usize) -> String {
    format!("{message_id}#{content_index}")
}

fn shared_key(message_id: &str, tool_call_id: &str) -> String {
    format!("{message_id}\u{0000}{tool_call_id}")
}

fn marked(mut call: OutstandingCall, ambiguous: bool) -> OutstandingCall {
    call.call_unknown = ambiguous;
    call
}

fn header_bytes(call: &OutstandingCall) -> Result<usize, CompactError> {
    Ok(serde_json::to_string(call)?.len())
}

/// The resident side of the calls component (S06): the live entries, four indexes, the owned
/// slot values, and the persistent root every change republishes through the shared component
/// roots.
pub struct CompactCallTable {
    pub root: ComponentRoot<OutstandingCall>,
    store: CompactContentStore,
    values: CompactValues,
    roots: CompactComponentRoots<OutstandingCall>,
    entries: HashMap<String, Entry>,
    by_turn: HashMap<String, String>,
    by_message: HashMap<String, String>,
    by_id: HashMap<String, Vec<String>>,
    children: HashMap<String, BTreeSet<String>>,
    shared: HashSet<String>,
    slot_bytes_of: HashMap<(String, CallSlotField), usize>,
    slot_bytes: usize,
    header_bytes: usize,
    pub counters: TableCounters,
}

impl CompactCallTable {
    pub fn new(store: CompactContentStore) -> Self {
        let values = CompactValues::new(store.clone());
        let roots = CompactComponentRoots::new(store.clone(), call_leaves);
        Self {
            root: ComponentRoot::default(),
            store,
            values,
            roots,
            entries: HashMap::new(),
            by_turn: HashMap::new(),
            by_message: HashMap::new(),
            by_id: HashMap::new(),
            children: HashMap::new(),
            shared: HashSet::new(),
            slot_bytes_of: HashMap::new(),
            slot_bytes: 0,
            header_bytes: 0,
            counters: TableCounters::default(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&OutstandingCall> {
        self.entries.get(key).map(|entry| &entry.call)
    }

    pub fn by_turn_id(
        &self,
        turn_id: Option<&str>,
        tool_call_id: Option<&str>,
    ) -> Option<&OutstandingCall> {
        let turn_id = turn_id?;
        let tool_call_id = tool_call_id.filter(|id| !id.is_empty())?;
        let key = self.by_turn.get(&turn_key(turn_id, tool_call_id))?;
        self.get(key)
    }

    pub fn by_owner(&self, message_id: &str, content_index: usize) -> Option<&OutstandingCall> {
        let key = self
            .by_message
            .get(&message_key(message_id, content_index))?;
        self.get(key)
    }

    /// The newest call with this id that is non-terminal or holds a synthetic outcome.
    pub fn newest_by_id(&self, tool_call_id: &str) -> Option<&OutstandingCall> {
        let keys = self.by_id.get(tool_call_id)?;
        for key in keys.iter().rev() {
            if let Some(call) = self.get(key)
                && (!terminal(call)
                    || matches!(
                        call.outcome,
                        Some(crate::compact_call_model::CallOutcome::Abandoned)
                    ))
            {
                return Some(call);
            }
        }
        None
    }

    /// The oldest `open` call with this id, its own turn first (FIFO by creation, senpi#4278).
    pub fn oldest_by_id(
        &self,
        tool_call_id: &str,
        turn_id: Option<&str>,
        open: impl Fn(&OutstandingCall) -> bool,
    ) -> Option<&OutstandingCall> {
        let keys = self.by_id.get(tool_call_id)?;
        let mut oldest: Option<(&Entry, (u8, &str))> = None;
        for key in keys {
            let Some(entry) = self.entries.get(key) else {
                continue;
            };
            if !open(&entry.call) {
                continue;
            }
            let rank = (
                u8::from(turn_id != Some(entry.call.turn_id.as_str())),
                entry.order.as_str(),
            );
            if oldest.is_none_or(|(_, best_rank)| rank < best_rank) {
                oldest = Some((entry, rank));
            }
        }
        oldest.map(|(entry, _)| &entry.call)
    }

    /// The oldest call of `turn_id` started under `id` before any block joined it (RULING-127 (b)).
    pub fn bridge_started(
        &self,
        turn_id: Option<&str>,
        id: Option<&str>,
    ) -> Option<&OutstandingCall> {
        let turn_id = turn_id?;
        let id = id?;
        self.oldest_by_id(id, Some(turn_id), |call| {
            call.turn_id == turn_id && call.owning_message_id.is_none() && call.parent_key.is_none()
        })
    }

    /// RULING-123, C3.4: whether another call of `call`'s own message took the same `toolCallId`.
    pub fn shares_id_in_message(&self, call: &OutstandingCall) -> bool {
        let (Some(message_id), Some(tool_call_id)) = (&call.owning_message_id, &call.tool_call_id)
        else {
            return false;
        };
        !tool_call_id.is_empty() && self.shared.contains(&shared_key(message_id, tool_call_id))
    }

    pub fn of_turn(&self, turn_id: &str) -> Vec<OutstandingCall> {
        self.entries
            .values()
            .filter(|entry| entry.call.turn_id == turn_id)
            .map(|entry| entry.call.clone())
            .collect()
    }

    pub fn all(&self) -> Vec<OutstandingCall> {
        self.entries
            .values()
            .map(|entry| entry.call.clone())
            .collect()
    }

    pub fn children_of(&self, parent_key: &str) -> Vec<OutstandingCall> {
        self.children
            .get(parent_key)
            .map(|keys| {
                keys.iter()
                    .filter_map(|key| self.get(key).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn own(&self, value: &Value) -> Result<OwnedSlot, CompactError> {
        let bytes = serde_json::to_string(value)?.len();
        let reference = self.values.own(value)?;
        Ok(OwnedSlot { reference, bytes })
    }

    /// X07 `slotBytes` counts one copy per slot: the replaced value's bytes leave with it.
    pub fn replace_slot(
        &mut self,
        call: &OutstandingCall,
        field: CallSlotField,
        value: &Value,
    ) -> Result<OutstandingCall, CompactError> {
        let slot = self.own(value)?;
        let mut next = call.clone();
        match field {
            CallSlotField::Hook => next.hook = Some(slot.reference.clone()),
            CallSlotField::Progress => next.progress = Some(slot.reference.clone()),
            CallSlotField::Result => next.result = Some(slot.reference.clone()),
        }
        let reput = self.reput(next.clone());
        if let Err(err) = reput {
            let _ = self.store.adjust(slot.reference.owner, -1, 0);
            return Err(err);
        }
        let key = (call.runtime_item_key.clone(), field);
        let prev_bytes = self.slot_bytes_of.insert(key, slot.bytes).unwrap_or(0);
        self.slot_bytes = self.slot_bytes + slot.bytes - prev_bytes;
        self.store.adjust(slot.reference.owner, -1, 0)?;
        Ok(next)
    }

    /// Publishes a created or changed call at its creation ordinal and refreshes every index.
    pub fn put(
        &mut self,
        input: OutstandingCall,
        sequence: u64,
        k: usize,
    ) -> Result<(), CompactError> {
        self.put_at(input, call_order(sequence, k))
    }

    /// A changed call keeps the creation ordinal its key was put at (RULING-13 Q1).
    pub fn reput(&mut self, input: OutstandingCall) -> Result<(), CompactError> {
        let Some(order) = self
            .entries
            .get(&input.runtime_item_key)
            .map(|entry| entry.order.clone())
        else {
            return Err(CompactError::Io(format!(
                "unknown call {}",
                input.runtime_item_key
            )));
        };
        self.put_at(input, order)
    }

    fn put_at(&mut self, input: OutstandingCall, order: String) -> Result<(), CompactError> {
        let share = self.will_share(&input);
        let call = marked(input, share);
        let bytes = header_bytes(&call)?;
        let new_root = self.roots.set(&self.root, &order, Some(call.clone()))?;
        let previous = self
            .entries
            .get(&call.runtime_item_key)
            .map(|entry| entry.call.clone());
        let prev_bytes = match &previous {
            Some(prev) => {
                self.unindex(prev);
                header_bytes(prev)?
            }
            None => 0,
        };
        self.root = new_root;
        self.counters.replacements += 1;
        self.header_bytes = self.header_bytes + bytes - prev_bytes;
        let key = call.runtime_item_key.clone();
        let mark_peers =
            call.call_unknown && call.owning_message_id.is_some() && call.tool_call_id.is_some();
        let peer_filter = if mark_peers {
            Some((
                call.owning_message_id.clone().unwrap_or_default(),
                call.tool_call_id.clone().unwrap_or_default(),
            ))
        } else {
            None
        };
        self.index(&call);
        self.entries.insert(key, Entry { call, order });
        if let Some((message_id, tool_call_id)) = peer_filter {
            let peer_keys: Vec<String> = self
                .in_message(&message_id, &tool_call_id)
                .into_iter()
                .filter(|k| self.get(k).is_some_and(|peer| !peer.call_unknown))
                .collect();
            for peer_key in peer_keys {
                if let Some(peer) = self.get(&peer_key).cloned() {
                    self.reput(peer)?;
                }
            }
        }
        Ok(())
    }

    fn will_share(&self, call: &OutstandingCall) -> bool {
        let (Some(message_id), Some(tool_call_id)) = (&call.owning_message_id, &call.tool_call_id)
        else {
            return false;
        };
        if tool_call_id.is_empty() {
            return false;
        }
        if self.shared.contains(&shared_key(message_id, tool_call_id)) {
            return true;
        }
        self.in_message(message_id, tool_call_id)
            .into_iter()
            .any(|key| key != call.runtime_item_key)
    }

    pub fn remove(&mut self, key: &str) -> Result<(), CompactError> {
        let Some(entry) = self.entries.remove(key) else {
            return Ok(());
        };
        self.unindex(&entry.call);
        for field in [
            CallSlotField::Hook,
            CallSlotField::Progress,
            CallSlotField::Result,
        ] {
            if let Some(bytes) = self.slot_bytes_of.remove(&(key.to_owned(), field)) {
                self.slot_bytes = self.slot_bytes.saturating_sub(bytes);
            }
        }
        let bytes = header_bytes(&entry.call)?;
        self.header_bytes = self.header_bytes.saturating_sub(bytes);
        if let (Some(message_id), Some(tool_call_id)) =
            (&entry.call.owning_message_id, &entry.call.tool_call_id)
            && !tool_call_id.is_empty()
            && self.in_message(message_id, tool_call_id).is_empty()
        {
            self.shared.remove(&shared_key(message_id, tool_call_id));
        }
        self.root = self.roots.set(&self.root, &entry.order, None)?;
        self.counters.replacements += 1;
        Ok(())
    }

    pub fn indexed_to(&self, turn_id: &str, tool_call_id: &str) -> Option<&str> {
        self.by_turn
            .get(&turn_key(turn_id, tool_call_id))
            .map(String::as_str)
    }

    pub fn repoint(&mut self, turn_id: &str, tool_call_id: &str, key: &str) {
        self.by_turn
            .insert(turn_key(turn_id, tool_call_id), key.to_owned());
    }

    pub fn snapshot(&self) -> CallCounters {
        let carrierless = self
            .entries
            .values()
            .filter(|entry| entry.call.join == crate::compact_call_model::CallJoin::Carrierless)
            .count();
        CallCounters {
            live: self.entries.len(),
            carrierless,
            replacements: self.counters.replacements,
            header_bytes: self.header_bytes,
            slot_bytes: self.slot_bytes,
            progress_replacements: self.counters.progress_replacements,
            ignored: IgnoredCounters {
                unknown: self.counters.unknown,
                late: self.counters.late,
            },
            collisions: self.counters.collisions,
            reused: self.counters.reused,
        }
    }

    pub fn freeze(&self) -> Result<Option<ValueRef>, CompactError> {
        if let Some(reference) = &self.root.reference {
            self.store.adjust(reference.owner, 0, 1)?;
        }
        Ok(self.root.reference.clone())
    }

    pub fn release(&self, reference: Option<&ValueRef>) -> Result<(), CompactError> {
        if let Some(reference) = reference {
            self.store.adjust(reference.owner, 0, -1)?;
        }
        Ok(())
    }

    pub fn entries_of<'a>(
        &'a self,
        reference: Option<&'a ValueRef>,
        valid: &'a (dyn Fn() -> Result<(), CompactError> + Sync),
    ) -> BoxFuture<'a, Result<Vec<(String, OutstandingCall)>, CompactError>> {
        self.roots.entries(reference, valid)
    }

    fn index(&mut self, call: &OutstandingCall) {
        if let Some(tool_call_id) = call.tool_call_id.as_deref().filter(|id| !id.is_empty()) {
            let slot = turn_key(&call.turn_id, tool_call_id);
            if !self.by_turn.contains_key(&slot)
                || self.by_turn.get(&slot).map(String::as_str)
                    == Some(call.runtime_item_key.as_str())
            {
                self.by_turn.insert(slot, call.runtime_item_key.clone());
            }
            let keys = self.by_id.entry(tool_call_id.to_owned()).or_default();
            if !keys.contains(&call.runtime_item_key) {
                keys.push(call.runtime_item_key.clone());
            }
            if let Some(message_id) = &call.owning_message_id
                && call.call_unknown
            {
                self.shared.insert(shared_key(message_id, tool_call_id));
            }
        }
        if let (Some(message_id), Some(content_index)) =
            (&call.owning_message_id, call.content_index)
        {
            self.by_message.insert(
                message_key(message_id, content_index),
                call.runtime_item_key.clone(),
            );
        }
        if let Some(parent_key) = &call.parent_key {
            self.children
                .entry(parent_key.clone())
                .or_default()
                .insert(call.runtime_item_key.clone());
        }
    }

    fn in_message(&self, message_id: &str, tool_call_id: &str) -> Vec<String> {
        self.by_id
            .get(tool_call_id)
            .map(|keys| {
                keys.iter()
                    .filter(|key| {
                        self.get(key).is_some_and(|call| {
                            call.owning_message_id.as_deref() == Some(message_id)
                        })
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn unindex(&mut self, call: &OutstandingCall) {
        if let Some(tool_call_id) = call.tool_call_id.as_deref().filter(|id| !id.is_empty()) {
            let slot = turn_key(&call.turn_id, tool_call_id);
            if self.by_turn.get(&slot).map(String::as_str) == Some(call.runtime_item_key.as_str()) {
                self.by_turn.remove(&slot);
            }
            if let Some(keys) = self.by_id.get_mut(tool_call_id) {
                keys.retain(|key| key != &call.runtime_item_key);
                if keys.is_empty() {
                    self.by_id.remove(tool_call_id);
                }
            }
        }
        if let (Some(message_id), Some(content_index)) =
            (&call.owning_message_id, call.content_index)
        {
            let slot = message_key(message_id, content_index);
            if self.by_message.get(&slot).map(String::as_str)
                == Some(call.runtime_item_key.as_str())
            {
                self.by_message.remove(&slot);
            }
        }
        if let Some(parent_key) = &call.parent_key
            && let Some(siblings) = self.children.get_mut(parent_key)
        {
            siblings.remove(&call.runtime_item_key);
            if siblings.is_empty() {
                self.children.remove(parent_key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_call_model::{CallPhase, fresh_call};
    use crate::compact_content_store::CompactContentStoreOptions;
    use serde_json::json;

    #[tokio::test]
    async fn marks_shared_tool_call_ids_within_same_message_and_tracks_slots() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let mut table = CompactCallTable::new(store.clone());
        let mut c1 = fresh_call("blk:m1:0".into(), "t1".into(), CallPhase::Streaming);
        c1.owning_message_id = Some("m1".into());
        c1.content_index = Some(0);
        c1.tool_call_id = Some("dup".into());
        table.put(c1, 1, 0).expect("put c1");
        assert!(!table.get("blk:m1:0").expect("c1").call_unknown);

        let mut c2 = fresh_call("blk:m1:1".into(), "t1".into(), CallPhase::Streaming);
        c2.owning_message_id = Some("m1".into());
        c2.content_index = Some(1);
        c2.tool_call_id = Some("dup".into());
        table.put(c2, 2, 0).expect("put c2");
        assert!(table.get("blk:m1:0").expect("c1 marked").call_unknown);
        assert!(table.get("blk:m1:1").expect("c2 marked").call_unknown);

        let c1_cur = table.get("blk:m1:0").expect("c1").clone();
        let _c1_prog = table
            .replace_slot(&c1_cur, CallSlotField::Progress, &json!({"pct": 50}))
            .expect("slot");
        assert!(table.snapshot().slot_bytes > 0);

        table.remove("blk:m1:0").expect("rm c1");
        table.remove("blk:m1:1").expect("rm c2");
        assert_eq!(table.snapshot().live, 0);
        assert_eq!(table.snapshot().slot_bytes, 0);
        store.retire().await.expect("retire");
    }
}
