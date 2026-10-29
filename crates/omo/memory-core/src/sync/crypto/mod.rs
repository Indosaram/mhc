//! Authenticated encrypted sync, with locally minted trust and admission capabilities.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used)]
pub mod anchor;
pub mod build;
pub mod chain;
pub mod chain_checks;
pub mod entry;
pub mod envelope;
pub mod fold;
pub mod links;
pub mod open;
pub mod parse;
pub mod receiver;
pub mod rejection;
pub mod rules;
pub mod signatures;
pub mod snapshot;
pub mod strict_ed25519;
pub mod trust_pin;
pub mod unwrap;
pub mod vendor;

#[cfg(test)]
mod batch_tests;
#[cfg(test)]
mod chain_tests;
#[cfg(test)]
mod snapshot_tests;
#[cfg(test)]
mod testing;
#[cfg(test)]
mod trust_pin_tests;
#[cfg(test)]
mod unwrap_tests;

pub use anchor::*;
pub use build::*;
pub use chain::*;
pub use entry::*;
pub use fold::{Authority, is_cancelled_admission, is_vetoed_epoch};
pub use links::next_link;
pub use open::*;
pub use rejection::*;
pub use snapshot::*;
pub use trust_pin::*;
pub use unwrap::*;
