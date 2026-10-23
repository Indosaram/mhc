//! Port of senpi packages/coding-agent/src/core/session-history-reduce.ts (senpi#2537).
//!
//! A fold over a session's whole history, in append order. Per-turn readers that only need "the
//! latest X" or "everything since the last Y" keep only their own state: the history is read once
//! per session load, and every later call folds just the entries appended since. A reducer must be
//! a stable (`static`) object because its state is memoized per reducer address, and must not rely
//! on `parentId`: an entry kept across a compaction can arrive with its link rewritten.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;

pub struct HistoryReducer<S> {
    pub initial: fn() -> S,
    /// May update the state in place; callers treat the returned state as read-only.
    pub add: fn(&mut S, &Value),
}

pub trait HistorySource {
    /// Changes whenever the history is replaced (new session, file load, reload); never on append or mirror trim.
    fn generation(&self) -> u64;
    /// Entries in the whole history, including any a trim left only on disk.
    fn count(&self) -> usize;
    /// The entries appended after the first `count`, or `None` when the mirror cannot supply them.
    fn appended_since(&self, count: usize) -> Option<Vec<Value>>;
    /// The whole history, materialized for this one read and not kept.
    fn read_all(&self) -> Vec<Value>;
}

struct Reduction {
    generation: u64,
    count: usize,
    state: Box<dyn Any + Send>,
}

#[derive(Default)]
pub struct HistoryReductions {
    reductions: Mutex<HashMap<usize, Reduction>>,
}

impl Clone for HistoryReductions {
    /// A cloned manager has its own history, so it never shares memoized states.
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for HistoryReductions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HistoryReductions")
    }
}

impl HistoryReductions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reduce<S: Clone + Send + 'static>(
        &self,
        reducer: &'static HistoryReducer<S>,
        source: &dyn HistorySource,
    ) -> S {
        let key = std::ptr::from_ref(reducer) as usize;
        let generation = source.generation();
        let count = source.count();
        // Taking the memo out first means a fold that panics leaves none behind: the next call starts over.
        let previous = self
            .reductions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        if let Some(previous) = previous
            && previous.generation == generation
            && previous.count <= count
            && let Ok(state) = previous.state.downcast::<S>()
        {
            let mut state = *state;
            if previous.count == count {
                self.store(key, generation, count, &state);
                return state;
            }
            if let Some(appended) = source.appended_since(previous.count) {
                for entry in &appended {
                    (reducer.add)(&mut state, entry);
                }
                self.store(key, generation, count, &state);
                return state;
            }
        }
        let mut state = (reducer.initial)();
        for entry in &source.read_all() {
            (reducer.add)(&mut state, entry);
        }
        self.store(key, generation, count, &state);
        state
    }

    fn store<S: Clone + Send + 'static>(
        &self,
        key: usize,
        generation: u64,
        count: usize,
        state: &S,
    ) {
        self.reductions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                key,
                Reduction {
                    generation,
                    count,
                    state: Box::new(state.clone()),
                },
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;

    struct Fake {
        generation: Cell<u64>,
        entries: std::cell::RefCell<Vec<Value>>,
        appended_available: Cell<bool>,
        read_all_calls: Cell<usize>,
    }

    impl HistorySource for Fake {
        fn generation(&self) -> u64 {
            self.generation.get()
        }
        fn count(&self) -> usize {
            self.entries.borrow().len()
        }
        fn appended_since(&self, count: usize) -> Option<Vec<Value>> {
            self.appended_available
                .get()
                .then(|| self.entries.borrow()[count..].to_vec())
        }
        fn read_all(&self) -> Vec<Value> {
            self.read_all_calls.set(self.read_all_calls.get() + 1);
            self.entries.borrow().clone()
        }
    }

    static SUM: HistoryReducer<i64> = HistoryReducer {
        initial: || 0,
        add: |state, entry| *state += entry.get("n").and_then(Value::as_i64).unwrap_or(0),
    };

    fn fake(values: &[i64]) -> Fake {
        Fake {
            generation: Cell::new(0),
            entries: std::cell::RefCell::new(values.iter().map(|n| json!({ "n": n })).collect()),
            appended_available: Cell::new(true),
            read_all_calls: Cell::new(0),
        }
    }

    #[test]
    fn later_calls_fold_only_the_appended_entries() {
        let source = fake(&[1, 2]);
        let reductions = HistoryReductions::new();
        assert_eq!(reductions.reduce(&SUM, &source), 3);
        source.entries.borrow_mut().push(json!({ "n": 4 }));
        assert_eq!(reductions.reduce(&SUM, &source), 7);
        assert_eq!(source.read_all_calls.get(), 1);
    }

    #[test]
    fn a_new_generation_or_missing_tail_refolds_the_whole_history() {
        let source = fake(&[1, 2]);
        let reductions = HistoryReductions::new();
        reductions.reduce(&SUM, &source);
        source.generation.set(1);
        assert_eq!(reductions.reduce(&SUM, &source), 3);
        source.entries.borrow_mut().push(json!({ "n": 5 }));
        source.appended_available.set(false);
        assert_eq!(reductions.reduce(&SUM, &source), 8);
        assert_eq!(source.read_all_calls.get(), 3);
    }
}
