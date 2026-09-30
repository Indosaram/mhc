//! Port of senpi packages/coding-agent/src/core/output-guard.ts.
//!
//! deviation: Rust has no process-global `process.stdout.write` to reassign, so this module owns
//! the process-wide sink maho's own writers use (maho_write_stdout / maho_write_stderr) and applies
//! the same takeover rules to it.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub const RAW_STDOUT_RETRY_DELAY_MS: u64 = 10;

type HiddenDiagnostic = Arc<dyn Fn(&str) + Send + Sync>;
type WriteSink = Arc<dyn Fn(&str) + Send + Sync>;
type Observer = Arc<dyn Fn() + Send + Sync>;
type ObserverList = Arc<Mutex<Vec<Observer>>>;
type DiagnosticFallback = Arc<dyn Fn(&str) -> String + Send + Sync>;

struct StdoutTakeover {
    raw_stdout_write: WriteSink,
    raw_stderr_write: WriteSink,
}

struct StderrTakeover {
    original_stderr_write: WriteSink,
    on_hidden_diagnostic: Option<HiddenDiagnostic>,
    format_hidden_diagnostic_fallback: Option<DiagnosticFallback>,
}

struct VisibleStderrObservation {
    original: WriteSink,
    writer: WriteSink,
    listeners: ObserverList,
}

fn default_stdout_write() -> WriteSink {
    Arc::new(|text: &str| {
        let mut out = std::io::stdout();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    })
}

fn default_stderr_write() -> WriteSink {
    Arc::new(|text: &str| {
        let mut err = std::io::stderr();
        let _ = err.write_all(text.as_bytes());
        let _ = err.flush();
    })
}

struct OutputGuardState {
    stdout_takeover: Option<StdoutTakeover>,
    stderr_takeover: Option<StderrTakeover>,
    visible_stderr: Option<VisibleStderrObservation>,
}

fn state() -> &'static Mutex<OutputGuardState> {
    static STATE: OnceLock<Mutex<OutputGuardState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(OutputGuardState {
            stdout_takeover: None,
            stderr_takeover: None,
            visible_stderr: None,
        })
    })
}

fn raw_stdout_write() -> WriteSink {
    let guard = state().lock().expect("output guard lock");
    match &guard.stdout_takeover {
        Some(takeover) => Arc::clone(&takeover.raw_stdout_write),
        None => default_stdout_write(),
    }
}

/// Redirect every stdout write to the raw stderr sink (senpi's `takeOverStdout`).
pub fn take_over_stdout() {
    let mut guard = state().lock().expect("output guard lock");
    if guard.stdout_takeover.is_some() {
        return;
    }
    guard.stdout_takeover = Some(StdoutTakeover { raw_stdout_write: default_stdout_write(), raw_stderr_write: default_stderr_write() });
}

pub fn restore_stdout() {
    let mut guard = state().lock().expect("output guard lock");
    guard.stdout_takeover = None;
}

pub fn is_stdout_taken_over() -> bool {
    state().lock().expect("output guard lock").stdout_takeover.is_some()
}

/// Write one chunk to the process stdout sink, honouring an active stdout takeover.
pub fn maho_write_stdout(text: &str) {
    if text.is_empty() {
        return;
    }
    let (sink, taken_over) = {
        let guard = state().lock().expect("output guard lock");
        match &guard.stdout_takeover {
            Some(takeover) => (Arc::clone(&takeover.raw_stderr_write), true),
            None => (default_stdout_write(), false),
        }
    };
    let _ = taken_over;
    sink(text);
}

fn visible_stderr_sink() -> WriteSink {
    let mut guard = state().lock().expect("output guard lock");
    if let Some(observation) = &guard.visible_stderr {
        return Arc::clone(&observation.writer);
    }
    let original = match &guard.stderr_takeover {
        Some(takeover) => Arc::clone(&takeover.original_stderr_write),
        None => default_stderr_write(),
    };
    let listeners: ObserverList = Arc::new(Mutex::new(Vec::new()));
    let writer: WriteSink = {
        let original = Arc::clone(&original);
        let listeners = Arc::clone(&listeners);
        Arc::new(move |text: &str| {
            let notified = listeners.lock().map(|listeners| listeners.clone()).unwrap_or_default();
            for notify in notified {
                notify();
            }
            original(text);
        })
    };
    guard.visible_stderr = Some(VisibleStderrObservation { original, writer: Arc::clone(&writer), listeners });
    writer
}

/// Observe the visible stderr sink; the returned handle unsubscribes when dropped.
pub fn observe_visible_stderr_writes(listener: Observer) -> StderrObservation {
    visible_stderr_sink();
    let listeners = {
        let guard = state().lock().expect("output guard lock");
        guard.visible_stderr.as_ref().map(|observation| Arc::clone(&observation.listeners))
    };
    if let Some(listeners) = listeners
        && let Ok(mut listeners) = listeners.lock() {
            listeners.push(Arc::clone(&listener));
        }
    StderrObservation { listener }
}

pub struct StderrObservation {
    listener: Observer,
}

