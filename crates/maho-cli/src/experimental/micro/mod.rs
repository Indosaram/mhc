//! Port of senpi `experimental/micro` (pin 903b8336): a small local agent over the Pico3 harness.
//! `main.ts` and `runtime.ts` are requeued to senpi-interactive; `tui.ts` waits on them.
pub mod api;
pub mod models;
pub mod sessions;
pub mod tools;
