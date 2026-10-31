//! Port of senpi packages/ai/src/utils/process-singleton.ts.
//!
//! Returns the one value this process keeps under `key`, creating it on first use. senpi needs
//! this because its release bundle can carry several copies of one module; a Rust binary links each
//! module once, so the keyed process-wide registry keeps the same contract (one value per key and
//! type for the life of the process) without the bundle concern.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

static SLOTS: LazyLock<Mutex<HashMap<String, Arc<dyn Any + Send + Sync>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `processSingleton(key, create)`. A key already holding a value of another type is a programming
/// error, like a JS caller reading a slot with the wrong shape.
pub fn process_singleton<T: Any + Send + Sync>(key: &str, create: impl FnOnce() -> T) -> Arc<T> {
    let mut slots = SLOTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let slot = slots
        .entry(key.to_owned())
        .or_insert_with(|| Arc::new(create()) as Arc<dyn Any + Send + Sync>)
        .clone();
    slot.downcast::<T>()
        .unwrap_or_else(|_| panic!("process singleton {key} holds a value of another type"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_once_and_returns_the_same_value_for_the_key() {
        let first = process_singleton("maho-ai.test.process-singleton", || Mutex::new(1_u32));
        *first.lock().unwrap() += 1;
        let second = process_singleton("maho-ai.test.process-singleton", || Mutex::new(100_u32));
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(*second.lock().unwrap(), 2);
    }
}
