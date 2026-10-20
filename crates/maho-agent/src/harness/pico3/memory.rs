//! Reference storage and the staged read model used by JSONL storage.
use std::{cell::RefCell, collections::{BTreeMap, BTreeSet}};
use maho_chord::delta::{apply_immutable, is_base, Op};
use crate::harness::context::Context;
use super::types::*;

pub(crate) const MAX_SAFE_ID: Id = 9_007_199_254_740_991;
pub(crate) fn storage_error(message: impl Into<String>) -> Error { Error::Storage(message.into()) }

#[derive(Clone, Default)]
pub(crate) struct State {
    pub conversations: BTreeMap<Id, Conversation>,
    pub entries: BTreeMap<Id, Entry>,
    pub own_entries: BTreeMap<Id, Vec<Id>>,
    pub tasks: BTreeMap<Id, Task>,
    pub task_order: Vec<Id>,
    pub conversation_order: Vec<Id>,
    pub inputs: BTreeMap<Id, Input>,
    pub requests: BTreeMap<(Id, String), Input>,
    pub rewindable: BTreeMap<Id, Vec<(Seq, Vec<Op>)>>,
    pub sticky: BTreeMap<Id, Vec<Vec<Op>>>,
    pub session: Option<JsonObject>,
    pub entry_seq: BTreeMap<Id, Seq>,
    pub next_id: Id,
    pub seq: Seq,
    pub closed: bool,
}

pub struct MemoryStorage { pub(crate) state: RefCell<State> }
impl Default for MemoryStorage { fn default() -> Self { Self::new() } }

impl MemoryStorage {
    pub fn new() -> Self {
        Self { state: RefCell::new(State { next_id: 1, ..State::default() }) }
    }

    pub(crate) fn stage(&self, writes: &[Write], seq: Seq) -> Result<State> {
        let current = self.state.borrow();
        if current.closed { return Err(storage_error("storage closed")); }
        let mut staged = current.clone();
        let mut created = BTreeSet::new();
        let mut max_id = 0;
        for write in writes {
            let claim = match write {
                Write::Conversation { conversation } => {
                    if current.conversations.contains_key(&conversation.id) { return Err(storage_error("conversation exists")); }
                    Some(conversation.id)
                }
                Write::Entry { entry } => {
                    if current.entries.contains_key(&entry.id) { return Err(storage_error("entry exists")); }
                    Some(entry.id)
                }
                Write::Task { task } => {
                    if current.tasks.contains_key(&task.id) { return Err(storage_error("task exists")); }
                    Some(task.id)
                }
                Write::Input { input } => (!current.inputs.contains_key(&input.id)).then_some(input.id),
                Write::TaskPatch { .. } | Write::Doc { .. } => None,
            };
            if let Some(id) = claim {
                if id == 0 || id > MAX_SAFE_ID { return Err(storage_error(format!("invalid id {id}"))); }
                if !created.insert(id) { return Err(storage_error(format!("id {id} created twice in one batch"))); }
                max_id = max_id.max(id);
            }
        }
        for write in writes {
            match write {
                Write::Conversation { conversation } => {
                    staged.conversation_order.push(conversation.id);
                    staged.conversations.insert(conversation.id, conversation.clone());
                    staged.own_entries.insert(conversation.id, Vec::new());
                }
                Write::Entry { entry } => {
                    staged.entries.insert(entry.id, entry.clone());
                    staged.own_entries.entry(entry.conversation_id).or_default().push(entry.id);
                    staged.entry_seq.insert(entry.id, seq);
                }
                Write::Task { task } => {
                    staged.task_order.push(task.id);
                    staged.tasks.insert(task.id, task.clone());
                }
                Write::TaskPatch { patch } => {
                    let task = staged.tasks.get(&patch.id).or_else(|| writes.iter().find_map(|w| match w {
                        Write::Task { task } if task.id == patch.id => Some(task),
                        _ => None,
                    })).ok_or_else(|| storage_error(format!("patch for unknown task {}", patch.id)))?;
                    // Merge at the lossless JSON boundary; absent and explicit-null checkpoint differ.
                    let mut value: JsonObject = serde_json::from_str(&serde_json::to_string(task)?)?;
                    let fields: JsonObject = serde_json::from_str(&serde_json::to_string(patch)?)?;
                    for (key, field) in fields.into_inner() {
                        if key.as_str() == Some("id") { continue; }
                        if key.as_str() == Some("checkpoint") && field == JsonValue::Null { value.remove("checkpoint"); }
                        else { value.insert(key, field); }
                    }
                    staged.tasks.insert(patch.id, serde_json::from_str(&serde_json::to_string(&value)?)?);
                }
                Write::Input { input } => {
                    staged.inputs.insert(input.id, input.clone());
                    if let Some(request) = &input.request_id {
                        staged.requests.insert((input.conversation_id, request.clone()), input.clone());
                    }
                }
                Write::Doc { r#ref, ops } => match r#ref {
                    DocRef::Session => {
                        let base = staged.session.clone().unwrap_or_else(session_default);
                        staged.session = Some(apply_object(&base, ops)?);
                    }
                    DocRef::Rewindable { conversation_id } => {
                        let log = staged.rewindable.entry(*conversation_id).or_default();
                        let base = fold(log.iter().map(|(_, ops)| ops.as_slice()))?;
                        apply_object(&base, ops)?;
                        log.push((seq, ops.clone()));
                    }
                    DocRef::Sticky { conversation_id } => {
                        let log = staged.sticky.entry(*conversation_id).or_default();
                        let base = fold(log.iter().map(Vec::as_slice))?;
                        apply_object(&base, ops)?;
                        log.push(ops.clone());
                    }
                },
            }
        }
        staged.next_id = staged.next_id.max(max_id.saturating_add(1)).max(1);
        staged.seq = seq;
        Ok(staged)
    }

