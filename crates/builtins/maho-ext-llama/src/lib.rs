//! Port of senpi `packages/coding-agent/src/extensions/llama/` at pin
//! `fe8c564bf33a2cbbdbbba99c9bd8b45b21e37407`: the hidden `llama.cpp` builtin.

pub mod client;
pub mod huggingface;
pub mod index;
pub mod provider;

pub use index::{LlamaExtension, llama};
