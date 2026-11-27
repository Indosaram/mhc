//! Child retry fallback profile builder (`runners/retry-fallback-profile.ts`).

use crate::runners::designated_fallback_profile::{
    HostRetryFallbackProfile, designated_fallback_profile,
};
use crate::runners::types::RpcRunnerSpec;

/// The child's designated chain, in the shape senpi applies as a session-only settings overlay.
/// Every child with a model gets one, a one-model child included: without it the engine would fall
/// back through the user's settings and senpi's shipped chains. `None` only when no model is pinned.
#[must_use]
pub fn child_retry_fallback_profile(spec: &RpcRunnerSpec) -> Option<HostRetryFallbackProfile> {
    let model = spec.model.as_deref()?;
    let fallbacks = spec.fallback_models.as_deref().unwrap_or(&[]);
    Some(designated_fallback_profile(model, fallbacks))
}
