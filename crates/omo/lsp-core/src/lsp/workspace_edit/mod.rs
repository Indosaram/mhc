//! Port of the TS `workspace-edit-*` family: plan (parse, snapshot, simulate) then commit.

pub mod commit;
pub mod contract_evidence;
pub mod fingerprint;
pub mod parser;
pub mod path;
pub mod plan;
pub mod simulation;
pub mod snapshot;
pub mod text;
pub mod types;

pub use commit::*;
pub use contract_evidence::*;
pub use plan::*;
pub use types::*;

#[cfg(test)]
mod tests;