    pub(crate) fn publish(&self, staged: State) { self.state.replace(staged); }

    pub(crate) fn scan(&self, scan: &EntryScan) -> Vec<Entry> {
        let state = self.state.borrow();
        let mut out = Vec::new();
        let mut conversation = Some(scan.conversation_id);
        let mut cap = scan.before;
        let mut visited = BTreeSet::new();
        while let Some(id) = conversation {
            if out.len() >= scan.limit || !visited.insert(id) { break; }
            if let Some(own) = state.own_entries.get(&id) {
                for entry_id in own.iter().rev() {
                    if out.len() >= scan.limit { break; }
                    if let Some(entry) = state.entries.get(entry_id) {
                        if cap.is_some_and(|before| entry.id >= before)
                            || scan.kind.as_ref().is_some_and(|kind| kind != &entry.kind)
                            || (scan.with_head == Some(true) && entry.head.is_none()) { continue; }
                        out.push(entry.clone());
                    }
                }
            }
            let parent = state.conversations.get(&id).and_then(|c| c.parent.as_ref());
            conversation = parent.map(|p| p.conversation_id);
            cap = parent.map(|p| cap.unwrap_or(Id::MAX).min(p.at.saturating_add(1)));
        }
        out
    }
}

fn session_default() -> JsonObject {
    let mut object = JsonObject::new();
    object.insert("plugins".into(), JsonValue::object(Vec::new()));
    object
}
fn apply_object(base: &JsonObject, ops: &[Op]) -> Result<JsonObject> {
    let value = JsonValue::object(base.clone().into_inner());
    let result = apply_immutable(Some(&value), ops)?;
    Ok(serde_json::from_str(&serde_json::to_string(&result)?)?)
}
pub(crate) fn fold<'a>(logs: impl IntoIterator<Item = &'a [Op]>) -> Result<JsonObject> {
    let logs: Vec<_> = logs.into_iter().collect();
    let start = logs.iter().rposition(|ops| is_base(ops)).unwrap_or(0);
    let mut value = JsonObject::new();
    for ops in logs.into_iter().skip(start) { value = apply_object(&value, ops)?; }
    Ok(value)
}

