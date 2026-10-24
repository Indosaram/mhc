//! Release-profile panic and backtrace fixture binary.
//!
//! Panics with an explicit message so QA can assert:
//! 1. Panic location `panic_probe.rs:<line>` is preserved (via `debug = "line-tables-only"`).
//! 2. Backtrace frames contain function names like `probe_panic` and `main` (via symbol retention with `strip = "debuginfo"`).

#[inline(never)]
fn probe_panic() {
    panic!("probe panic for release profile backtrace verification");
}

fn main() {
    probe_panic();
}
