//! Commit-granular public projection of the isolated Pico3 session.
use std::{cell::{Cell, RefCell}, collections::{BTreeMap, VecDeque}, rc::{Rc, Weak}};
use super::{session::{CommitChanges, Session}, types::*};

pub const WATCH_CAPACITY: usize = 256;
pub type Listener = Rc<dyn Fn(&Envelope) -> Result<()>>;

pub fn apply_envelope(view: &ConversationView, envelope: &Envelope) -> Result<ConversationView> {
    decode(&maho_chord::delta::apply_immutable(Some(&encode(view)?), &envelope.ops)?)
}
pub(crate) fn encode<T: serde::Serialize>(value: &T) -> Result<JsonValue> {
    Ok(to_stored(value)?.as_json().clone())
}
pub(crate) fn decode<T: serde::de::DeserializeOwned>(value: &JsonValue) -> Result<T> {
    Ok(serde_json::from_str(&serde_json::to_string(value)?)?)
}

/// The snapshot and revision are immutable: start drains only subsequent commits.
#[derive(Clone)]
pub struct Watch { inner: Rc<WatchState> }
struct WatchState {
    view: ConversationView, revision: u64, stopped: Cell<bool>,
    listener: RefCell<Option<Listener>>, buffer: RefCell<VecDeque<Envelope>>,
    report: Rc<dyn Fn(&Error)>, on_stop: Box<dyn Fn()>,
}
impl Watch {
    pub(crate) fn new(view: ConversationView, revision: u64, report: Rc<dyn Fn(&Error)>, on_stop: impl Fn() + 'static) -> Self {
        Self { inner: Rc::new(WatchState { view, revision, stopped: Cell::new(false), listener: RefCell::new(None), buffer: RefCell::new(VecDeque::new()), report, on_stop: Box::new(on_stop) }) }
    }
    pub fn view(&self) -> &ConversationView { &self.inner.view }
    pub fn revision(&self) -> u64 { self.inner.revision }
    pub fn closed(&self) -> bool { self.inner.stopped.get() }
    pub fn start(&self, listener: Listener) {
        if self.closed() || self.inner.listener.borrow().is_some() { return; }
        *self.inner.listener.borrow_mut() = Some(listener);
        let pending = std::mem::take(&mut *self.inner.buffer.borrow_mut());
        for envelope in pending { if self.closed() { break; } self.deliver(&envelope); }
    }
    pub fn stop(&self) {
        if self.inner.stopped.replace(true) { return; }
        self.inner.buffer.borrow_mut().clear();
        self.inner.listener.borrow_mut().take();
        (self.inner.on_stop)();
    }
    pub(crate) fn accept(&self, envelope: Envelope) {
        if self.closed() { return; }
        if self.inner.listener.borrow().is_none() {
            if self.inner.buffer.borrow().len() >= WATCH_CAPACITY {
                self.fail(Error::InvalidShape(format!("watch capacity {WATCH_CAPACITY} exceeded before start()")));
            } else { self.inner.buffer.borrow_mut().push_back(envelope); }
        } else { self.deliver(&envelope); }
    }
    fn deliver(&self, envelope: &Envelope) {
        let listener = self.inner.listener.borrow().clone();
        if let Some(listener) = listener {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(envelope))) {
                Ok(Ok(())) => {}, Ok(Err(error)) => self.fail(error),
                Err(_) => self.fail(Error::InvalidShape("watch listener panicked".into())),
            }
        }
    }
    fn fail(&self, error: Error) { self.stop(); report(&self.inner.report, &error); }
}
pub(crate) fn report(callback: &Rc<dyn Fn(&Error)>, error: &Error) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(error))).is_err() {
        tracing::error!(%error, "Pico3 error reporter panicked");
    }
}

