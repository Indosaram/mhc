use std::{cell::{Cell, RefCell}, collections::VecDeque, rc::{Rc, Weak}, sync::Arc};
use maho_chord::{delta::{Draft, Op, Seg}, types::{MutableReplicatedState, ReplicatedState, Service, ServiceError}};
use super::{harness::{ConversationHandle, Harness}, types::*, view::{self, Watch, WATCH_CAPACITY}};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PublishedCommit { pub events: Vec<ViewEvent> }
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PublishedConversationView {
    #[serde(flatten)] pub view: ConversationView,
    pub commit: PublishedCommit,
}
pub fn pico_harness_service() -> std::result::Result<Service<Harness>, ServiceError> {
    maho_chord::api::define_service("pi.harness", true)
}
pub fn pico_conversation_service() -> std::result::Result<Service<PicoConversationService>, ServiceError> {
    maho_chord::api::define_service("pi.conversation", false)
}
pub struct PicoConversationService {
    harness: Rc<Harness>, conversation: ConversationHandle,
    pub view: Arc<dyn ReplicatedState<JsonValue>>,
}
pub fn create_pico_conversation_service(harness: Rc<Harness>, conversation: ConversationHandle, view: Arc<dyn ReplicatedState<JsonValue>>) -> PicoConversationService {
    PicoConversationService { harness, conversation, view }
}
impl PicoConversationService {
    pub async fn send(&self, input: SendInput, ctx: &Context) -> Result<Id> { Ok(self.conversation.send(input, ctx).await?.id) }
    pub async fn write(&self, entry: NewEntry, ctx: &Context) -> Result<Id> { self.conversation.write(entry, ctx).await }
    pub async fn input_abort(&self, id: Id, ctx: &Context) -> Result<&'static str> { self.harness.abort_input(id, Some(self.conversation.id), ctx).await }
    pub async fn config_set(&self, patch: JsonObject, ctx: &Context) -> Result<()> { self.conversation.config_set(patch, ctx).await }
    pub async fn abort(&self, ctx: &Context) -> Result<()> { self.conversation.abort(ctx).await }
    pub async fn reset(&self, handoff: Option<String>, ctx: &Context) -> Result<()> { self.conversation.reset(handoff, ctx).await }
    pub async fn collapse(&self, instructions: Option<String>, ctx: &Context) -> Result<Id> { self.conversation.collapse(instructions, ctx).await }
    pub async fn fork(&self, at: ForkAt, spec: ConversationSpec, ctx: &Context) -> Result<Id> { Ok(self.conversation.fork(at, spec, ctx).await?.id) }
    pub async fn entries(&self, mut scan: EntryScan, ctx: &Context) -> Result<Vec<Entry>> { scan.conversation_id = self.conversation.id; self.harness.entries(scan, ctx).await }
}
pub struct ChordViewBridgeOptions {
    pub capacity: usize, pub on_failure: Rc<dyn Fn(&Error)>,
}
impl Default for ChordViewBridgeOptions {
    fn default() -> Self { Self { capacity: WATCH_CAPACITY, on_failure: Rc::new(|error| tracing::error!(%error, "Pico-to-Chord bridge")) } }
}
pub struct ChordViewBridge {
    pub view: Arc<dyn MutableReplicatedState>, watch: Watch,
    queue: RefCell<VecDeque<Envelope>>, scheduled: Cell<bool>, closed: Cell<bool>,
    options: ChordViewBridgeOptions, ctx: Context, myself: Weak<Self>,
}
pub async fn attach_chord_view(conversation: &ConversationHandle, ctx: &Context, options: ChordViewBridgeOptions) -> Result<Rc<ChordViewBridge>> {
    attach_chord_view_with(conversation, |initial| {
        maho_chord::services::state::MutableReplicatedStateImpl::new(initial)
            .map(|state| state as Arc<dyn MutableReplicatedState>)
            .map_err(Error::Chord)
    }, ctx, options).await
}
pub async fn attach_chord_view_with(conversation: &ConversationHandle, create_state: impl FnOnce(JsonValue) -> Result<Arc<dyn MutableReplicatedState>>, ctx: &Context, options: ChordViewBridgeOptions) -> Result<Rc<ChordViewBridge>> {
    if options.capacity == 0 { return Err(Error::InvalidShape("Chord view queue capacity must be positive".into())); }
    let watch = conversation.watch(ctx).await?;
    let initial = view::encode(&PublishedConversationView { view: watch.view().clone(), commit: PublishedCommit { events: Vec::new() } })?;
    let state = match create_state(initial) { Ok(state) => state, Err(error) => { watch.stop(); return Err(error); } };
    Ok(bridge_watch(watch, state, ctx.clone(), options))
}
fn bridge_watch(watch: Watch, state: Arc<dyn MutableReplicatedState>, ctx: Context, options: ChordViewBridgeOptions) -> Rc<ChordViewBridge> {
    let bridge = Rc::new_cyclic(|myself| ChordViewBridge { view: state, watch: watch.clone(), queue: RefCell::new(VecDeque::new()), scheduled: Cell::new(false), closed: Cell::new(false), options, ctx, myself: myself.clone() });
    let weak = Rc::downgrade(&bridge);
    watch.start(Rc::new(move |envelope| {
        let Some(bridge) = weak.upgrade() else { return Ok(()); };
        if bridge.closed() { return Ok(()); }
        if bridge.queue.borrow().len() >= bridge.options.capacity {
            bridge.fail(Error::InvalidShape(format!("Pico-to-Chord view queue exceeded {} envelopes", bridge.options.capacity)));
            return Ok(());
        }
        bridge.queue.borrow_mut().push_back(envelope.clone());
        if !bridge.scheduled.replace(true) {
            let weak = bridge.myself.clone();
            tokio::task::spawn_local(async move { if let Some(bridge) = weak.upgrade() { bridge.drain(); } });
        }
        Ok(())
    }));
    bridge
}
impl ChordViewBridge {
    pub fn closed(&self) -> bool { self.closed.get() }
    pub fn close(&self) {
        if self.closed.replace(true) { return; }
        self.queue.borrow_mut().clear(); self.watch.stop();
    }
    fn fail(&self, error: Error) { if !self.closed() { self.close(); view::report(&self.options.on_failure, &error); } }
    fn drain(&self) {
        self.scheduled.set(false);
        loop {
            if self.closed() { break; }
            let Some(envelope) = self.queue.borrow_mut().pop_front() else { break; };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.view.change(self.ctx.as_chord(), &mut |change| {
                let root = change.state()?;
                apply_tracked(&root, &envelope.ops)?;
                root.at(&["commit".into()])?.set("events", &view::encode(&envelope.events).map_err(|e| Box::new(e) as ServiceError)?)?;
                Ok(())
            })));
            match result {
                Ok(Ok(())) => {},
                Ok(Err(error)) => self.fail(Error::Chord(error)),
                Err(_) => self.fail(Error::InvalidShape("Chord publication panicked".into())),
            }
        }
    }
}
impl Drop for ChordViewBridge { fn drop(&mut self) { self.watch.stop(); } }
fn apply_tracked(root: &Draft, ops: &[Op]) -> std::result::Result<(), ServiceError> {
    for op in ops {
        match op {
            Op::Replace(_) => return Err(Box::new(Error::InvalidShape("live Pico envelope unexpectedly replaced view root".into()))),
            Op::Splice(path, start, count, values) => {
                let start = isize::try_from(*start).map_err(|e| Box::new(e) as ServiceError)?;
                root.at(path)?.splice(start, *count, values)?;
            }
            Op::Permute(path, order) => {
                let target = root.at(path)?;
                let JsonValue::Array(previous) = target.value()? else { return Err(Box::new(Error::InvalidShape("permutation target is not array".into()))); };
                if previous.len() != order.len() { return Err(Box::new(Error::InvalidShape("permutation length mismatch".into()))); }
                let mut seen = vec![false; order.len()]; let mut next = Vec::with_capacity(order.len());
                for index in order {
                    let Some(mark) = seen.get_mut(*index) else { return Err(Box::new(Error::InvalidShape("permutation index out of range".into()))); };
                    if std::mem::replace(mark, true) { return Err(Box::new(Error::InvalidShape("duplicate permutation index".into()))); }
                    next.push(previous.get(*index).ok_or_else(|| Box::new(Error::InvalidShape("permutation missing value".into())) as ServiceError)?.clone());
                }
                target.splice(0, previous.len(), &next)?;
            }
            Op::Set(path, value) => { let (key, parent) = parent(root, path)?; parent.set(key, value)?; }
            Op::Delete(path) => {
                let (key, parent) = parent(root, path)?;
                match (parent.value()?, key) {
                    (JsonValue::Array(_), Seg::Index(index)) => { parent.splice(isize::try_from(index).map_err(|e| Box::new(e) as ServiceError)?, 1, &[])?; },
                    (_, key) => parent.delete(key)?,
                }
            }
            Op::Append(path, suffix) => {
                let (key, parent) = parent(root, path)?;
                let JsonValue::String(current) = parent.at(std::slice::from_ref(&key))?.value()? else { return Err(Box::new(Error::InvalidShape("append target is not string".into()))); };
                parent.set(key, &JsonValue::String(current.concat(suffix)))?;
            }
            Op::Truncate(path, count) => {
                let (key, parent) = parent(root, path)?;
                let JsonValue::String(current) = parent.at(std::slice::from_ref(&key))?.value()? else { return Err(Box::new(Error::InvalidShape("truncate target is not string".into()))); };
                parent.set(key, &JsonValue::String(current.slice(*count)))?;
            }
        }
    }
    Ok(())
}
fn parent(root: &Draft, path: &[Seg]) -> std::result::Result<(Seg, Draft), ServiceError> {
    let Some((last, prefix)) = path.split_last() else { return Err(Box::new(Error::InvalidShape("empty live operation path".into()))); };
    Ok((last.clone(), root.at(prefix)?))
}

#[cfg(test)]
#[path = "chord_tests.rs"]
mod tests;
