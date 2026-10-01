//! Integer millisecond views of the shared auth engine's cooldown constants.

/// DEFAULT_SLOT_BLOCK_MS: first-failure cooldown for a rate-limited credential.
pub const DEFAULT_SLOT_BLOCK_MS: u64 = maho_ai::auth::pool::failover::DEFAULT_SLOT_BLOCK_MS as u64;
/// MAX_SLOT_BLOCK_MS: the 48-hour cap every cooldown is clamped to.
pub const MAX_SLOT_BLOCK_MS: u64 = maho_ai::auth::pool::failover::MAX_SLOT_BLOCK_MS as u64;
/// PROVIDER_NOT_CONFIGURED_PREFIX (packages/ai/src/auth/resolve.ts).
pub use maho_ai::auth::resolve::PROVIDER_NOT_CONFIGURED_PREFIX;
