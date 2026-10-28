//! Run pause and admission primitives (`pause/` in TypeScript).

pub mod admission_barrier;
pub mod guidance;
pub mod native_child_pause;
pub mod ports;
pub mod registry;
pub mod run_controller;

#[cfg(test)]
mod parity_tests;
