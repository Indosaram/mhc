use crate::{
    delta::Op,
    types::{ContextRef, JsonValue, ServiceError, ServiceSequence},
};
use std::sync::{Arc, LazyLock, Mutex, Weak};

pub type SourceListener =
    Arc<dyn Fn(&[Op], ServiceSequence, ContextRef) -> Result<(), ServiceError> + Send + Sync>;
pub type Unsubscribe = Box<dyn FnOnce() + Send>;
pub trait ReplicatedStateInternals: Send + Sync {
    fn snapshot(&self) -> (Arc<JsonValue>, ServiceSequence);
    fn subscribe_source(&self, listener: SourceListener) -> Unsubscribe;
}
type Entry = (
    Weak<dyn ReplicatedStateInternals>,
    Weak<dyn ReplicatedStateInternals>,
);
static SOURCES: LazyLock<Mutex<Vec<Entry>>> = LazyLock::new(|| Mutex::new(Vec::new()));
pub fn register_replicated_state_internals(
    value: &Arc<dyn ReplicatedStateInternals>,
    internals: &Arc<dyn ReplicatedStateInternals>,
) {
    let mut entries = SOURCES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    entries.retain(|(key, _)| key.strong_count() != 0);
    let key = Arc::downgrade(value);
    if let Some(entry) = entries
        .iter_mut()
        .find(|(existing, _)| existing.ptr_eq(&key))
    {
        entry.1 = Arc::downgrade(internals);
    } else {
        entries.push((key, Arc::downgrade(internals)));
    }
}
pub fn get_replicated_state_internals(
    value: &Arc<dyn ReplicatedStateInternals>,
) -> Option<Arc<dyn ReplicatedStateInternals>> {
    let key = Arc::downgrade(value);
    SOURCES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|(existing, _)| existing.ptr_eq(&key))
        .and_then(|(_, value)| value.upgrade())
}
