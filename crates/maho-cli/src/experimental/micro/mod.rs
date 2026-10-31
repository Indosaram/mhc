//! Port of senpi `experimental/micro` (pin 903b8336): a small local agent over the Pico3 harness.
//! `tui.ts` (the screen host) is not ported yet; `main.rs` drives the runtime without it.
pub mod api;
pub mod lock_gated_storage;
pub mod main;
pub mod models;
pub mod runtime;
pub mod sessions;
pub mod tools;
