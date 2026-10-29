//! Host-runtime (senpi) value adapters (`senpi/` in TypeScript).
//!
//! `minimal-resource-loader.ts` builds a host `ResourceLoader` object; the host owns that shape, so
//! it lives behind the [`crate::host`] seam rather than here (see parity.md).

mod thinking_level;

pub use thinking_level::{SenpiThinkingLevel, as_senpi_thinking_level};

#[cfg(test)]
#[path = "senpi_tests.rs"]
mod tests;
