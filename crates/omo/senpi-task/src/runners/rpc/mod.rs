//! RPC child-process runner pieces (`runners/rpc/`).

pub mod errors;
pub mod exit_mapping;
pub mod handle;
pub mod model_admission;
pub mod parent_extensions;
pub mod process;
pub mod protocol_client;
pub mod spawn;
pub mod terminate;
pub mod turn_outcome;
pub mod ui_auto_answer;

#[cfg(test)]
mod rpc_tests;
