//! Slot-blocking constants owned by senpi packages/ai/src/auth/pool/failover.ts.
//!
//! deviation: maho-ai's auth/pool/failover is still the todo-13 stub, so the two constants this
//! crate consumes are declared here with senpi's values until that module lands.

/// DEFAULT_SLOT_BLOCK_MS: first-failure cooldown for a rate-limited credential.
pub const DEFAULT_SLOT_BLOCK_MS: u64 = 60_000;
/// MAX_SLOT_BLOCK_MS: the 48-hour cap every cooldown is clamped to.
pub const MAX_SLOT_BLOCK_MS: u64 = 48 * 60 * 60 * 1000;
/// PROVIDER_NOT_CONFIGURED_PREFIX (packages/ai/src/auth/resolve.ts).
pub const PROVIDER_NOT_CONFIGURED_PREFIX: &str = "Provider is not configured: ";
