//! Port of senpi `packages/tui/src/stderr-observer.ts`.

use std::sync::Arc;

use crate::process_stdio::{StreamWrite, set_stderr_writer, stderr_writer};

/// Observes stderr writes without taking ownership of any later writer installed above it.
/// Returns the uninstall function.
pub fn observe_process_stderr_writes(
    listener: Arc<dyn Fn() + Send + Sync>,
) -> Box<dyn FnOnce() + Send> {
    let original = stderr_writer();
    let inner = Arc::clone(&original);
    let observed: StreamWrite = Arc::new(move |text: &str| {
        listener();
        inner(text);
    });
    set_stderr_writer(Arc::clone(&observed));
    Box::new(move || {
        let current = stderr_writer();
        if Arc::ptr_eq(&current, &observed) {
            set_stderr_writer(original);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_stdio::stderr_write;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn observes_writes_and_keeps_later_writers() {
        let _lock = crate::process_stdio::TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let sink = Arc::new(Mutex::new(Vec::<String>::new()));
        let base_sink = Arc::clone(&sink);
        let base = set_stderr_writer(Arc::new(move |t: &str| {
            base_sink.lock().unwrap().push(t.to_string())
        }));
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        let stop = observe_process_stderr_writes(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        stderr_write("a");
        assert_eq!(seen.load(Ordering::SeqCst), 1);
        assert_eq!(*sink.lock().unwrap(), vec!["a".to_string()]);
        let later: StreamWrite = Arc::new(|_t: &str| {});
        let observed = set_stderr_writer(Arc::clone(&later));
        stop();
        assert!(
            Arc::ptr_eq(&stderr_writer(), &later),
            "a later writer is not replaced"
        );
        set_stderr_writer(observed);
        set_stderr_writer(base);
    }
}