struct Record { conversation: Conversation, view: ConversationView, revision: u64, watchers: Vec<Weak<WatchState>> }
pub struct ViewManager {
    session: Rc<Session>, records: RefCell<BTreeMap<Id, Record>>,
    deliveries: RefCell<VecDeque<(Envelope, Vec<Watch>)>>, report: Rc<dyn Fn(&Error)>,
}
impl ViewManager {
    pub fn new(session: Rc<Session>, report: Rc<dyn Fn(&Error)>) -> Rc<Self> {
        Rc::new(Self { session, records: RefCell::new(BTreeMap::new()), deliveries: RefCell::new(VecDeque::new()), report })
    }
    /// Must be called inside the Session transaction that captured entries.
    pub fn watch(self: &Rc<Self>, conversation: Conversation, entries: Vec<Entry>) -> Result<Watch> {
        let id = conversation.id;
        if !self.records.borrow().contains_key(&id) {
            let view = self.build(conversation.clone(), entries)?;
            self.records.borrow_mut().insert(id, Record { conversation, view, revision: 0, watchers: Vec::new() });
        }
        let weak = Rc::downgrade(self);
        let mut records = self.records.borrow_mut();
        let record = records.get_mut(&id).ok_or_else(|| Error::InvalidShape("view record missing".into()))?;
        let watch = Watch::new(record.view.clone(), record.revision, self.report.clone(), move || {
            if let Some(manager) = weak.upgrade() {
                let mut records = manager.records.borrow_mut();
                if let Some(record) = records.get_mut(&id) {
                    record.watchers.retain(|w| w.upgrade().is_some_and(|w| !w.stopped.get()));
                    if record.watchers.is_empty() { records.remove(&id); }
                }
            }
        });
        record.watchers.push(Rc::downgrade(&watch.inner));
        Ok(watch)
    }
    /// Invoked after durable publication and index adoption, still on the line.
    pub fn update(&self, changes: &CommitChanges) -> Result<()> {
        let ids: Vec<_> = self.records.borrow().keys().copied().collect();
        for id in ids {
            let outcome = (|| -> Result<()> {
                let mut records = self.records.borrow_mut();
                let Some(record) = records.get_mut(&id) else { return Ok(()); };
                let events: Vec<_> = changes.events.iter().filter(|(c, _)| *c == id).map(|(_, event)| event.clone()).collect();
                let appended: Vec<_> = changes.entries.iter().filter(|e| e.conversation_id == id).cloned().collect();
                let relevant = !appended.is_empty() || !events.is_empty() || changes.tasks.iter().any(|t| t.conversation_id == id)
                    || changes.docs.iter().any(|(r, _)| matches!(r, DocRef::Session) || matches!(r, DocRef::Rewindable { conversation_id } | DocRef::Sticky { conversation_id } if *conversation_id == id));
                if !relevant { return Ok(()); }
                let mut entries = record.view.entries.clone();
                apply_entries(&mut entries, &appended);
                let next = self.build(record.conversation.clone(), entries)?;
                let ops = maho_chord::delta::diff_revisions(&encode(&record.view)?, &encode(&next)?);
                if ops.is_empty() && events.is_empty() { return Ok(()); }
                record.revision = record.revision.checked_add(1).ok_or_else(|| Error::InvalidShape("view revision exhausted".into()))?;
                record.view = next;
                let watchers = record.watchers.iter().filter_map(Weak::upgrade).map(|inner| Watch { inner }).collect();
                self.deliveries.borrow_mut().push_back((Envelope { revision: record.revision, ops, events }, watchers));
                Ok(())
            })();
            if let Err(error) = outcome {
                let record = self.records.borrow_mut().remove(&id);
                if let Some(record) = record { for inner in record.watchers.into_iter().filter_map(|w| w.upgrade()) { Watch { inner }.stop(); } }
                report(&self.report, &error);
            }
        }
        Ok(())
    }
    /// No user callback runs while Session owns its line.
    pub fn deliver(&self) {
        let pending = std::mem::take(&mut *self.deliveries.borrow_mut());
        for (envelope, watchers) in pending { for watcher in watchers { watcher.accept(envelope.clone()); } }
    }
    pub fn close(&self) {
        self.deliveries.borrow_mut().clear();
        let records = std::mem::take(&mut *self.records.borrow_mut());
        for record in records.into_values() { for inner in record.watchers.into_iter().filter_map(|w| w.upgrade()) { Watch { inner }.stop(); } }
    }
    fn document<T: serde::de::DeserializeOwned>(&self, reference: DocRef) -> Result<T> {
        decode(&encode(&self.session.loaded_document(&reference)?)?)
    }
    fn build(&self, conversation: Conversation, entries: Vec<Entry>) -> Result<ConversationView> {
        let id = conversation.id;
        let rewindable: RewindableState = self.document(DocRef::Rewindable { conversation_id: id })?;
        let sticky: StickyState = self.document(DocRef::Sticky { conversation_id: id })?;
        let session: SessionState = self.document(DocRef::Session)?;
        let raw_r: JsonObject = decode(&encode(&rewindable)?)?;
        let raw_s: JsonObject = decode(&encode(&sticky)?)?;
        let defaults = self.session.defaults.borrow();
        let mut config = JsonObject::new();
        for (key, route) in &defaults.route {
            let (source, fallback) = match route { DocKind::Rewindable => (&raw_r, &defaults.rewindable), DocKind::Sticky => (&raw_s, &defaults.sticky), DocKind::Session => continue };
            if let Some(value) = source.get(key).or_else(|| fallback.get(key)) { config.insert(key.clone().into(), value.clone()); }
        }
        let live: Vec<_> = self.session.live_tasks.borrow().values().filter(|t| t.conversation_id == id).cloned().collect();
        let kinds = self.session.kinds.borrow();
        let generation = live.iter().find(|t| t.kind == "pi.generation");
        let turn_task = generation.or_else(|| live.iter().find(|t| t.kind == "pi.post_tools"));
        let has_turn = live.iter().any(|t| kinds.get(&t.kind).is_some_and(|k| k.turn));
        let turn = if has_turn {
            let inputs = turn_task.map(|t| input_ids(&t.input)).unwrap_or_default();
            Some(TurnView { inputs, generation: generation.map(|t| generation_status(t, sticky.turn.message.is_some())), message: sticky.turn.message.clone(), tools: sticky.turn.tools.clone().into_iter().map(ToolSlotView::from).collect() })
        } else { None };
        let compaction = live.iter().find(|t| t.kind == "pi.collapse").map(compaction_view).transpose()?;
        let mut tasks = BTreeMap::new();
        for task in &live {
            let Some(kind) = kinds.get(&task.kind) else { continue; };
            if kind.turn || task.kind == "pi.collapse" { continue; }
            let mut slot = sticky.tasks.get(&task.id.to_string()).cloned();
            if let Some(slot) = &mut slot { slot.remove("memos"); }
            let status = match &kind.describe {
                Some(describe) => describe(task, slot.as_ref())?,
                None => JsonValue::object(vec![("phase".into(), JsonValue::String(task.checkpoint.as_ref().map(|c| c.phase.as_str()).unwrap_or(if task.status == TaskStatus::Pending { "pending" } else { "running" }).into()))]),
            };
            tasks.insert(task.id.to_string(), TaskView { kind: task.kind.clone(), background: task.background, marked: task.abort, status });
        }
        let mut plugins = BTreeMap::new();
        for (name, registration) in self.session.namespaces.borrow().iter() {
            if !registration.token.is_registered() { continue; }
            let Some(project) = &registration.project else { continue; };
            let mut merged = JsonObject::new();
            for source in [&registration.defaults.rewindable, &registration.defaults.sticky, &registration.defaults.session].into_iter().chain([rewindable.plugins.get(name), sticky.plugins.get(name), session.plugins.get(name)].into_iter().flatten()) {
                for (key, value) in source.iter() { merged.insert(key.clone(), value.clone()); }
            }
            plugins.insert(name.clone(), project(&merged));
        }
        Ok(ConversationView { conversation: conversation.into(), entries, config, inbox: sticky.inbox, turn, compaction, tasks, plugins })
    }
}
fn apply_entries(entries: &mut Vec<Entry>, appended: &[Entry]) {
    for entry in appended {
        if let Some(head) = entry.head { let retain = entries.iter().position(|e| e.id >= head).unwrap_or(entries.len()); entries.drain(..retain); }
        entries.push(entry.clone());
    }
}
fn input_ids(input: &JsonValue) -> Vec<Id> {
    let JsonValue::Object(input) = input else { return Vec::new(); };
    match input.iter().find(|(k, _)| k.as_str() == Some("inputs")).map(|(_, v)| v) {
        Some(JsonValue::Array(ids)) => ids.iter().filter_map(|id| match id { JsonValue::Number(n) => n.as_u64(), _ => None }).collect(), _ => Vec::new(),
    }
}
fn number(fields: Option<&JsonObject>, key: &str, fallback: u64) -> u64 {
    fields.and_then(|f| f.get(key)).and_then(|v| match v { JsonValue::Number(n) => n.as_u64(), _ => None }).unwrap_or(fallback)
}
fn generation_status(task: &Task, streaming: bool) -> GenerationStatus {
    if task.status == TaskStatus::Pending && !task.after.is_empty() { return GenerationStatus::Waiting { on: CompactionWait::Compaction }; }
    let fields = task.checkpoint.as_ref().map(|c| &c.fields);
    let attempt = u32::try_from(number(fields, "attempt", 1)).unwrap_or(u32::MAX);
    match task.checkpoint.as_ref().map(|c| c.phase.as_str()) {
        Some("requesting") => if streaming { GenerationStatus::Streaming { attempt } } else { GenerationStatus::Requesting { attempt } },
        Some("retrying") => GenerationStatus::Retrying { attempt, retry_at: number(fields, "untilMs", 0), last_error: fields.and_then(|f| f.get("lastError")).and_then(|v| match v { JsonValue::String(s) => s.as_str(), _ => None }).unwrap_or("").into() },
        Some("deferred") => GenerationStatus::Deferred { attempt, poll_at: number(fields, "pollAt", 0) },
        _ => GenerationStatus::Preparing,
    }
}
fn compaction_view(task: &Task) -> Result<CompactionView> {
    #[derive(serde::Deserialize)] struct Input { reason: CompactionReason }
    let input: Input = decode(&task.input)?;
    let fields = task.checkpoint.as_ref().map(|c| &c.fields);
    let retrying = task.checkpoint.as_ref().is_some_and(|c| c.phase == "retrying");
    Ok(CompactionView { task_id: task.id, reason: input.reason, stage: if retrying { CompactionStage::Retrying } else { CompactionStage::Summarizing }, attempt: u32::try_from(number(fields, "attempt", 1)).unwrap_or(u32::MAX), retry_at: if retrying { Some(number(fields, "untilMs", 0)) } else { None } })
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