impl Storage for MemoryStorage {
    fn mint_id(&self) -> Result<Id> {
        let mut state = self.state.borrow_mut();
        let id = state.next_id.max(1);
        if id > MAX_SAFE_ID { return Err(storage_error("id space exhausted")); }
        state.next_id = id.saturating_add(1);
        Ok(id)
    }
    fn commit<'a>(&'a self, writes: &'a [Write], _ctx: &'a Context) -> LocalBoxFuture<'a, Result<Seq>> {
        Box::pin(async move {
            let seq = self.state.borrow().seq.checked_add(1).filter(|seq| *seq <= MAX_SAFE_ID).ok_or_else(|| storage_error("sequence exhausted"))?;
            let staged = self.stage(writes, seq)?;
            self.publish(staged);
            Ok(seq)
        })
    }
    fn conversation<'a>(&'a self, id: Id, _ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<Conversation>>> { Box::pin(async move { Ok(self.state.borrow().conversations.get(&id).cloned()) }) }
    fn conversations<'a>(&'a self, _ctx: &'a Context) -> LocalBoxFuture<'a, Result<Vec<Conversation>>> { Box::pin(async move { let s = self.state.borrow(); Ok(s.conversation_order.iter().filter_map(|id| s.conversations.get(id).cloned()).collect()) }) }
    fn entries<'a>(&'a self, ids: &'a [Id], _ctx: &'a Context) -> LocalBoxFuture<'a, Result<BTreeMap<Id, Entry>>> { Box::pin(async move { let s = self.state.borrow(); Ok(ids.iter().filter_map(|id| s.entries.get(id).map(|e| (*id, e.clone()))).collect()) }) }
    fn scan_entries<'a>(&'a self, scan: EntryScan, _ctx: &'a Context) -> LocalBoxFuture<'a, Result<Vec<Entry>>> { Box::pin(async move { Ok(self.scan(&scan)) }) }
    fn task<'a>(&'a self, id: Id, _ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<Task>>> { Box::pin(async move { Ok(self.state.borrow().tasks.get(&id).cloned()) }) }
    fn scan_tasks<'a>(&'a self, scan: TaskScan, _ctx: &'a Context) -> LocalBoxFuture<'a, Result<Vec<Task>>> { Box::pin(async move {
        let state = self.state.borrow();
        Ok(state.task_order.iter().filter_map(|id| state.tasks.get(id)).filter(|t| {
            scan.conversation_id.is_none_or(|id| id == t.conversation_id)
                && scan.kind.as_ref().is_none_or(|kind| kind == &t.kind)
                && scan.status.as_ref().is_none_or(|statuses| statuses.contains(&t.status))
        }).cloned().collect())
    }) }
    fn input<'a>(&'a self, id: Id, _ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<Input>>> { Box::pin(async move { Ok(self.state.borrow().inputs.get(&id).cloned()) }) }
    fn input_by_request<'a>(&'a self, id: Id, request: &'a str, _ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<Input>>> { Box::pin(async move { Ok(self.state.borrow().requests.get(&(id, request.to_owned())).cloned()) }) }
    fn doc<'a>(&'a self, reference: DocRef, _ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<JsonObject>>> { Box::pin(async move {
        let state = self.state.borrow();
        match reference {
            DocRef::Session => Ok(Some(state.session.clone().unwrap_or_else(session_default))),
            DocRef::Rewindable { conversation_id } => state.rewindable.get(&conversation_id).map(|log| fold(log.iter().map(|(_, ops)| ops.as_slice()))).transpose(),
            DocRef::Sticky { conversation_id } => state.sticky.get(&conversation_id).map(|log| fold(log.iter().map(Vec::as_slice))).transpose(),
        }
    }) }
    fn doc_as_of<'a>(&'a self, conversation_id: Id, at: Id, _ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<JsonObject>>> { Box::pin(async move {
        let state = self.state.borrow();
        let Some(owner) = state.entries.get(&at).map(|e| e.conversation_id) else { return Ok(None); };
        let mut cursor = Some(conversation_id);
        let mut seen = BTreeSet::new();
        while let Some(id) = cursor {
            if !seen.insert(id) { return Ok(None); }
            let Some(conversation) = state.conversations.get(&id) else { return Ok(None); };
            if id == owner {
                let seq = state.entry_seq.get(&at).copied().unwrap_or(0);
                let logs: Vec<_> = state.rewindable.get(&owner).into_iter().flatten().filter(|(s, _)| *s <= seq).map(|(_, ops)| ops.as_slice()).collect();
                return if logs.is_empty() { Ok(None) } else { fold(logs).map(Some) };
            }
            cursor = conversation.parent.as_ref().map(|p| p.conversation_id);
        }
        Ok(None)
    }) }
    fn truncate<'a>(&'a self, reference: DocRef, _ctx: &'a Context) -> LocalBoxFuture<'a, Result<()>> { Box::pin(async move {
        if let DocRef::Sticky { conversation_id } = reference {
            if let Some(log) = self.state.borrow_mut().sticky.get_mut(&conversation_id) {
                let start = log.iter().rposition(|ops| is_base(ops)).unwrap_or(0);
                log.drain(..start);
            }
        }
        Ok(())
    }) }
    fn close<'a>(&'a self, _ctx: &'a Context) -> LocalBoxFuture<'a, Result<()>> { Box::pin(async move { self.state.borrow_mut().closed = true; Ok(()) }) }
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
