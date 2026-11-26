//! Port of senpi `auth/pool/failover.ts` at v2026.11.19-5, where this module exports only
//! the shared retry prefix and block-duration constants.

pub use crate::utils::provider_failure_description::TURN_RETRY_SUPPRESSION_PREFIX;
pub const DEFAULT_SLOT_BLOCK_MS: f64 = 60_000.0;
pub const MAX_SLOT_BLOCK_MS: f64 = 48.0 * 60.0 * 60.0 * 1_000.0;
