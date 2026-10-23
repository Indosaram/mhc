//! Port of senpi packages/coding-agent/src/core/question-input.ts.
//!
//! Engine-owned capability, not a user-asserted origin on the extension message API. The registry is
//! keyed by extension-context identity (the address of the shared context allocation).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

type Release = Arc<dyn Fn() + Send + Sync>;

fn releases() -> &'static Mutex<HashMap<usize, Release>> {
    static RELEASES: OnceLock<Mutex<HashMap<usize, Release>>> = OnceLock::new();
    RELEASES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn identity<T>(ctx: &Arc<T>) -> usize {
    Arc::as_ptr(ctx) as *const () as usize
}

pub fn bind_question_input<T>(ctx: &Arc<T>, release: impl Fn() + Send + Sync + 'static) {
    if let Ok(mut map) = releases().lock() {
        map.insert(identity(ctx), Arc::new(release));
    }
}

pub fn release_question_input<T>(ctx: &Arc<T>) {
    let release = releases()
        .lock()
        .ok()
        .and_then(|map| map.get(&identity(ctx)).cloned());
    if let Some(release) = release {
        release();
    }
}

/// Drop the binding when the context is disposed so the identity key cannot be reused stale.
pub fn unbind_question_input<T>(ctx: &Arc<T>) {
    if let Ok(mut map) = releases().lock() {
        map.remove(&identity(ctx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn release_calls_bound_release_only_for_that_context() {
        let hits = Arc::new(AtomicUsize::new(0));
        let bound = Arc::new(1u8);
        let other = Arc::new(2u8);
        let counter = Arc::clone(&hits);
        bind_question_input(&bound, move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        release_question_input(&other);
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        release_question_input(&bound);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        unbind_question_input(&bound);
        release_question_input(&bound);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
