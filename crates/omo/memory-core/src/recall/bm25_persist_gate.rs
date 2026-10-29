//! The exclusive gate over a repository's persisted recall terms, for work that replaces the
//! repository (a backup restore) (pin `recall/bm25-persist-gate.ts`). While a repository is excluded
//! no term-store save of this process starts or lands in it, and saves already writing finish before
//! the work runs; saves requested meanwhile are dropped, because their terms describe the tree being
//! replaced and the next build saves again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

/// Identifier of the pin's process-wide gate state (`omo.memory-core.recall-term-save-gate`); a Rust
/// process has exactly one statically allocated state.
pub const RECALL_TERM_SAVE_GATE_KEY: &str = "omo.memory-core.recall-term-save-gate";

#[derive(Default)]
struct GateState {
    excluded: HashMap<PathBuf, usize>,
    writing: HashMap<PathBuf, usize>,
}

static STATE: Mutex<Option<GateState>> = Mutex::new(None);
static SETTLED: Condvar = Condvar::new();

fn state() -> MutexGuard<'static, Option<GateState>> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn key(dir: &Path) -> PathBuf {
    std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf())
}

pub fn is_recall_term_save_excluded(dir: &Path) -> bool {
    state()
        .as_ref()
        .is_some_and(|state| state.excluded.contains_key(&key(dir)))
}

/// An in-flight save; dropping it marks the save finished.
pub struct RecallTermSaveGuard {
    key: PathBuf,
}

impl Drop for RecallTermSaveGuard {
    fn drop(&mut self) {
        let mut guard = state();
        if let Some(state) = guard.as_mut()
            && let Some(count) = state.writing.get_mut(&self.key)
        {
            *count -= 1;
            if *count == 0 {
                state.writing.remove(&self.key);
            }
        }
        SETTLED.notify_all();
    }
}

/// Registers a save writing into `dir` until the returned guard drops.
pub fn track_recall_term_save(dir: &Path) -> RecallTermSaveGuard {
    let key = key(dir);
    *state()
        .get_or_insert_with(GateState::default)
        .writing
        .entry(key.clone())
        .or_insert(0) += 1;
    RecallTermSaveGuard { key }
}

struct Exclusion {
    key: PathBuf,
}

impl Drop for Exclusion {
    fn drop(&mut self) {
        if let Some(state) = state().as_mut() {
            let depth = state.excluded.get(&self.key).copied().unwrap_or(1) - 1;
            if depth == 0 {
                state.excluded.remove(&self.key);
            } else {
                state.excluded.insert(self.key.clone(), depth);
            }
        }
    }
}

/// Runs `work` with every term-store save of this process kept out of `dir`.
pub fn with_recall_term_saves_excluded<T>(dir: &Path, work: impl FnOnce() -> T) -> T {
    let key = key(dir);
    let mut guard = state();
    *guard
        .get_or_insert_with(GateState::default)
        .excluded
        .entry(key.clone())
        .or_insert(0) += 1;
    let _exclusion = Exclusion { key: key.clone() };
    while guard
        .as_ref()
        .is_some_and(|state| state.writing.contains_key(&key))
    {
        guard = SETTLED.wait(guard).unwrap_or_else(PoisonError::into_inner);
    }
    drop(guard);
    work()
}

#[cfg(test)]
#[path = "bm25_persist_gate_tests.rs"]
mod tests;
