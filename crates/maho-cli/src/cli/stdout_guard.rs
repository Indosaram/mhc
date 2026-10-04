//! Port of the stdout takeover in senpi `core/output-guard.ts`.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

static TAKEN_OVER: AtomicBool = AtomicBool::new(false);

pub fn take_over_stdout() {
    TAKEN_OVER.store(true, Ordering::SeqCst);
}

pub fn is_taken_over() -> bool {
    TAKEN_OVER.load(Ordering::SeqCst)
}

pub fn write_line(text: &str) {
    let result = if is_taken_over() {
        std::io::stderr().lock().write_all(text.as_bytes())
    } else {
        std::io::stdout().lock().write_all(text.as_bytes())
    };
    if let Err(error) = result {
        eprintln!("{error}");
    }
}