impl Drop for StderrObservation {
    fn drop(&mut self) {
        let (remaining, original, writer) = {
            let guard = state().lock().expect("output guard lock");
            let Some(observation) = &guard.visible_stderr else { return };
            let mut remaining = 0usize;
            if let Ok(mut listeners) = observation.listeners.lock() {
                listeners.retain(|candidate| !Arc::ptr_eq(candidate, &self.listener));
                remaining = listeners.len();
            }
            (remaining, Arc::clone(&observation.original), Arc::clone(&observation.writer))
        };
        if remaining > 0 {
            return;
        }
        let mut guard = state().lock().expect("output guard lock");
        if let Some(takeover) = guard.stderr_takeover.as_mut()
            && Arc::ptr_eq(&takeover.original_stderr_write, &writer) {
                takeover.original_stderr_write = original;
            }
        guard.visible_stderr = None;
    }
}

pub fn take_over_stderr(on_hidden_diagnostic: Option<HiddenDiagnostic>, format_hidden_diagnostic_fallback: Option<DiagnosticFallback>) {
    let mut guard = state().lock().expect("output guard lock");
    if guard.stderr_takeover.is_some() {
        return;
    }
    guard.stderr_takeover = Some(StderrTakeover {
        original_stderr_write: default_stderr_write(),
        on_hidden_diagnostic,
        format_hidden_diagnostic_fallback,
    });
}

pub fn restore_stderr() {
    let mut guard = state().lock().expect("output guard lock");
    guard.stderr_takeover = None;
}

/// Route one stderr chunk through the diagnostic redirect; a failing handler falls back to the
/// visible sink. Returns false when the hidden handler failed (the TS write callback error).
pub fn maho_write_stderr(text: &str) -> bool {
    let handler = {
        let guard = state().lock().expect("output guard lock");
        guard.stderr_takeover.as_ref().map(|takeover| (takeover.on_hidden_diagnostic.clone(), takeover.format_hidden_diagnostic_fallback.clone()))
    };
    let Some((handler, fallback)) = handler else {
        default_stderr_write()(text);
        return true;
    };
    let Some(handler) = handler else {
        return true;
    };
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(text))).is_err() {
        let fallback_text = fallback.map(|fallback| fallback(text)).unwrap_or_else(|| text.to_owned());
        if !fallback_text.is_empty() {
            default_stderr_write()(&fallback_text);
        }
        return false;
    }
    true
}

static RAW_STDOUT_PENDING: AtomicBool = AtomicBool::new(false);

/// Append text to the raw stdout queue (senpi's `writeRawStdout`).
pub fn write_raw_stdout(text: &str) {
    if text.is_empty() {
        return;
    }
    let sink = raw_stdout_write();
    let pending = RAW_STDOUT_PENDING.load(Ordering::SeqCst);
    RAW_STDOUT_PENDING.store(true, Ordering::SeqCst);
    let _ = pending;
    sink(text);
    RAW_STDOUT_PENDING.store(false, Ordering::SeqCst);
}

fn raw_stdout_tail() -> &'static tokio::sync::Mutex<()> {
    static TAIL: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    TAIL.get_or_init(|| tokio::sync::Mutex::new(()))
}

pub async fn write_raw_stdout_chunk(text: &str) {
    let _held = raw_stdout_tail().lock().await;
    write_raw_stdout(text);
}

pub async fn wait_for_raw_stdout_backpressure() {
    let _held = raw_stdout_tail().lock().await;
}

pub async fn flush_raw_stdout() {
    write_raw_stdout_chunk("").await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset() {
        let mut guard = state().lock().expect("lock");
        guard.stdout_takeover = None;
        guard.stderr_takeover = None;
        guard.visible_stderr = None;
    }

    #[test]
    fn stdout_takeover_is_idempotent_and_restorable() {
        reset();
        assert!(!is_stdout_taken_over());
        take_over_stdout();
        take_over_stdout();
        assert!(is_stdout_taken_over());
        restore_stdout();
        assert!(!is_stdout_taken_over());
        restore_stdout();
        assert!(!is_stdout_taken_over());
    }

    #[test]
    fn hidden_diagnostics_are_redirected_and_failures_fall_back() {
        reset();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let capture = Arc::clone(&seen);
        take_over_stderr(Some(Arc::new(move |text| capture.lock().expect("lock").push(text.to_owned()))), None);
        assert!(maho_write_stderr("diagnostic"));
        assert_eq!(seen.lock().expect("lock").as_slice(), &["diagnostic".to_owned()]);
        restore_stderr();
        reset();
        take_over_stderr(Some(Arc::new(|_| panic!("handler boom"))), Some(Arc::new(|text| format!("fallback: {text}"))));
        assert!(!maho_write_stderr("hidden"));
        restore_stderr();
        reset();
        take_over_stderr(Some(Arc::new(|_| panic!("handler boom"))), None);
        assert!(!maho_write_stderr("hidden"));
        restore_stderr();
        reset();
    }

    #[test]
    fn visible_stderr_observers_are_notified_and_detached_on_drop() {
        reset();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observer = {
            let count = Arc::clone(&count);
            observe_visible_stderr_writes(Arc::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            }))
        };
        let sink = visible_stderr_sink();
        sink("x");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        drop(observer);
        reset();
    }

    #[tokio::test]
    async fn raw_stdout_writes_and_flushes() {
        reset();
        write_raw_stdout("a");
        write_raw_stdout_chunk("b").await;
        flush_raw_stdout().await;
        wait_for_raw_stdout_backpressure().await;
        reset();
    }
}
